//! BuiltinExperimentRunner — executes holdout/A-B experiments locally.
//!
//! Wraps `proxy/holdout.rs` behind the OCLA trait. Experiments are identified
//! by deterministic refs. Results carry an outcome ref for correlation with
//! the OutcomeTracker and an optional rollback ref for reverting the cohort.

use crate::core::ocla::traits::{ExperimentRunner, OclaService};
use crate::core::ocla::types::{
    ExperimentOutcome, ExperimentRequest, ExperimentResult, ExperimentStopConditions,
    OclaCapability, OclaCapabilityKind, OclaResult,
};

/// Determines whether a request belongs to the holdout (control) group.
fn is_holdout(seed: &str, request_ref: &str, holdout_pct: u8) -> bool {
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed.as_bytes());
    hasher.update(request_ref.as_bytes());
    let hash = hasher.finalize();
    let bucket = u64::from_le_bytes(
        hash.as_bytes()[..8]
            .try_into()
            .expect("blake3 hash is at least 8 bytes"),
    ) % 100;
    bucket < u64::from(holdout_pct)
}

/// Tracks experiment state for stop-condition evaluation.
struct ExperimentState {
    samples: u64,
    started_at: std::time::Instant,
    treatment_sum: f64,
    control_sum: f64,
    treatment_count: u64,
    control_count: u64,
}

impl ExperimentState {
    fn new() -> Self {
        Self {
            samples: 0,
            started_at: std::time::Instant::now(),
            treatment_sum: 0.0,
            control_sum: 0.0,
            treatment_count: 0,
            control_count: 0,
        }
    }

    fn should_stop(&self, conditions: &ExperimentStopConditions) -> Option<String> {
        if conditions
            .max_samples
            .is_some_and(|max| self.samples >= max)
        {
            return Some("max_samples".into());
        }
        if conditions
            .max_duration_secs
            .is_some_and(|max| self.started_at.elapsed().as_secs() >= max)
        {
            return Some("max_duration_secs".into());
        }
        if let Some(min_improvement_pct) = conditions.min_improvement_pct {
            let outcome = self.outcome("");
            if self.control_count > 0
                && self.treatment_count > 0
                && outcome.improvement_pct < f64::from(min_improvement_pct)
            {
                return Some("min_improvement_pct".into());
            }
        }
        None
    }

    fn record_sample(&mut self, is_holdout: bool, metric: f64) {
        self.samples += 1;
        if is_holdout {
            self.control_sum += metric;
            self.control_count += 1;
        } else {
            self.treatment_sum += metric;
            self.treatment_count += 1;
        }
    }

    fn outcome(&self, experiment_ref: &str) -> ExperimentOutcome {
        let treatment_metric = average(self.treatment_sum, self.treatment_count);
        let control_metric = average(self.control_sum, self.control_count);
        let improvement_pct = if self.control_count == 0 || control_metric == 0.0 {
            0.0
        } else {
            (treatment_metric - control_metric) / control_metric * 100.0
        };
        ExperimentOutcome {
            experiment_ref: experiment_ref.into(),
            treatment_samples: self.treatment_count,
            control_samples: self.control_count,
            treatment_metric,
            control_metric,
            improvement_pct,
            stopped_reason: None,
            is_significant: self.treatment_count > 0
                && self.control_count > 0
                && treatment_metric != control_metric,
        }
    }
}

fn average(sum: f64, count: u64) -> f64 {
    if count == 0 { 0.0 } else { sum / count as f64 }
}

pub struct BuiltinExperimentRunner;

impl BuiltinExperimentRunner {
    pub fn new() -> Self {
        Self
    }

    /// Computes an experiment outcome from deterministic holdout assignments.
    pub fn compute_outcome(
        &self,
        request: &ExperimentRequest,
        metric_fn: impl Fn(&str) -> f64,
    ) -> OclaResult<ExperimentOutcome> {
        let holdout_samples = request
            .holdout
            .as_ref()
            .and_then(|holdout| holdout.max_samples);
        let stop_samples = request
            .stop_conditions
            .as_ref()
            .and_then(|conditions| conditions.max_samples);
        let sample_count = match (holdout_samples, stop_samples) {
            (Some(holdout), Some(stop)) => holdout.min(stop),
            (Some(samples), None) | (None, Some(samples)) => samples,
            (None, None) => 1,
        };
        let mut state = ExperimentState::new();
        let mut stopped_reason = None;

        for sample in 0..sample_count {
            let request_ref = format!("{}:{sample}", request.context.request_id);
            let is_holdout = request.holdout.as_ref().is_some_and(|holdout| {
                is_holdout(&holdout.assignment_seed, &request_ref, holdout.holdout_pct)
            });
            state.record_sample(is_holdout, metric_fn(&request_ref));
            if let Some(conditions) = request.stop_conditions.as_ref()
                && let Some(reason) = state.should_stop(conditions)
            {
                stopped_reason = Some(reason);
                break;
            }
        }

        let mut outcome = state.outcome(&request.experiment_ref);
        outcome.stopped_reason = stopped_reason;
        Ok(outcome)
    }
}

