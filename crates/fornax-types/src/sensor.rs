//! Sensor/source contract for evidence collection (FORNX-157, parent epic
//! FORNX-138).
//!
//! Today, evidence is produced by ad-hoc code inline in each adapter's
//! `translate()` (e.g. `fornax-adapter-claude`'s Bash exit-code heuristic).
//! That code already does the right thing — it checks
//! [`crate::RuntimeCapabilities`] before claiming a signal, and it stamps a
//! human-readable `provenance` string on every [`crate::Evidence`] it
//! produces. What it does not do is expose that shape as a *contract* other
//! collectors (a Git sensor, a CI-webhook sensor, a future
//! reasoning/logprob sensor) can implement uniformly, or attach *structured*
//! provenance/trust metadata that survives a join-free read.
//!
//! This module formalizes that as two things:
//!
//! - [`EvidenceSource`]: a structured "what produced this, and how much do
//!   we trust it" record, attached to [`crate::Evidence::source`]. Replaces
//!   ad-hoc free-text provenance strings as far as trust/collector identity
//!   is concerned — `Evidence::provenance` (the existing field) stays as the
//!   more granular "handler/branch that fired" breadcrumb (e.g.
//!   `"claude_code:PostToolUse:Bash#heuristic:stderr_empty"`); the two are
//!   complementary, not a replacement of one by the other.
//! - [`EvidenceSensor`]: the trait a collector implements. Deliberately one
//!   method (`collect`) plus two capability-declaration accessors, mirroring
//!   [`crate::CapabilityProbe`]'s "one method, no registry, no dynamic
//!   negotiation" shape (FORNX-155) for the same reason: a sensor's declared
//!   capabilities are fixed at implementation time, not discovered at
//!   runtime. This is deliberate — see the module's non-goals below.
//!
//! # Non-goals (explicit, FORNX-157 AC)
//!
//! - **No dynamic plugin loading.** A sensor is a concrete Rust type an
//!   adapter or the daemon constructs and calls directly, the same way
//!   `translate()` already calls its heuristic functions directly. There is
//!   no sensor registry, no `Box<dyn EvidenceSensor>` dispatch table, no
//!   discovery mechanism.
//! - **No evidence weighting/fusion.** A sensor either collects evidence or
//!   explains why it didn't ([`SensorOutcome`]); nothing here scores,
//!   ranks, or merges evidence from multiple sensors. That stays entirely
//!   out of scope, same as it is for [`crate::Verifier`]-adjacent code today
//!   (verifiers consume already-collected `Evidence`, they don't produce
//!   or arbitrate it).
//!
//! # Worked example: a future `ReasoningSummarySensor`
//!
//! FORNX-157 AC requires showing how a not-yet-built signal (a provider's
//! reasoning/chain-of-thought summary, logprobs, or other model-internal
//! telemetry) would attach to this contract without a core rewrite. The
//! `sensor_tests` module below implements exactly this, compiled and
//! tested: a `ReasoningSummarySensor` that declares
//! [`crate::SignalClass::ReasoningSummary`], reports
//! [`TrustClass::ModelInternal`], and — because no provider integration
//! exposes that signal yet — always returns
//! [`crate::SignalAvailability::Unsupported`] from `collect`. No change to
//! `EvidenceSensor`, `EvidenceSource`, `Evidence`, or `Verifier` was needed
//! to add it; it exists purely as an additional implementor of the trait
//! already defined here, exactly as a real future sensor would.

use serde::{Deserialize, Serialize};

use crate::{AgentEvent, Evidence, Provider, RuntimeCapabilities, SignalAvailability, SignalClass};

