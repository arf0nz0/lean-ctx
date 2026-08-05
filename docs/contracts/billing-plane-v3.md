# Contract: Billing Plane v3 — OIDC SSO Entitlement (`billing-plane-v3`)

Status: stable · Plane: commercial (Team/Cloud) · Base: [`billing-plane-v1`](billing-plane-v1.md)
Source: engine `rust/src/core/billing/plans.rs` · control plane `lean-ctx-cloud/src/plan.rs`

> **Deprecation (v3.9):** The `business` plan was merged into `team` as of
> v3.9. The `business` wire id maps to `team` for backward compatibility.
> All former Business entitlements (OIDC SSO, 20 GB index, 10 connectors,
> 365-day audit) are now part of the Team plan. See the updated catalog below.

An **additive** extension of [`billing-plane-v1`](billing-plane-v1.md) (GL #460/#533):
it adds the **`sso_oidc`** entitlement key.
Per v1's own versioning rule ("adding a plan or entitlement field is additive"),
the semantics stay v1; this document exists because the v1 doc is frozen and the
addition deserves its own normative record. Everything in v1 and
[v2 (metered add-ons)](billing-plane-v2.md) still holds.

> Local-Free Invariant (RFC §4/§6): unchanged. No plan gates anything local.

## What v3 adds (over v1)

1. **`sso_oidc` entitlement key** — self-serve org SSO via OIDC (GL #482).
   Distinct from `sso_scim` (SAML SSO + SCIM provisioning), which stays the
   negotiated Enterprise surface. As of v3.9, `sso_oidc` is a **Team**
   entitlement (previously required the now-removed Business plan).

## Catalog delta (post v3.9 merger)

| Entitlement | team | enterprise |
|-------------|------|------------|
| billing model | $18/seat/mo | negotiated |
| seats | unlimited | unlimited |
| hosted_index_mb | 20000 | unlimited |
| managed_connectors | 10 | unlimited |
| private_registry | yes | yes |
| sso_oidc | **yes** | yes |
| sso_scim | no | yes |
| audit_retention_days | 365 | 3650 |
| revenue_share | yes | yes |
| supporter / cloud_sync | yes | yes |

## `entitlement_allows` / `min_plan_for`

- `entitlement_allows(plan, "sso_oidc")` resolves from the catalog:
  `team` and `enterprise` only.
- `min_plan_for("sso_oidc") == Some(Team)` — upgrade hints (#346) point to
  the self-serve checkout (`lean-ctx cloud upgrade --plan team`).
- `min_plan_for("sso_scim") == Some(Enterprise)` — unchanged.

## Wire ids

`business` (alias `biz`) parses to `Plan::Team` for backward compatibility;
unknown ids still map to `free` (fail-open, never gates). The `team` id is
stable and appears in checkout
(`POST /api/billing/checkout {"plan": "team"}`), webhook plan mapping
(`STRIPE_PRICE_TEAM_MONTHLY` / `_YEARLY`), entitlement payloads and the CLI
(`lean-ctx billing entitlements team`).

## Invariants (test-enforced)

1. All v1 invariants (local-free, additive ladder, privacy) — unchanged.
2. `team` includes self-serve OIDC SSO (`sso_oidc`) without SAML/SCIM
   (`team_includes_self_serve_governance`).
3. Catalog fixtures match byte-for-byte on both repos
   (`catalog_matches_golden_fixture`, engine + control plane).

## Versioning

Future additive plan/entitlement changes append to this ladder under the same
rule. Removing/renaming a field or changing local-free semantics requires a new
major contract version.
