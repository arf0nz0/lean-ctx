//! Entitlement-aware upgrade hints (#346).
//!
//! When a *hosted* capability is gated, the client should explain — honestly and
//! consistently — what unlocks it, that the local engine stays free, and the
//! exact command to run. The hint is tailored to the gated capability and to the
//! **cheapest** plan that unlocks it (via
//! [`min_plan_for`](crate::core::billing::min_plan_for)), never implying that a
//! local feature must be paid for.

use crate::cloud_client;
use crate::core::billing::{Plan, min_plan_for};

/// Human-facing name for a gated *hosted* capability. Unknown keys fall back to
/// the raw feature id, hence the [`Cow`](std::borrow::Cow).
fn capability_label(feature: &str) -> std::borrow::Cow<'static, str> {
    match feature {
        "cloud_sync" => "Cloud sync (Personal Cloud)".into(),
        "private_registry" => "Private extension/persona registry".into(),
        "sso_oidc" => "Org SSO (OIDC)".into(),
        "sso_scim" => "SAML SSO + SCIM provisioning".into(),
        "hosted_index" => "Hosted retrieval index".into(),
        "managed_connectors" => "Managed connectors".into(),
        "audit_retention" => "Audit-log retention".into(),
        "revenue_share" => "Marketplace revenue share".into(),
        other => other.to_string().into(),
    }
}

/// Capitalised plan name for prose (the wire ids are lower-case).
fn plan_display(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "Free",
        Plan::Supporter => "Supporter",
        Plan::Pro => "Pro",
        Plan::Team => "Team",
        Plan::Enterprise => "Enterprise",
    }
}

/// The exact command (or pointer) that unlocks `min`. Pro/Team are
/// self-serve via hosted Stripe Checkout; Enterprise is sales-assisted.
fn upgrade_command(min: Plan) -> &'static str {
    match min {
        Plan::Team => "lean-ctx cloud upgrade --plan team",
        Plan::Enterprise => "Enterprise plan — see https://leanctx.com/pricing",
        _ => "lean-ctx cloud upgrade --plan pro",
    }
}

/// Build the hint text for `feature` given the user's `current` effective plan,
/// or `None` if `feature` is not gated (local/unknown). Pure and testable: no
/// I/O, no global state.
fn render_hint(feature: &str, current: Plan) -> Option<String> {
    let min = min_plan_for(feature)?;
    let label = capability_label(feature);
    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "{label} is a lean-ctx {} feature.\n",
        plan_display(min)
    ));
    out.push_str("Everything local keeps working — this only adds a hosted capability.\n");
    if current != Plan::Free && !crate::core::billing::entitlement_allows(current, feature) {
        out.push_str(&format!("You're on {}.\n", plan_display(current)));
    }
    out.push_str(&format!("Unlock it:  {}\n", upgrade_command(min)));
    Some(out)
}

/// Print an entitlement-aware upgrade hint for a gated hosted `feature`.
///
/// Resolves the current plan from the offline-grace cache (#345) so the message
/// reflects what the user actually has. No-op for local/unknown features. Call
/// this at a *definitive* gate (e.g. after the server returns HTTP 402): it
/// always prints, because the server has already decided the user is not
/// entitled.
pub(crate) fn hint_for(feature: &str) {
    let eff = cloud_client::resolve_effective_plan_cached();
    if let Some(text) = render_hint(feature, eff.plan) {
        print!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_feature_produces_no_hint() {
        assert!(render_hint("read", Plan::Free).is_none());
        assert!(render_hint("some_local_thing", Plan::Free).is_none());
    }

    #[test]
    fn cloud_sync_hint_targets_pro_and_reassures_local() {
        let text = render_hint("cloud_sync", Plan::Free).expect("gated → hint");
        assert!(text.contains("Cloud sync (Personal Cloud)"));
        assert!(text.contains("lean-ctx Pro feature"));
        assert!(text.contains("Everything local keeps working"));
        assert!(text.contains("lean-ctx cloud upgrade --plan pro"));
        assert!(!text.contains("You're on"));
    }

    #[test]
    fn private_registry_hint_targets_team_and_shows_current_plan() {
        let text = render_hint("private_registry", Plan::Pro).expect("gated → hint");
        assert!(text.contains("lean-ctx Team feature"));
        assert!(text.contains("You're on Pro."));
        assert!(text.contains("lean-ctx cloud upgrade --plan team"));
    }

    #[test]
    fn entitled_current_plan_suppresses_misleading_current_line() {
        let text = render_hint("cloud_sync", Plan::Pro).expect("still renders");
        assert!(!text.contains("You're on"));
    }

    #[test]
    fn enterprise_capability_points_to_sales() {
        let text = render_hint("sso_scim", Plan::Team).expect("gated → hint");
        assert!(text.contains("lean-ctx Enterprise feature"));
        assert!(text.contains("leanctx.com/pricing"));
    }

    #[test]
    fn oidc_sso_hint_targets_team_self_serve() {
        let text = render_hint("sso_oidc", Plan::Pro).expect("gated");
        assert!(text.contains("Org SSO (OIDC)"));
        assert!(text.contains("lean-ctx Team feature"));
        assert!(text.contains("lean-ctx cloud upgrade --plan team"));
    }
}
