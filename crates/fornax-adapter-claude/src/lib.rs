//! Claude Code hook adapter (FORNX-28), formalized against the
//! `fornax_types::AgentAdapter` contract (FORNX-156). Thin by design (D5,
//! ADR 0001): no verification logic here, only translation of hook stdin
//! JSON into canonical `fornax_types::IngestMessage`s.

use fornax_types::{
    AgentAdapter, AgentEvent, CapabilityProbe, Claim, EventKind, Evidence, EvidenceKind,
    EvidenceSensor, EvidenceSource, IngestMessage, NormalizationOutcome, Provider,
    RuntimeCapabilities, SensorOutcome, SignalAvailability, SignalClass, TrustClass,
};
use uuid::Uuid;

/// This adapter implementation's own version — independent of the Claude
/// Code runtime version, which belongs in a `CapabilitySignal::detail`
/// string (see `ClaudeAdapter::probe`). Attached to every capability
/// declaration via `notes["adapter_version"]`.
pub const ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stateless: Claude Code hooks are one-shot per-invocation processes, so
/// there is no cross-call state to hold (contrast `fornax-adapter-codex`'s
/// `call_id` pairing).
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeAdapter;

impl CapabilityProbe for ClaudeAdapter {
    /// Declared conservatively, matching what this adapter actually reads —
    /// never inferred as more capable than confirmed (D5, ADR 0001).
    /// Confirmed against a live Claude Code v2.1.238 session (2026-08-29):
    /// PostToolUse and Stop both fire with the shapes this adapter parses.
    ///
    /// Formalized (FORNX-155) from six fixed bools into an explicit
    /// `SignalClass` -> `SignalAvailability` declaration. Every class this
    /// adapter previously declared `true` for stays `Available`;
    /// `ProcessResult` is `Unsupported`: Claude Code's Bash `tool_response`
    /// never carries a literal exit code, only a heuristic derived from
    /// stdout/stderr/interrupted (see `normalize`'s `PostToolUse` handling).
    fn probe(&self) -> RuntimeCapabilities {
        use fornax_types::{CapabilitySignal, SignalAvailability, SignalClass};
        RuntimeCapabilities {
            schema_version: fornax_types::CAPABILITY_SCHEMA_VERSION,
            provider: Provider::ClaudeCode,
            signals: vec![
                CapabilitySignal {
                    class: SignalClass::ToolInvocation,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                CapabilitySignal {
                    class: SignalClass::ToolTrace,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                CapabilitySignal {
                    class: SignalClass::ToolResultPayload,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                CapabilitySignal {
                    class: SignalClass::SessionLifecycle,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                CapabilitySignal {
                    class: SignalClass::FinalResponse,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                CapabilitySignal {
                    class: SignalClass::SubagentLifecycle,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                CapabilitySignal {
                    class: SignalClass::ProcessResult,
                    state: SignalAvailability::Unsupported,
                    detail: Some(
                        "Bash tool_response carries no literal exit code as of v2.1.238; \
                         ExitCode evidence is heuristic from stdout/stderr/interrupted"
                            .to_string(),
                    ),
                },
            ],
            notes: [(
                "exit_code".to_string(),
                "heuristic from stdout/stderr/interrupted — Claude Code's Bash tool_response \
                 carries no literal exit code as of v2.1.238"
                    .to_string(),
            )]
            .into(),
        }
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::ClaudeCode
    }

    fn adapter_version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    /// `session_hint` is unused here on purpose: every Claude Code hook
    /// payload carries its own `session_id`, which is authoritative for
    /// this transport (see the trait docs on why neither source is assumed
    /// authoritative in general).
    fn normalize(
        &mut self,
        session_hint: &str,
        native: &serde_json::Value,
    ) -> NormalizationOutcome {
        translate(self, session_hint, native)
    }
}

fn stamped_capabilities(adapter: &ClaudeAdapter, session_id: &str) -> RuntimeCapabilities {
    let mut caps = adapter.probe();
    // Reserved, machine-consumed transport fields — see the doc comment on
    // `RuntimeCapabilities::notes` in `fornax-types/src/capabilities.rs`.
    caps.notes
        .insert("session_id".to_string(), session_id.to_string());
    caps.notes.insert(
        "adapter_version".to_string(),
        adapter.adapter_version().to_string(),
    );
    caps
}

/// FORNX-157: formalizes what this adapter has always done inline —
/// extracting a heuristic exit code from a Claude Code Bash `tool_response`
/// — as an `EvidenceSensor`. The heuristic itself is byte-for-byte the same
/// as before this migration (see the `tests` module's existing exit-code
/// tests, whose assertions were left untouched as the before/after
/// regression proof).
///
/// Carries `adapter_version` as a field (rather than a trait parameter,
/// which `EvidenceSensor::collect`'s fixed signature has no room for) so
/// its provenance strings keep embedding it, exactly as `translate` did
/// before this migration.
struct ClaudeBashExitCodeSensor {
    adapter_version: &'static str,
}

impl EvidenceSensor for ClaudeBashExitCodeSensor {
    fn name(&self) -> &'static str {
        "claude_bash_exit_code_sensor_v1"
    }

    fn required_capabilities(&self) -> &'static [SignalClass] {
        &[SignalClass::ToolResultPayload]
    }

    fn trust_class(&self) -> TrustClass {
        // Claude Code's own tool_response is the provider's account of what
        // happened, not something Fornax measured itself.
        TrustClass::AgentAdjacent
    }

    // `caps` is intentionally unused: gating this sensor on
    // `ToolResultPayload` being confirmed `Available` would change which
    // sessions produce evidence today (a behavior change this migration
    // must not introduce). The real adapter only ever calls `collect` on a
    // live Claude Code PostToolUse event, where this capability is always
    // available in practice.
    fn collect(&self, event: &AgentEvent, _caps: &RuntimeCapabilities) -> SensorOutcome {
        if event.kind != EventKind::PostToolUse || event.tool_name.as_deref() != Some("Bash") {
            return SensorOutcome::not_collected(
                SignalAvailability::Unknown,
                Some("not a Bash PostToolUse event".to_string()),
            );
        }
        let Some(resp) = &event.tool_response else {
            return SensorOutcome::not_collected(
                SignalAvailability::Unavailable,
                Some("no tool_response present on this event".to_string()),
            );
        };

        // Field name is not stable across CC versions (see
        // docs/research/adapter-capability-matrix.md); check a small set of
        // plausible keys rather than assume one.
        let explicit_code = ["exit_code", "exitCode", "returncode", "status"]
            .iter()
            .find_map(|k| resp.get(k).and_then(|v| v.as_i64()));

        // Confirmed against a real Claude Code v2.1.238 transcript
        // (2026-08-29): the Bash tool_response never carries any of the
        // keys above — it is {stdout, stderr, interrupted, isImage,
        // noOutputExpected}. Fall back to a heuristic derived from that
        // shape so Evidence is still produced, and mark its provenance as
        // heuristic (not authoritative) rather than silently fabricating a
        // real exit code.
        let (code, provenance) = match explicit_code {
            Some(code) => (
                Some(code),
                format!(
                    "claude_code:{v}:PostToolUse:Bash#tool_response",
                    v = self.adapter_version
                ),
            ),
            None => {
                let interrupted = resp
                    .get("interrupted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let stderr_nonempty = resp
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                if interrupted {
                    (
                        Some(130),
                        format!(
                            "claude_code:{v}:PostToolUse:Bash#heuristic:interrupted",
                            v = self.adapter_version
                        ),
                    )
                } else if stderr_nonempty {
                    (
                        Some(1),
                        format!(
                            "claude_code:{v}:PostToolUse:Bash#heuristic:stderr_nonempty",
                            v = self.adapter_version
                        ),
                    )
                } else if resp.get("stdout").is_some() {
                    (
                        Some(0),
                        format!(
                            "claude_code:{v}:PostToolUse:Bash#heuristic:stderr_empty",
                            v = self.adapter_version
                        ),
                    )
                } else {
                    (None, String::new())
                }
            }
        };

        let Some(code) = code else {
            return SensorOutcome::not_collected(
                SignalAvailability::Unavailable,
                Some("no recognizable exit-code shape in tool_response".to_string()),
            );
        };

        SensorOutcome::collected(vec![Evidence {
            id: Uuid::new_v4(),
            session_id: event.session_id.clone(),
            source_event_id: event.id,
            kind: EvidenceKind::ExitCode,
            observed_at: event.observed_at.clone(),
            payload: serde_json::json!({
                "command": event
                    .tool_input
                    .as_ref()
                    .and_then(|ti| ti.get("command"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                "exit_code": code,
                "heuristic": explicit_code.is_none(),
            }),
            provenance,
            source: Some(EvidenceSource::now(
                self.name(),
                self.trust_class(),
                Some(Provider::ClaudeCode),
            )),
            extension: None,
        }])
    }
}

fn translate(
    adapter: &ClaudeAdapter,
    session_hint: &str,
    raw: &serde_json::Value,
) -> NormalizationOutcome {
    let hook_event = raw
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = raw
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| session_hint.to_string());
    let now = chrono::Utc::now().to_rfc3339();

    let kind = match hook_event {
        "PreToolUse" => EventKind::PreToolUse,
        "PostToolUse" => EventKind::PostToolUse,
        "SessionStart" => EventKind::SessionStart,
        "Stop" => EventKind::SessionEnd,
        "UserPromptSubmit" => EventKind::UserPromptSubmit,
        "SubagentStart" => EventKind::SubagentStart,
        "SubagentStop" => EventKind::SubagentStop,
        "Notification" => EventKind::Notification,
        "" => {
            return NormalizationOutcome::Unrecognized {
                discriminator: "<missing hook_event_name>".to_string(),
            }
        }
        other => {
            // A hook event name this adapter has no canonical mapping for.
            // Never seen live yet, so treated as genuinely unrecognized
            // (not a deliberate `Ignored`) — see the trait docs.
            return NormalizationOutcome::Unrecognized {
                discriminator: other.to_string(),
            };
        }
    };

    let event_id = Uuid::new_v4();
    let tool_name = raw
        .get("tool_name")
        .and_then(|v| v.as_str())
        .map(String::from);
    let tool_input = raw.get("tool_input").cloned();
    let tool_response = raw.get("tool_response").cloned();

    let event = AgentEvent {
        id: event_id,
        session_id: session_id.clone(),
        provider: Provider::ClaudeCode,
        kind,
        observed_at: now.clone(),
        tool_name: tool_name.clone(),
        tool_input,
        tool_response: tool_response.clone(),
        raw: raw.clone(),
    };

    // Declare capabilities on every event, not just session start: Claude
    // Code hooks are stateless invocations of this binary, so there is no
    // single "session start" moment where a capability declaration is
    // guaranteed to be sent exactly once. The daemon's Capabilities handler
    // overwrites its per-session map entry, so repeated identical
    // declarations are idempotent. This was a real gap found 2026-08-29
    // while proving FORNX-34 against live Claude Code data: without it,
    // the daemon never learns this session can expose exit-code evidence,
    // and every claim resolves Unavailable regardless of Evidence present.
    let caps = stamped_capabilities(adapter, &session_id);
    let mut out = vec![
        IngestMessage::Capabilities(caps.clone()),
        IngestMessage::Event(event.clone()),
    ];

    // PostToolUse for a Bash call: if Claude Code's tool_response carries an
    // exit-code-shaped field, extract it as Evidence. Formalized (FORNX-157)
    // as a `ClaudeBashExitCodeSensor` implementing `EvidenceSensor` — see
    // that type for the unchanged heuristic (proven by the `tests` module's
    // existing exit-code tests, whose assertions were not touched by this
    // change).
    if kind == EventKind::PostToolUse && tool_name.as_deref() == Some("Bash") {
        let sensor = ClaudeBashExitCodeSensor {
            adapter_version: adapter.adapter_version(),
        };
        let outcome = sensor.collect(&event, &caps);
        out.extend(outcome.evidence.into_iter().map(IngestMessage::Evidence));
    }

    // Stop: best-effort claim extraction from the transcript's last
    // assistant message, if Claude Code gave us a transcript_path.
    if kind == EventKind::SessionEnd {
        if let Some(text) = last_assistant_text(raw) {
            if fornax_verify_claims_tests_passed(&text) {
                out.push(IngestMessage::Claim(Claim {
                    id: Uuid::new_v4(),
                    session_id,
                    source_event_id: event_id,
                    text,
                    subject: "test_result".to_string(),
                    claimed_at: now,
                }));
            }
        }
    }

    NormalizationOutcome::Messages(out)
}

/// Duplicated (not imported) on purpose: adapters must not depend on
/// fornax-verify (that would blur the "adapters are thin, verifiers own
/// domain logic" boundary — see the `AgentAdapter` trait docs' "Allowed core
/// dependencies" section) — this is only a cheap pre-filter so the daemon
/// isn't sent every Stop-event message as a candidate claim. The daemon's
/// verifier is the actual authority.
fn fornax_verify_claims_tests_passed(text: &str) -> bool {
    let t = text.to_lowercase();
    (t.contains("test") || t.contains("tests"))
        && (t.contains("passed") || t.contains("succeeded") || t.contains("all green"))
        && !t.contains("failed")
}

fn last_assistant_text(raw: &serde_json::Value) -> Option<String> {
    let path = raw.get("transcript_path").and_then(|v| v.as_str())?;
    let content = std::fs::read_to_string(path).ok()?;
    let mut last_text: Option<String> = None;
    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        // A real assistant turn's content array frequently has a tool_use
        // (or thinking) block at index 0 with no "text" field at all —
        // confirmed against a live Claude Code v2.1.238 transcript
        // 2026-08-29, where content[0] was a tool_use block. Scan every
        // block in the turn for a "text"-typed one instead of assuming
        // index 0.
        if let Some(blocks) = entry.pointer("/message/content").and_then(|v| v.as_array()) {
            for block in blocks {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        last_text = Some(text.to_string());
                    }
                }
            }
        }
    }
    last_text
}

#[cfg(test)]
mod tests {
    use super::*;
    use fornax_types::{EventKind, IngestMessage, Provider};

    fn normalize(raw: &serde_json::Value) -> NormalizationOutcome {
        ClaudeAdapter.normalize("unused-hint", raw)
    }

    /// FORNX-155 AC4: the real capabilities this adapter sends, projected
    /// through the legacy wire shape (what `fornax-cli export-spool`
    /// actually emits), must reproduce the exact six bool values this
    /// adapter declared before the formalization — not just a hand-built
    /// fixture that happens to agree.
    #[test]
    fn claude_capabilities_legacy_projection_matches_pre_formalization_bools() {
        let legacy = fornax_types::LegacyCapabilitiesWire::from(&ClaudeAdapter.probe());
        assert!(legacy.supports_pre_tool_use);
        assert!(legacy.supports_post_tool_use);
        assert!(legacy.supports_tool_response_capture);
        assert!(legacy.supports_session_stop_event);
        assert!(legacy.supports_transcript_tail);
        assert!(legacy.supports_subagent_lifecycle);
    }

    #[test]
    fn post_tool_use_bash_with_exit_code_produces_event_and_evidence() {
        let raw = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "pytest"},
            "tool_response": {"exit_code": 1}
        });
        let msgs = normalize(&raw).into_messages();
        assert_eq!(msgs.len(), 3);
        assert!(matches!(&msgs[0], IngestMessage::Capabilities(_)));
        match &msgs[1] {
            IngestMessage::Event(e) => {
                assert_eq!(e.provider, Provider::ClaudeCode);
                assert_eq!(e.kind, EventKind::PostToolUse);
                assert_eq!(e.session_id, "sess-1");
            }
            other => panic!("expected Event, got {other:?}"),
        }
        match &msgs[2] {
            IngestMessage::Evidence(ev) => {
                assert_eq!(ev.payload["exit_code"], 1);
            }
            other => panic!("expected Evidence, got {other:?}"),
        }
    }

