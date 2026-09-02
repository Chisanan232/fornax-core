//! Decision UX (FORNX-96, parent epic FORNX-66, local half only — see this
//! ticket's PR body for the SaaS-half follow-up). Translates a computed
//! [`crate::fusion::FusedFinding`] into an actionable recommendation —
//! `PROCEED`/`REVIEW`/`BLOCK` — without ever hiding or replacing the
//! underlying evidence that produced it.
//!
//! # Relationship to fusion
//!
//! This module is strictly downstream of [`crate::fusion`]: a
//! [`DecisionPolicy`] consumes an already-computed `FusedFinding` and never
//! recomputes or reinterprets the evidence graph itself. [`Recommendation`]
//! carries `claim_id` — a pointer back to the `FusedFinding`/claim it was
//! computed from — and never embeds or duplicates the fusion rationale
//! trail. FORNX-96 AC: "Recommendation never replaces the underlying
//! Finding/evidence graph" is enforced operationally at the daemon layer
//! (`fornax-daemon`'s `/api/decision` always returns the `Recommendation`
//! *and* the `FusedFinding` together, never one instead of the other) —
//! this module only guarantees the type-level half of that contract.
//!
//! # Policy identity is separate from fusion policy identity
//!
//! [`Recommendation::policy_name`]/[`Recommendation::policy_version`] name
//! the *decision* policy (e.g. `"default_risk_policy_v1"`), a distinct
//! identity axis from `FusedFinding::policy_name`/`policy_version`, which
//! name the *fusion* policy that produced the referenced `FusedFinding`. A
//! caller must never conflate the two — swapping the decision policy must
//! never require re-fusing evidence, and vice versa.
//!
//! # Risk classes make the same finding yield different actions
//!
//! FORNX-96 AC: "Same finding can yield different actions under explicit
//! policy/risk contexts without changing historical evidence." [`RiskClass`]
//! is the explicit context a caller supplies; a `DecisionPolicy` is expected
//! to be pure — the same `(FusedFinding, RiskClass)` pair should always
//! produce the same `Recommendation`, and two different `RiskClass` values
//! run over the identical `FusedFinding` can (and, per
//! [`crate::decision::DefaultRiskPolicy`]'s mapping table, sometimes do)
//! produce different [`RecommendationAction`]s. The `FusedFinding` itself is
//! never mutated or re-derived by this process.
//!
//! # Safety floor: uncertainty is never presented as confidently safe
//!
//! FORNX-96 AC: "High uncertainty/missing critical evidence cannot be
//! presented as confidently safe by default." Any `DecisionPolicy`
//! implementation shipped in this module must treat this as a hard floor,
//! not a tunable default — see [`DefaultRiskPolicy`]'s own doc comment for
//! the concrete mapping table that enforces it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fusion::FusedFinding;

/// Closed recommendation vocabulary (FORNX-96 scope line: "PROCEED, REVIEW,
/// BLOCK ... recommendation semantics separate from integrity verdict").
/// Deliberately not merged with [`fornax_types::Verdict`] — a
/// `RecommendationAction` is a downstream decision about *what to do*,
/// never a restatement of *what was observed*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationAction {
    Proceed,
    Review,
    Block,
}

/// Closed risk-class vocabulary (FORNX-96 AC: "same finding can yield
/// different actions under explicit policy/risk contexts"). Three classes,
/// ordered from most to least conservative:
///
/// - `Strict`: minimizes false negatives (missed problems) — most likely of
///   the three to `Review`/`Block` rather than `Proceed`.
/// - `Balanced`: the default applied when no risk class is specified by the
///   caller; every hard safety floor documented on this module is written
///   against this class specifically, though `Strict`/`Lenient` honor the
///   same floors (see [`DefaultRiskPolicy`]).
/// - `Lenient`: still never crosses a hard safety floor established in
///   [`DefaultRiskPolicy`] — it only ever relaxes a `Block` to a `Review`
///   where `DefaultRiskPolicy` judges that safe, never a `Review`/`Block`
///   to `Proceed` in a case this module has decided is unsafe by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Strict,
    Balanced,
    Lenient,
}

/// Output of [`DecisionPolicy::decide`]. Points back at the
/// [`FusedFinding`]/claim it was computed from via `claim_id` rather than
/// embedding or duplicating its rationale — see this module's docs, "FORNX-96
/// AC: recommendation never replaces the underlying Finding/evidence graph".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub claim_id: Uuid,
    pub action: RecommendationAction,
    pub risk_class: RiskClass,
    /// Identity of the DECISION policy that produced this recommendation —
    /// a separate identity axis from the FUSION policy that produced the
    /// referenced [`FusedFinding`] (see module docs).
    pub policy_name: String,
    pub policy_version: u32,
    /// Short human-readable summary of *why* this action was chosen. Not a
    /// substitute for the full fusion rationale trail, which stays
    /// reachable via `claim_id` -> the referenced `FusedFinding`.
    pub rationale_summary: String,
}

/// Swap/benchmark boundary for decision recommendations (mirrors
/// [`crate::fusion::FusionPolicy`]'s shape). Maps an already-computed
/// `FusedFinding` plus an explicit `RiskClass` to one `Recommendation` —
/// never touches or recomputes the underlying evidence graph.
pub trait DecisionPolicy {
    /// Stable identity, recorded on every [`Recommendation::policy_name`]
    /// this policy produces.
    fn name(&self) -> &'static str;

    /// This policy's own version — bump whenever its mapping table changes
    /// in a way that could change output for the same input.
    fn policy_version(&self) -> u32;

    fn decide(&self, fused: &FusedFinding, risk: RiskClass) -> Recommendation;
}