/// How much a piece of evidence's *origin* should be trusted, independent of
/// its content. Named exactly per FORNX-138/FORNX-157's taxonomy — do not
/// rename variants without updating the ticket-referenced vocabulary.
///
/// Attached to [`EvidenceSource`], not [`Evidence`] directly, so it always
/// travels with the rest of "what produced this" rather than living as a
/// second, easily-desynced tag on `Evidence` itself.
///
/// Carries the same `#[serde(untagged)] Unrecognized(String)` forward-compat
/// tail as [`SignalClass`]/[`SignalAvailability`] (FORNX-155 precedent) — a
/// persisted `EvidenceSource` is exactly as durable a payload as a persisted
/// `RuntimeCapabilities`, and must tolerate a future variant the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// Reported by the coding agent's own provider integration (e.g. Claude
    /// Code's `tool_response`, Codex's rollout JSONL) — the provider's own
    /// account of what happened, not independently measured by Fornax.
    AgentAdjacent,
    /// Measured directly by Fornax's own local tooling (a literal process
    /// exit code Fornax captured itself, a `git` invocation Fornax ran) —
    /// independent of what the provider claims happened.
    HostObserved,
    /// From a system outside both the agent and the local host — a CI
    /// webhook, a third-party API result.
    IndependentExternal,
    /// Confirmed or entered by a person, not inferred from any automated
    /// signal.
    HumanReviewed,
    /// From the model's own internals (reasoning summaries, logprobs,
    /// other provider-internal telemetry) — see [`crate::SignalClass::ReasoningSummary`]
    /// / [`crate::SignalClass::RawReasoning`] / [`crate::SignalClass::TokenLogprobs`].
    ModelInternal,
    /// Forward-compatibility catch-all, matching
    /// [`crate::SignalAvailability::Unrecognized`]'s round-trip guarantee.
    /// Must stay last.
    #[serde(untagged)]
    Unrecognized(String),
}

/// Structured identity/provenance for one piece of [`Evidence`]: which
/// sensor produced it, when, from which adapter/provider connection, and
/// how much its origin should be trusted. First-class metadata (a struct),
/// not a string tag folded into `Evidence::provenance`.
///
/// `None` on [`Evidence::source`] means exactly what it says: this evidence
/// predates the sensor contract (FORNX-157) or was constructed by code that
/// has not been migrated onto it yet — not "trust unknown" in some deeper
/// sense. See `fornax-store`'s migration for the on-disk equivalent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSource {
    /// The producing sensor's stable name (mirrors [`crate::Verifier::name`]
    /// — a `&'static str` identity, stored as an owned `String` here because
    /// it crosses serialization, unlike a verifier's in-process-only name).
    pub sensor_name: String,
    pub trust_class: TrustClass,
    /// RFC3339 timestamp the sensor collected this evidence. Distinct from
    /// `Evidence::observed_at`, which is when the underlying event was
    /// observed — a sensor may run its collection step after the event it
    /// reads from was already recorded.
    pub collected_at: String,
    /// Which adapter/provider connection this sensor was running under, if
    /// applicable. `None` for a sensor with no single owning provider (e.g.
    /// a future host-level Git sensor that isn't tied to any one adapter
    /// connection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
}

impl EvidenceSource {
    /// Convenience constructor stamping `collected_at` as now — the common
    /// case for a sensor producing evidence synchronously from live input.
    pub fn now(
        sensor_name: &'static str,
        trust_class: TrustClass,
        provider: Option<Provider>,
    ) -> Self {
        Self {
            sensor_name: sensor_name.to_string(),
            trust_class,
            collected_at: chrono::Utc::now().to_rfc3339(),
            provider,
        }
    }
}

/// Result of one [`EvidenceSensor::collect`] call. A struct rather than an
/// enum with `Collected`/`Unavailable`/`Failed` variants because a sensor
/// must be able to report *partial* availability honestly: e.g. a
/// hypothetical multi-file diff sensor that produced evidence for two of
/// three changed files before hitting a permission error on the third must
/// not be forced to choose between silently dropping the two it got or
/// lying about the one it didn't.
///
/// Reading the four combinations:
/// - `state: Available`, `evidence` non-empty — normal success.
/// - `state: Available`, `evidence` empty — the sensor ran, its required
///   capabilities are observable, but there was nothing evidentiary to
///   report this call (e.g. a Bash call with no recognizable exit-code
///   shape at all — see `fornax-adapter-claude`'s heuristic fallback).
/// - `state` not `Available`, `evidence` non-empty — partial collection:
///   some evidence was produced before/alongside a failure or capability
///   gap. Callers must not discard the evidence just because `state` isn't
///   clean.
/// - `state` not `Available`, `evidence` empty — clean non-collection: the
///   sensor could not observe anything at all, `state` says why
///   ([`SignalAvailability::Unsupported`]/`Unavailable`/`Redacted`/
///   `CollectionFailed`/`Unknown`).
///
/// A sensor-internal timeout is reported as `state: CollectionFailed` with
/// `detail` naming the timeout — there is no separate `SensorError` type.
/// This reuses [`SignalAvailability`]'s existing "we tried and it errored"
/// variant rather than inventing a parallel failure taxonomy (FORNX-157 AC:
/// unavailability/failure returns use `SignalAvailability`).
#[derive(Debug, Clone)]
pub struct SensorOutcome {
    pub evidence: Vec<Evidence>,
    pub state: SignalAvailability,
    pub detail: Option<String>,
}

