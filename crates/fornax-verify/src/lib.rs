//! Deterministic claim verification (FORNX-27).
//!
//! `Claim + Evidence[] + RuntimeCapabilities -> Finding`, pure — no I/O.
//! Verifiers must never invent evidence: absent the signal they need, they
//! return `Unavailable`, not `Verified`.

use fornax_types::{Claim, Evidence, Finding, RuntimeCapabilities, Verdict};
use uuid::Uuid;

pub trait Verifier {
    /// Verifier's stable name, recorded on every `Finding` it produces.
    fn name(&self) -> &'static str;

    /// Whether this verifier applies to the given claim's `subject`.
    fn applies_to(&self, claim: &Claim) -> bool;

    /// Compute a finding for `claim` given all evidence observed so far in
    /// the same session and what the runtime is capable of observing.
    fn verify(&self, claim: &Claim, evidence: &[Evidence], caps: &RuntimeCapabilities) -> Finding;
}

/// First high-signal verifier (epic's canonical aha scenario): an agent
/// claims tests passed; deterministically check the most recent test-runner
/// exit code observed in evidence.
pub struct TestResultVerifier;

impl TestResultVerifier {
    /// Very small, deliberately literal claim-text heuristic for v0.0.1 —
    /// claim extraction itself is out of scope for FORNX-27; this verifier
    /// only decides the subject match and the verdict, given an already
    /// extracted `Claim`.
    pub fn claims_tests_passed(text: &str) -> bool {
        let t = text.to_lowercase();
        (t.contains("test") || t.contains("tests"))
            && (t.contains("passed")
                || t.contains("pass")
                || t.contains("succeeded")
                || t.contains("all green"))
            && !t.contains("failed")
    }
}

impl Verifier for TestResultVerifier {
    fn name(&self) -> &'static str {
        "test_result_verifier_v1"
    }

    fn applies_to(&self, claim: &Claim) -> bool {
        claim.subject == "test_result"
    }

    fn verify(&self, claim: &Claim, evidence: &[Evidence], caps: &RuntimeCapabilities) -> Finding {
        let now = chrono::Utc::now().to_rfc3339();

        if !caps.supports_post_tool_use && !caps.supports_transcript_tail {
            return unavailable(
                claim.id,
                self.name(),
                "runtime does not expose tool-result/transcript evidence needed to check exit codes",
                now,
            );
        }

        // Find the most recent exit-code-bearing evidence for a test-runner
        // invocation in this session, most recent first (evidence is stored
        // oldest-first; scan from the end).
        let test_evidence = evidence.iter().rev().find(|e| is_test_runner_evidence(e));

        let Some(ev) = test_evidence else {
            return Finding {
                id: Uuid::new_v4(),
                claim_id: claim.id,
                verdict: Verdict::Unverified,
                evidence_ids: vec![],
                verifier_name: self.name().to_string(),
                rationale: "no test-runner invocation observed in this session's evidence"
                    .to_string(),
                computed_at: now,
            };
        };

        let exit_code = ev.payload.get("exit_code").and_then(|v| v.as_i64());

        match exit_code {
            Some(0) => Finding {
                id: Uuid::new_v4(),
                claim_id: claim.id,
                verdict: Verdict::Verified,
                evidence_ids: vec![ev.id],
                verifier_name: self.name().to_string(),
                rationale: format!("observed test-runner exit_code=0 ({})", ev.provenance),
                computed_at: now,
            },
            Some(code) => Finding {
                id: Uuid::new_v4(),
                claim_id: claim.id,
                verdict: Verdict::Contradicted,
                evidence_ids: vec![ev.id],
                verifier_name: self.name().to_string(),
                rationale: format!(
                    "claim states tests passed, but observed test-runner exit_code={code} ({})",
                    ev.provenance
                ),
                computed_at: now,
            },
            None => unavailable(
                claim.id,
                self.name(),
                "test-runner evidence found but no exit_code field present",
                now,
            ),
        }
    }
}

fn is_test_runner_evidence(e: &Evidence) -> bool {
    let cmd = e
        .payload
        .get("command")
        .map(|v| v.to_string().to_lowercase())
        .unwrap_or_default();
    e.payload.get("exit_code").is_some()
        && (cmd.contains("pytest")
            || cmd.contains("cargo test")
            || cmd.contains("cargo nextest")
            || cmd.contains("npm test")
            || cmd.contains("vitest")
            || cmd.contains("jest"))
}