    /// FORNX-157: proves the exit-code evidence path now also carries
    /// structured `EvidenceSource`/trust-class metadata, on top of the
    /// unmodified provenance/payload assertions above (which are the
    /// before/after behavior-preservation proof for the migration onto
    /// `ClaudeBashExitCodeSensor`).
    #[test]
    fn post_tool_use_bash_evidence_carries_sensor_source_metadata() {
        let raw = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "pytest"},
            "tool_response": {"exit_code": 1}
        });
        let msgs = normalize(&raw).into_messages();
        match &msgs[2] {
            IngestMessage::Evidence(ev) => {
                let source = ev
                    .source
                    .as_ref()
                    .expect("sensor-produced evidence must carry source");
                assert_eq!(source.sensor_name, "claude_bash_exit_code_sensor_v1");
                assert_eq!(source.trust_class, fornax_types::TrustClass::AgentAdjacent);
                assert_eq!(source.provider, Some(Provider::ClaudeCode));
            }
            other => panic!("expected Evidence, got {other:?}"),
        }
    }

    /// FORNX-157: the sensor is directly unit-testable in isolation from
    /// `normalize()`'s hook-JSON plumbing, given only a canonical
    /// `AgentEvent` — proving the "adapters consume canonical types, not
    /// raw transport" boundary holds on the collection side too.
    #[test]
    fn claude_bash_exit_code_sensor_reports_unavailable_with_no_tool_response() {
        let event = AgentEvent {
            id: Uuid::new_v4(),
            session_id: "sess-1".into(),
            provider: Provider::ClaudeCode,
            kind: EventKind::PostToolUse,
            observed_at: "2026-01-01T00:00:00Z".into(),
            tool_name: Some("Bash".into()),
            tool_input: None,
            tool_response: None,
            raw: serde_json::json!({}),
        };
        let sensor = ClaudeBashExitCodeSensor {
            adapter_version: ADAPTER_VERSION,
        };
        let outcome = sensor.collect(&event, &ClaudeAdapter.probe());
        assert!(!outcome.has_evidence());
        assert_eq!(outcome.state, SignalAvailability::Unavailable);
    }

    #[test]
    fn post_tool_use_real_claude_code_shape_infers_heuristic_success() {
        // Confirmed real Claude Code v2.1.238 Bash tool_response shape
        // (2026-08-29 live capture): no exit_code/exitCode/returncode/status
        // key exists at all. Empty stderr + not interrupted should still
        // yield heuristic exit-code-0 Evidence, marked as a heuristic.
        let raw = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
            "tool_response": {"stdout": "hi\n", "stderr": "", "interrupted": false, "isImage": false, "noOutputExpected": false}
        });
        let msgs = normalize(&raw).into_messages();
        assert_eq!(msgs.len(), 3);
        match &msgs[2] {
            IngestMessage::Evidence(ev) => {
                assert_eq!(ev.payload["exit_code"], 0);
                assert_eq!(ev.payload["heuristic"], true);
                assert!(ev.provenance.contains("heuristic:stderr_empty"));
            }
            _ => panic!("expected Evidence"),
        }
    }

    #[test]
    fn post_tool_use_real_shape_stderr_nonempty_infers_heuristic_failure() {
        let raw = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "false"},
            "tool_response": {"stdout": "", "stderr": "boom", "interrupted": false}
        });
        let msgs = normalize(&raw).into_messages();
        assert_eq!(msgs.len(), 3);
        match &msgs[2] {
            IngestMessage::Evidence(ev) => {
                assert_eq!(ev.payload["exit_code"], 1);
                assert!(ev.provenance.contains("heuristic:stderr_nonempty"));
            }
            _ => panic!("expected Evidence"),
        }
    }

    #[test]
    fn post_tool_use_without_any_recognizable_shape_produces_only_event() {
        let raw = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
            "tool_response": {}
        });
        let msgs = normalize(&raw).into_messages();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], IngestMessage::Capabilities(_)));
        assert!(matches!(&msgs[1], IngestMessage::Event(_)));
    }

    #[test]
    fn stop_event_finds_text_block_when_content_0_is_tool_use() {
        // Confirmed real Claude Code v2.1.238 transcript shape (2026-08-29
        // live capture): an assistant turn's content[0] is routinely a
        // tool_use block with no "text" field — the final text-bearing
        // block can be anywhere in the array, not just index 0.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("fornax-test-transcript-{}.jsonl", Uuid::new_v4()));
        let transcript = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}},
                    {"type": "text", "text": "all tests passed"}
                ]
            }
        })
        .to_string();
        std::fs::write(&path, transcript).unwrap();

        let raw = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "sess-1",
            "transcript_path": path.to_str().unwrap()
        });
        let msgs = normalize(&raw).into_messages();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 3);
        match &msgs[2] {
            IngestMessage::Claim(c) => assert_eq!(c.text, "all tests passed"),
            _ => panic!("expected Claim"),
        }
    }

    #[test]
    fn stop_event_without_transcript_path_produces_only_event() {
        let raw = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "sess-1"
        });
        let msgs = normalize(&raw).into_messages();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], IngestMessage::Capabilities(_)));
        assert!(matches!(&msgs[1], IngestMessage::Event(_)));
    }

    #[test]
    fn unknown_hook_event_is_unrecognized_not_a_crash() {
        let raw = serde_json::json!({
            "hook_event_name": "SomethingClaudeCodeAddsLater",
            "session_id": "sess-1"
        });
        match normalize(&raw) {
            NormalizationOutcome::Unrecognized { discriminator } => {
                assert_eq!(discriminator, "SomethingClaudeCodeAddsLater")
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn missing_hook_event_name_is_unrecognized_not_a_crash() {
        let raw = serde_json::json!({"session_id": "sess-1"});
        match normalize(&raw) {
            NormalizationOutcome::Unrecognized { .. } => {}
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn user_prompt_submit_produces_one_event() {
        let raw = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-1",
            "prompt": "run the tests"
        });
        let msgs = normalize(&raw).into_messages();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], IngestMessage::Capabilities(_)));
        assert!(
            matches!(&msgs[1], IngestMessage::Event(e) if e.kind == EventKind::UserPromptSubmit)
        );
    }

    #[test]
    fn capabilities_carry_adapter_version_and_session_id_notes() {
        let raw = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "sess-1"
        });
        let msgs = normalize(&raw).into_messages();
        match &msgs[0] {
            IngestMessage::Capabilities(caps) => {
                assert_eq!(
                    caps.notes.get("adapter_version").map(String::as_str),
                    Some(ADAPTER_VERSION)
                );
                assert_eq!(
                    caps.notes.get("session_id").map(String::as_str),
                    Some("sess-1")
                );
            }
            other => panic!("expected Capabilities, got {other:?}"),
        }
    }
}