impl SensorOutcome {
    /// Normal success: one or more evidence items, capability confirmed
    /// available.
    pub fn collected(evidence: Vec<Evidence>) -> Self {
        Self {
            evidence,
            state: SignalAvailability::Available,
            detail: None,
        }
    }

    /// Capability confirmed available, but nothing evidentiary to report
    /// this call.
    pub fn nothing_to_report() -> Self {
        Self {
            evidence: vec![],
            state: SignalAvailability::Available,
            detail: None,
        }
    }

    /// Clean non-collection: no evidence, `state` explains why (must not be
    /// `Available` — use `collected`/`nothing_to_report` for that).
    pub fn not_collected(state: SignalAvailability, detail: impl Into<Option<String>>) -> Self {
        Self {
            evidence: vec![],
            state,
            detail: detail.into(),
        }
    }

    /// True only when at least one evidence item was produced, regardless
    /// of `state` (partial collection still counts).
    pub fn has_evidence(&self) -> bool {
        !self.evidence.is_empty()
    }
}

/// Contract for a component that observes something and turns it into
/// canonical [`Evidence`], the observation-collection side of the
/// "immutable observation before interpretation" invariant
/// (`docs/adr/0001-architecture-invariants.md`). A sensor must never
/// interpret/verify — that's [`crate::Verifier`]'s job, downstream of
/// already-collected evidence.
///
/// Implementors live in the adapter/collector crate that owns the raw input
/// a sensor reads (e.g. `fornax-adapter-claude` owns `ClaudeBashExitCodeSensor`
/// because it owns the Claude Code `tool_response` shape), not in
/// `fornax-types` itself — this trait is the shared contract, not a place to
/// centralize collection logic (adapters stay thin, D5, but the *shape* they
/// implement is now uniform).
pub trait EvidenceSensor {
    /// Stable identity, stamped onto every [`EvidenceSource::sensor_name`]
    /// this sensor produces.
    fn name(&self) -> &'static str;

    /// The [`SignalClass`]es this sensor needs `Available` to collect
    /// anything at all. A sensor may still choose to check `caps` itself
    /// inside `collect` for finer-grained partial-availability behavior;
    /// this is the coarse declaration a caller can use to skip calling
    /// `collect` entirely when nothing is observable.
    fn required_capabilities(&self) -> &'static [SignalClass];

    /// How much this sensor's output should be trusted, attached to every
    /// [`EvidenceSource`] it stamps.
    fn trust_class(&self) -> TrustClass;

    /// Attempt to collect evidence from `event`, given what `caps` says is
    /// currently observable. Must not block indefinitely — a sensor backed
    /// by a genuinely slow/remote source (a future CI-webhook sensor) is
    /// responsible for enforcing its own budget and reporting
    /// `SignalAvailability::CollectionFailed` with a `detail` naming the
    /// timeout, rather than this trait imposing an OS-level deadline no
    /// sensor implemented so far actually needs.
    fn collect(&self, event: &AgentEvent, caps: &RuntimeCapabilities) -> SensorOutcome;

