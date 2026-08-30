//! Comprehensive contract tests (FORNX-160), extending FORNX-156's original
//! property suite (`tests/conformance.rs`) beyond what fell out incidentally
//! of that ticket's own scope: capability declaration correctness
//! (FORNX-155), provenance correctness (FORNX-157), canonical-payload
//! validation wired to real adapter output (FORNX-158, picking up part of
//! FORNX-289), and unknown-event/failure semantics interacting correctly
//! with the extension envelope (FORNX-158).

use fornax_adapter_claude::ClaudeAdapter;
use fornax_adapter_codex::CodexAdapter;
use fornax_adapter_conformance::{
    capability_declaration_is_well_formed,
    evidence_payloads_validate_against_their_canonical_schema, evidence_sources_are_valid,
    load_fixtures, replay_fixture,
};
use fornax_types::{
    AgentAdapter, ContentClass, ExtensionEnvelope, IngestMessage, NormalizationOutcome, Provider,
};

fn claude_native_events() -> Vec<serde_json::Value> {
    load_fixtures("claude")
        .into_iter()
        .filter(|f| !f.name.starts_with("unrecognized_"))
        .flat_map(|f| f.native_events)
        .collect()
}

fn codex_native_events() -> Vec<serde_json::Value> {
    // Excludes the call/output pairing fixture: these generic per-event
    // checks replay a fresh adapter per native event, which would break the
    // call_id correlation. The pairing itself is covered end-to-end by
    // `golden_fixtures.rs`'s FORNX-55 regression test.
    load_fixtures("codex")
        .into_iter()
        .filter(|f| !f.name.starts_with("unrecognized_") && !f.name.contains("pair"))
        .flat_map(|f| f.native_events)
        .collect()
}

// --- Capability declaration correctness (FORNX-155) -----------------------

#[test]
fn claude_capability_declaration_is_well_formed() {
    capability_declaration_is_well_formed(&ClaudeAdapter);
}

#[test]
fn codex_capability_declaration_is_well_formed() {
    capability_declaration_is_well_formed(&CodexAdapter::new());
}

// --- Provenance correctness (FORNX-157) ------------------------------------

#[test]
fn claude_evidence_from_golden_fixtures_carries_valid_provenance() {
    let mut adapter = ClaudeAdapter;
    evidence_sources_are_valid(&mut adapter, "fixture-hint", &claude_native_events());
}

#[test]
fn codex_evidence_from_golden_fixtures_carries_valid_provenance() {
    let mut adapter = CodexAdapter::new();
    evidence_sources_are_valid(&mut adapter, "fixture-hint", &codex_native_events());
}

// --- Canonical-payload validation wired to real output (FORNX-158/289) ----

#[test]
fn claude_evidence_payloads_validate_against_their_canonical_schema() {
    let mut adapter = ClaudeAdapter;
    evidence_payloads_validate_against_their_canonical_schema(
        &mut adapter,
        "fixture-hint",
        &claude_native_events(),
    );
}

#[test]
fn codex_evidence_payloads_validate_against_their_canonical_schema() {
    let mut adapter = CodexAdapter::new();
    evidence_payloads_validate_against_their_canonical_schema(
        &mut adapter,
        "fixture-hint",
        &codex_native_events(),
    );
}

// --- Unknown-event/failure semantics interacting with the extension
//     envelope (FORNX-158) ---------------------------------------------------

/// An `Unrecognized` outcome must never carry any canonical message
/// alongside it — not an event, not evidence, and therefore never an
/// `ExtensionEnvelope` either. `NormalizationOutcome::into_messages()`
/// already guarantees this structurally (see `fornax-types/src/adapter.rs`),
/// but this test re-affirms it at the conformance-suite level against real
/// breaking-change fixtures, not just the type's own unit tests.
#[test]
fn unrecognized_breaking_change_fixtures_never_produce_any_canonical_message() {
    let claude_fixture = load_fixtures("claude")
        .into_iter()
        .find(|f| f.name == "unrecognized_future_hook")
        .expect("expected the unrecognized_future_hook fixture");
    let mut claude = ClaudeAdapter;
    let outcomes = replay_fixture(&mut claude, "fixture-hint", &claude_fixture);
    for outcome in outcomes {
        assert!(outcome.into_messages().is_empty());
    }

    let codex_fixture = load_fixtures("codex")
        .into_iter()
        .find(|f| f.name == "unrecognized_future_event")
        .expect("expected the unrecognized_future_event fixture");
    let mut codex = CodexAdapter::new();
    let outcomes = replay_fixture(&mut codex, "fixture-hint", &codex_fixture);
    for outcome in outcomes {
        assert!(outcome.into_messages().is_empty());
    }
}

/// An adapter must never construct an `ExtensionEnvelope` from data it did
/// not itself recognize (see `extension.rs`'s "Not a laundering path for
/// unrecognized native payloads" module doc). Neither shipped adapter uses
/// the envelope today, so this test pins the boundary at the type level: an
/// `ExtensionEnvelope` built with a schema_version outside
/// `SUPPORTED_EXTENSION_SCHEMA_VERSIONS` must fail to deserialize — the
/// "explicit hard failure over silent corruption" half of FORNX-158's
/// forward/backward-compatibility model, re-affirmed here as a conformance
/// concern rather than only an internal `fornax-types` unit test.
#[test]
fn a_version_incompatible_extension_envelope_is_rejected_not_silently_accepted() {
    let envelope = ExtensionEnvelope::new(
        Provider::Codex,
        "conformance-fixture-adapter-0.0.0",
        ContentClass::RawProviderMetadata,
        serde_json::json!({}),
    );
    let mut value = serde_json::to_value(&envelope).unwrap();
    value["schema_version"] = serde_json::json!(9999);
    let result: Result<ExtensionEnvelope, _> = serde_json::from_value(value);
    assert!(
        result.is_err(),
        "an incompatible schema_version must fail explicitly, never silently parse"
    );
}

// --- Sanity: fixtures actually exercise every documented sensor path ------

/// Guards against a golden-fixture regression where every fixture happens
/// to stop producing Evidence (e.g. a fixture bit-rots as the real provider
/// shape drifts again) without any test noticing — at least one Claude and
/// one Codex fixture must still produce real Evidence today.
#[test]
fn at_least_one_fixture_per_provider_still_produces_evidence() {
    let mut claude = ClaudeAdapter;
    let claude_has_evidence = claude_native_events().iter().any(|native| {
        matches!(
            claude.normalize("fixture-hint", native),
            NormalizationOutcome::Messages(msgs)
                if msgs.iter().any(|m| matches!(m, IngestMessage::Evidence(_)))
        )
    });
    assert!(
        claude_has_evidence,
        "expected at least one Claude fixture to produce Evidence"
    );

    let mut codex = CodexAdapter::new();
    let codex_has_evidence = codex_native_events().iter().any(|native| {
        matches!(
            codex.normalize("fixture-hint", native),
            NormalizationOutcome::Messages(msgs)
                if msgs.iter().any(|m| matches!(m, IngestMessage::Evidence(_)))
        )
    });
    assert!(
        codex_has_evidence,
        "expected at least one Codex fixture to produce Evidence"
    );
}
