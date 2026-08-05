//! Thin content-deduplication bridge for the ctx_read hot path.

use super::dedup_wiring::{self, DedupAction};

/// Read-specific snapshot of content-deduplication statistics.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReadDedupSummary {
    /// Number of reads checked for duplicate content.
    pub total_reads: usize,
    /// Number of reads replaced with unchanged-content stubs.
    pub dedup_hits: usize,
    /// Estimated tokens avoided by deduplicated reads.
    pub tokens_saved: usize,
    /// Fraction of checked reads replaced with stubs.
    pub hit_rate: f64,
}

/// Returns whether content deduplication should run for a ctx_read mode.
#[must_use]
pub(crate) fn should_dedup(mode: &str) -> bool {
    super::kernel_config::is_enabled()
        && super::kernel_config::is_feature_enabled("content_dedup")
        && mode != "raw"
}

/// Returns an unchanged-content stub when a prior delivery matches.
///
/// Returns `None` (deliver full content) when the kernel master switch is
/// disabled, when the `content_dedup` feature is off, or when the content
/// has not been seen before.
#[must_use]
pub(crate) fn try_dedup(path: &str, content: &str, fresh: bool) -> Option<String> {
    if fresh {
        // When fresh=true, invalidate the dedup fingerprint so the re-read is delivered.
        on_file_write(path);
        return None;
    }
    if !super::kernel_config::is_enabled() {
        return None;
    }
    match dedup_wiring::check_content(path, content, false) {
        DedupAction::DeliverStub { stub } => Some(stub),
        DedupAction::DeliverFull | DedupAction::DeliverModified => None,
    }
}
/// Invalidates deduplication state after a file write.
pub(crate) fn on_file_write(path: &str) {
    // Wired from ctx_edit + ctx_patch post-write paths.
    dedup_wiring::invalidate(path);
}

/// Returns read-specific cumulative content-deduplication statistics.
#[must_use]
pub(crate) fn dedup_summary() -> ReadDedupSummary {
    let stats = dedup_wiring::dedup_stats();
    ReadDedupSummary {
        total_reads: stats.total_checks,
        dedup_hits: stats.cache_hits,
        tokens_saved: stats.tokens_saved,
        hit_rate: stats.hit_rate,
    }
}

/// Clears content-deduplication state and statistics.
pub(crate) fn reset() {
    dedup_wiring::reset_dedup();
}

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::{dedup_summary, on_file_write, reset, should_dedup, try_dedup};
    use crate::core::context_kernel::kernel_config::{
        KERNEL_TEST_LOCK, KernelFeatures, reset_features, update_features,
    };

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_features();
        crate::core::context_kernel::dedup_wiring::reset_dedup();
        reset();
        guard
    }

    #[test]
    fn should_dedup_respects_config() {
        let _guard = isolated();
        let features = KernelFeatures {
            enabled: false,
            ..KernelFeatures::default()
        };
        update_features(features);
        assert!(!should_dedup("full"));
    }

    #[test]
    fn should_dedup_raw_mode_false() {
        let _guard = isolated();
        assert!(!should_dedup("raw"));
        assert!(should_dedup("full"));
    }

    #[test]
    fn try_dedup_new_file_none() {
        let _guard = isolated();
        assert_eq!(try_dedup("new.rs", "content", false), None);
    }

    #[test]
    fn try_dedup_repeated_file_some() {
        let _guard = isolated();
        assert_eq!(try_dedup("repeat.rs", "content", false), None);
        let stub = try_dedup("repeat.rs", "content", false);
        assert!(stub.is_some());
        assert!(stub.is_some_and(|value| value.contains("repeat.rs unchanged")));
    }

    #[test]
    fn on_write_invalidates() {
        let _guard = isolated();
        assert_eq!(try_dedup("written.rs", "content", false), None);
        assert!(try_dedup("written.rs", "content", false).is_some());
        on_file_write("written.rs");
        assert_eq!(try_dedup("written.rs", "content", false), None);
    }

    #[test]
    fn summary_tracks_reads() {
        let _guard = isolated();
        assert_eq!(try_dedup("a.rs", "one", false), None);
        assert!(try_dedup("a.rs", "one", false).is_some());
        assert!(try_dedup("a.rs", "one", false).is_some());
        assert_eq!(try_dedup("b.rs", "two", false), None);
        assert!(try_dedup("b.rs", "two", false).is_some());

        let summary = dedup_summary();
        assert_eq!(summary.total_reads, 5);
        assert_eq!(summary.dedup_hits, 3);
        assert!((summary.hit_rate - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn fresh_bypasses_dedup() {
        let _guard = isolated();
        assert_eq!(try_dedup("src/lib.rs", "fn main() {}", false), None);
        assert!(try_dedup("src/lib.rs", "fn main() {}", false).is_some());
        assert_eq!(try_dedup("src/lib.rs", "fn main() {}", true), None);
    }
}