impl Default for BuiltinExperimentRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinExperimentRunner {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ExperimentRunner)
    }
}

impl ExperimentRunner for BuiltinExperimentRunner {
    fn run_experiment(&self, request: ExperimentRequest) -> OclaResult<ExperimentResult> {
        let config = crate::core::config::Config::load();
        let requested_model = config
            .proxy
            .baseline
            .reference_model
            .as_deref()
            .ok_or_else(|| {
                crate::core::ocla::types::OclaError::Rejected(
                    OclaCapabilityKind::ExperimentRunner,
                    "no reference model configured for routing evaluation".into(),
                )
            })?;
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();

        crate::core::eval_ab::routing_eval::run_routing_experiment(
            &request,
            requested_model,
            &config.proxy.routing,
            &pricing,
        )
        .map_err(|error| {
            crate::core::ocla::types::OclaError::Rejected(
                OclaCapabilityKind::ExperimentRunner,
                error.to_string(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BuiltinExperimentRunner, ExperimentState, is_holdout};
    use crate::core::ocla::traits::ExperimentRunner;
    use crate::core::ocla::types::{
        ExperimentRequest, ExperimentStopConditions, HoldoutConfig, OclaRequestContext,
    };

    fn experiment(name: &str) -> ExperimentRequest {
        ExperimentRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
                trace_id: "tr-unit".into(),
            },
            experiment_ref: name.into(),
            cohort_ref: "cohort:control".into(),
            holdout: None,
            stop_conditions: None,
        }
    }

    #[test]
    fn holdout_assignment_is_deterministic() {
        assert_eq!(
            is_holdout("seed", "request-1", 50),
            is_holdout("seed", "request-1", 50)
        );
    }

    #[test]
    fn distinct_seeds_produce_distinct_assignments() {
        assert!((0..100).any(|index| {
            let request_ref = format!("request-{index}");
            is_holdout("seed-a", &request_ref, 50) != is_holdout("seed-b", &request_ref, 50)
        }));
    }

    #[test]
    fn max_samples_stops_experiment() {
        let mut state = ExperimentState::new();
        state.record_sample(false, 1.0);
        let conditions = ExperimentStopConditions {
            max_samples: Some(1),
            min_improvement_pct: None,
            max_duration_secs: None,
        };
        assert_eq!(
            state.should_stop(&conditions).as_deref(),
            Some("max_samples")
        );
    }

    #[test]
    fn empty_stop_conditions_never_stop() {
        let mut state = ExperimentState::new();
        state.record_sample(false, 1.0);
        let conditions = ExperimentStopConditions {
            max_samples: None,
            min_improvement_pct: None,
            max_duration_secs: None,
        };
        assert_eq!(state.should_stop(&conditions), None);
    }

    #[test]
    fn compute_outcome_returns_holdout_metrics() {
        let runner = BuiltinExperimentRunner::new();
        let mut request = experiment("exp-outcome");
        request.holdout = Some(HoldoutConfig {
            holdout_pct: 50,
            assignment_seed: "seed".into(),
            max_samples: Some(100),
        });
        let outcome = runner.compute_outcome(&request, |_| 10.0).unwrap();

        assert_eq!(outcome.treatment_samples + outcome.control_samples, 100);
        assert_eq!(outcome.treatment_metric, 10.0);
        assert_eq!(outcome.control_metric, 10.0);
        assert_eq!(outcome.improvement_pct, 0.0);
    }

    #[test]
    fn rejects_missing_suite_instead_of_fabricating_result() {
        let runner = BuiltinExperimentRunner::new();
        let error = runner.run_experiment(experiment("/definitely/missing-suite.ndjson"));
        assert!(error.is_err());
    }

    #[test]
    fn invalid_request_never_returns_synthetic_refs() {
        let runner = BuiltinExperimentRunner::new();
        let result = runner.run_experiment(experiment("exp-b"));
        assert!(result.is_err());
    }

    #[test]
    fn registry_builtins_route_experiment_requests_to_runner() {
        let registry = crate::core::ocla::registry::OclaRegistry::with_builtins();
        let result = registry
            .experiment_runner
            .run_experiment(experiment("/definitely/missing-suite.ndjson"));
        assert!(result.is_err());
    }
}