fn unavailable(claim_id: Uuid, verifier: &str, reason: &str, now: String) -> Finding {
    Finding {
        id: Uuid::new_v4(),
        claim_id,
        verdict: Verdict::Unavailable,
        evidence_ids: vec![],
        verifier_name: verifier.to_string(),
        rationale: reason.to_string(),
        computed_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fornax_types::{EvidenceKind, Provider};

    fn caps() -> RuntimeCapabilities {
        RuntimeCapabilities {
            provider: Provider::Codex,
            supports_pre_tool_use: true,
            supports_post_tool_use: true,
            supports_tool_response_capture: true,
            supports_session_stop_event: true,
            supports_transcript_tail: true,
            supports_subagent_lifecycle: false,
            notes: Default::default(),
        }
    }

    fn evidence_with_exit_code(code: i64) -> Evidence {
        Evidence {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            source_event_id: Uuid::new_v4(),
            kind: EvidenceKind::ExitCode,
            observed_at: chrono::Utc::now().to_rfc3339(),
            payload: serde_json::json!({"command": ["pytest"], "exit_code": code}),
            provenance: "codex:rollout:exec_command_end".into(),
        }
    }

    fn claim(text: &str) -> Claim {
        Claim {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            source_event_id: Uuid::new_v4(),
            text: text.into(),
            subject: "test_result".into(),
            claimed_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn contradicts_when_agent_claims_pass_but_exit_code_nonzero() {
        let v = TestResultVerifier;
        let c = claim("All tests passed.");
        let ev = vec![evidence_with_exit_code(1)];
        let f = v.verify(&c, &ev, &caps());
        assert_eq!(f.verdict, Verdict::Contradicted);
        assert_eq!(f.evidence_ids, vec![ev[0].id]);
    }

    #[test]
    fn verified_when_exit_code_zero() {
        let v = TestResultVerifier;
        let c = claim("All tests passed.");
        let ev = vec![evidence_with_exit_code(0)];
        let f = v.verify(&c, &ev, &caps());
        assert_eq!(f.verdict, Verdict::Verified);
    }

    #[test]
    fn unverified_when_no_test_evidence_present() {
        let v = TestResultVerifier;
        let c = claim("All tests passed.");
        let f = v.verify(&c, &[], &caps());
        assert_eq!(f.verdict, Verdict::Unverified);
    }

    #[test]
    fn unavailable_when_runtime_cannot_observe_tool_results() {
        let v = TestResultVerifier;
        let c = claim("All tests passed.");
        let mut no_caps = caps();
        no_caps.supports_post_tool_use = false;
        no_caps.supports_transcript_tail = false;
        let f = v.verify(&c, &[], &no_caps);
        assert_eq!(f.verdict, Verdict::Unavailable);
    }

    #[test]
    fn claim_text_heuristic_matches_expected_phrasings() {
        assert!(TestResultVerifier::claims_tests_passed("All tests passed."));
        assert!(TestResultVerifier::claims_tests_passed("tests succeeded"));
        assert!(!TestResultVerifier::claims_tests_passed("3 tests failed"));
    }

    /// FORNX-27 AC: "A replay command/test can recompute findings from
    /// persisted evidence." Re-running verify() against the exact same
    /// claim+evidence+capabilities (as if freshly loaded from storage,
    /// FORNX-26) must yield the identical verdict and rationale — no hidden
    /// state, no non-determinism, safe to recompute after a verifier change.
    #[test]
    fn recomputing_from_the_same_persisted_inputs_is_deterministic() {
        let v = TestResultVerifier;
        let c = claim("All tests passed.");
        let ev = vec![evidence_with_exit_code(1)];
        let capabilities = caps();

        let first = v.verify(&c, &ev, &capabilities);
        let replayed = v.verify(&c, &ev, &capabilities);

        assert_eq!(first.verdict, replayed.verdict);
        assert_eq!(first.rationale, replayed.rationale);
        assert_eq!(first.evidence_ids, replayed.evidence_ids);
    }
}
