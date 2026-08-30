//! Generic `AgentAdapter` conformance harness (FORNX-156). Written against
//! the trait only, with no dependency on either concrete adapter crate — the
//! integration tests under `tests/` instantiate it against
//! `fornax-adapter-claude::ClaudeAdapter` and `fornax-adapter-codex::CodexAdapter`
//! as `[dev-dependencies]`, so this crate never becomes something a shipped
//! binary could accidentally depend on (see the `AgentAdapter` trait docs,
//! "Allowed core dependencies").
//!
//! Assertions are over *properties* every conforming adapter must hold, not
//! over message sequence/count: the two real adapters differ deliberately in
//! how often they announce capabilities (Claude: every event; Codex: once
//! per connection — both conforming, see `AgentAdapter::probe`'s doc on
//! repeated-announcement idempotency), so a sequence-shaped assertion would
//! be true for one and false for the other by construction, not by either
//! one being broken.

use fornax_types::{AgentAdapter, IngestMessage, NormalizationOutcome};

/// Every `AgentEvent`/`RuntimeCapabilities` a conforming adapter emits must
/// carry that adapter's own declared `provider()` — core code relies on this
/// to route/attribute observations without asking the adapter again.
pub fn provider_is_stamped_consistently<A: AgentAdapter>(
    adapter: &mut A,
    session_hint: &str,
    native_events: &[serde_json::Value],
) {
    let expected = adapter.provider();
    for native in native_events {
        if let NormalizationOutcome::Messages(msgs) = adapter.normalize(session_hint, native) {
            for msg in msgs {
                match msg {
                    IngestMessage::Event(e) => assert_eq!(
                        e.provider, expected,
                        "AgentEvent.provider did not match adapter.provider()"
                    ),
                    IngestMessage::Capabilities(c) => assert_eq!(
                        c.provider, expected,
                        "RuntimeCapabilities.provider did not match adapter.provider()"
                    ),
                    IngestMessage::Claim(_) | IngestMessage::Evidence(_) => {
                        // Neither carries a provider field by design
                        // (see `fornax_types::Claim`/`Evidence`) — nothing
                        // to assert here.
                    }
                }
            }
        }
    }
}

/// Every message a conforming adapter emits must be a `serde_json`-round-
/// trippable `IngestMessage` — this is the wire contract the daemon's UDS
/// ingest loop (`fornax-daemon::handle_connection`) actually parses against.
pub fn every_message_round_trips_through_the_wire_protocol<A: AgentAdapter>(
    adapter: &mut A,
    session_hint: &str,
    native_events: &[serde_json::Value],
) {
    for native in native_events {
        if let NormalizationOutcome::Messages(msgs) = adapter.normalize(session_hint, native) {
            for msg in msgs {
                let json = serde_json::to_string(&msg).expect("IngestMessage must serialize");
                let _: IngestMessage = serde_json::from_str(&json)
                    .expect("IngestMessage must round-trip through its own wire shape");
            }
        }
    }
}

/// A conforming adapter's `probe()` must report the same `provider` as
/// `provider()` itself — the two are not allowed to disagree.
pub fn probe_provider_matches_declared_provider<A: AgentAdapter>(adapter: &A) {
    assert_eq!(
        adapter.probe().provider,
        adapter.provider(),
        "CapabilityProbe::probe().provider disagreed with AgentAdapter::provider()"
    );
}

/// A conforming adapter must never panic on a shape it doesn't recognize —
/// it must classify the input as `Unrecognized` (or, for a shape it
/// recognizes but chooses not to translate, `Ignored`), never crash the
/// process/session observing it (see the `AgentAdapter` trait docs, "Error
/// semantics"). Returns the outcome so callers can additionally assert on
/// which of the two policy branches a given fixture landed in.
pub fn normalizing_never_panics<A: AgentAdapter>(
    adapter: &mut A,
    session_hint: &str,
    native: &serde_json::Value,
) -> NormalizationOutcome {
    adapter.normalize(session_hint, native)
}

/// `Unrecognized`'s `discriminator` must never be empty — an adapter that
/// can't name what it didn't recognize gives a verifier/operator nothing to
/// act on.
pub fn unrecognized_always_carries_a_discriminator(outcome: &NormalizationOutcome) {
    if let NormalizationOutcome::Unrecognized { discriminator } = outcome {
        assert!(
            !discriminator.is_empty(),
            "Unrecognized outcome must carry a non-empty discriminator"
        );
    }
}