    /// Convenience: true only if every required capability is confirmed
    /// `Available`. Sensors with partial-availability behavior should not
    /// rely on this alone inside `collect` — see the trait doc.
    fn is_ready(&self, caps: &RuntimeCapabilities) -> bool {
        self.required_capabilities()
            .iter()
            .all(|c| caps.is_observable(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilitySignal, EventKind, EvidenceKind};
    use uuid::Uuid;

    fn caps_with(signals: Vec<CapabilitySignal>) -> RuntimeCapabilities {
        RuntimeCapabilities {
            schema_version: crate::CAPABILITY_SCHEMA_VERSION,
            provider: Provider::ClaudeCode,
            signals,
            notes: Default::default(),
        }
    }

    fn dummy_event() -> AgentEvent {
        AgentEvent {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            provider: Provider::ClaudeCode,
            kind: EventKind::PostToolUse,
            observed_at: "2026-01-01T00:00:00Z".into(),
            tool_name: Some("Bash".into()),
            tool_input: None,
            tool_response: None,
            raw: serde_json::json!({}),
        }
    }

    // --- TrustClass round-trip (FORNX-155 precedent) --------------------

    #[test]
    fn unrecognized_trust_class_tag_round_trips_the_original_string() {
        let json = r#""quantum_verified""#;
        let v: TrustClass = serde_json::from_str(json).unwrap();
        assert_eq!(v, TrustClass::Unrecognized("quantum_verified".to_string()));
        let back = serde_json::to_string(&v).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn every_canonical_trust_class_tag_round_trips_to_its_named_variant() {
        let cases = [
            ("\"agent_adjacent\"", TrustClass::AgentAdjacent),
            ("\"host_observed\"", TrustClass::HostObserved),
            ("\"independent_external\"", TrustClass::IndependentExternal),
            ("\"human_reviewed\"", TrustClass::HumanReviewed),
            ("\"model_internal\"", TrustClass::ModelInternal),
        ];
        for (json, expected) in cases {
            let v: TrustClass = serde_json::from_str(json).unwrap();
            assert_eq!(v, expected, "tag {json} did not parse to its named variant");
        }
    }

    // --- EvidenceSource round-trip ---------------------------------------

    #[test]
    fn evidence_source_serializes_and_round_trips() {
        let src = EvidenceSource::now(
            "claude_bash_exit_code_sensor_v1",
            TrustClass::AgentAdjacent,
            Some(Provider::ClaudeCode),
        );
        let json = serde_json::to_string(&src).unwrap();
        let back: EvidenceSource = serde_json::from_str(&json).unwrap();
        assert_eq!(src, back);
    }

    // --- SensorOutcome partial-availability combinations ------------------

    #[test]
    fn sensor_outcome_supports_partial_availability_with_evidence_and_a_non_available_state() {
        let ev = Evidence {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            source_event_id: Uuid::new_v4(),
            kind: EvidenceKind::ExitCode,
            observed_at: "2026-01-01T00:00:00Z".into(),
            payload: serde_json::json!({"exit_code": 0}),
            provenance: "test:partial".into(),
            source: None,
            extension: None,
        };
        let outcome = SensorOutcome {
            evidence: vec![ev.clone()],
            state: SignalAvailability::CollectionFailed,
            detail: Some("collected one of two expected items before erroring".to_string()),
        };
        assert!(outcome.has_evidence());
        assert_ne!(outcome.state, SignalAvailability::Available);
    }

    #[test]
    fn not_collected_is_clean_when_no_evidence_and_non_available_state() {
        let outcome = SensorOutcome::not_collected(
            SignalAvailability::Unsupported,
            Some("runtime cannot expose this signal".to_string()),
        );
        assert!(!outcome.has_evidence());
        assert_eq!(outcome.state, SignalAvailability::Unsupported);
    }

    // --- Worked example: a future ReasoningSummarySensor ------------------
    // FORNX-157 AC: docs must show how a future reasoning/logprob/internal
    // signal sensor would attach without core rewrites. This is that
    // example, compiled and exercised — no provider integration exposes
    // this signal today, so `collect` always reports `Unsupported`.

    struct ReasoningSummarySensor;

    impl EvidenceSensor for ReasoningSummarySensor {
        fn name(&self) -> &'static str {
            "reasoning_summary_sensor_v1"
        }

        fn required_capabilities(&self) -> &'static [SignalClass] {
            &[SignalClass::ReasoningSummary]
        }

        fn trust_class(&self) -> TrustClass {
            TrustClass::ModelInternal
        }

        fn collect(&self, _event: &AgentEvent, caps: &RuntimeCapabilities) -> SensorOutcome {
            if !self.is_ready(caps) {
                return SensorOutcome::not_collected(
                    SignalAvailability::Unsupported,
                    Some(
                        "no provider integration exposes reasoning-summary content yet".to_string(),
                    ),
                );
            }
            // Unreachable today (no provider ever declares this class
            // Available), but a real implementation would extract the
            // summary from `event.raw` here and return
            // `SensorOutcome::collected(vec![...])`.
            SensorOutcome::nothing_to_report()
        }
    }

    #[test]
    fn future_reasoning_summary_sensor_attaches_via_the_existing_trait_with_no_core_changes() {
        let sensor = ReasoningSummarySensor;
        assert_eq!(sensor.name(), "reasoning_summary_sensor_v1");
        assert_eq!(sensor.trust_class(), TrustClass::ModelInternal);

        // No provider declares ReasoningSummary Available today.
        let caps = caps_with(vec![]);
        assert!(!sensor.is_ready(&caps));
        let outcome = sensor.collect(&dummy_event(), &caps);
        assert!(!outcome.has_evidence());
        assert_eq!(outcome.state, SignalAvailability::Unsupported);

        // If a future provider *did* declare it available, the same sensor
        // (unmodified) would be ready.
        let future_caps = caps_with(vec![CapabilitySignal {
            class: SignalClass::ReasoningSummary,
            state: SignalAvailability::Available,
            detail: None,
        }]);
        assert!(sensor.is_ready(&future_caps));
    }
}
