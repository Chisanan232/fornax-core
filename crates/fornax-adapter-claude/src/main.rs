//! Claude Code hook adapter (FORNX-28). Invoked as a hook command; reads the
//! hook's stdin JSON, translates it into canonical `IngestMessage`s, and
//! forwards them to the daemon over the Unix Domain Socket. Thin by design
//! (D5, ADR 0001): no verification logic here.
//!
//! Wire into `~/.claude/settings.json` (not done automatically — this is the
//! user's global config):
//! ```json
//! "PreToolUse":  [{ "hooks": [{ "type": "command", "command": "fornax-hook-claude" }] }],
//! "PostToolUse": [{ "hooks": [{ "type": "command", "command": "fornax-hook-claude" }] }],
//! "Stop":        [{ "hooks": [{ "type": "command", "command": "fornax-hook-claude" }] }],
//! "SessionStart":[{ "hooks": [{ "type": "command", "command": "fornax-hook-claude" }] }]
//! ```

use fornax_types::{AgentEvent, Claim, EventKind, Evidence, EvidenceKind, IngestMessage, Provider};
use std::io::Read;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use uuid::Uuid;

fn sock_path() -> std::path::PathBuf {
    let home = std::env::var("FORNAX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".fornax")
        });
    home.join("fornax.sock")
}

#[tokio::main]
async fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() || input.trim().is_empty() {
        return; // No stdin payload — nothing to report, exit 0 quietly.
    }
    let raw: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return,
    };

    let messages = translate(&raw);
    if messages.is_empty() {
        return;
    }

    // Best-effort: a daemon that isn't running must never block/fail the
    // agent's own turn. Fire-and-forget, swallow connection errors.
    if let Ok(mut stream) = UnixStream::connect(sock_path()).await {
        for msg in messages {
            if let Ok(mut line) = serde_json::to_string(&msg) {
                line.push('\n');
                let _ = stream.write_all(line.as_bytes()).await;
            }
        }
    }
}

/// Declared conservatively, matching what this adapter actually reads —
/// never inferred as more capable than confirmed (D5, ADR 0001). Confirmed
/// against a live Claude Code v2.1.238 session (2026-08-29): PostToolUse and
/// Stop both fire with the shapes this adapter parses.
///
/// Formalized (FORNX-155) from six fixed bools into an explicit
/// `fornax_types::SignalClass` -> `SignalAvailability` declaration. Every
/// class this adapter previously declared `true` for stays `Available`;
/// `ProcessResult` is new — Claude Code's Bash `tool_response` never carries
/// a literal exit code, only a heuristic derived from
/// stdout/stderr/interrupted (see the `PostToolUse` handling in `translate`
/// below), so it is declared `Unsupported`, not `Available`.
fn claude_capabilities(session_id: &str) -> fornax_types::RuntimeCapabilities {
    use fornax_types::{CapabilitySignal, SignalAvailability, SignalClass};
    fornax_types::RuntimeCapabilities {
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
        notes: [
            // Reserved, machine-consumed transport field — see the doc
            // comment on `RuntimeCapabilities::notes` in
            // `fornax-types/src/capabilities.rs`. Not free-form.
            ("session_id".to_string(), session_id.to_string()),
            (
                "exit_code".to_string(),
                "heuristic from stdout/stderr/interrupted — Claude Code's Bash tool_response \
                 carries no literal exit code as of v2.1.238"
                    .to_string(),
            ),
        ]
        .into(),
    }
}

fn translate(raw: &serde_json::Value) -> Vec<IngestMessage> {
    let hook_event = raw
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = raw
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
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
        _ => return vec![],
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
    let mut out = vec![
        IngestMessage::Capabilities(claude_capabilities(&session_id)),
        IngestMessage::Event(event),
    ];

    // PostToolUse for a Bash call: if Claude Code's tool_response carries an
    // exit-code-shaped field, extract it as Evidence. Field name is not
    // stable across CC versions (see docs/research/adapter-capability-matrix.md);
    // check a small set of plausible keys rather than assume one.
    if kind == EventKind::PostToolUse && tool_name.as_deref() == Some("Bash") {
        if let Some(resp) = &tool_response {
            let explicit_code = ["exit_code", "exitCode", "returncode", "status"]
                .iter()
                .find_map(|k| resp.get(k).and_then(|v| v.as_i64()));

            // Confirmed against a real Claude Code v2.1.238 transcript
            // (2026-08-29): the Bash tool_response never carries any of the
            // keys above — it is {stdout, stderr, interrupted, isImage,
            // noOutputExpected}. Fall back to a heuristic derived from that
            // shape so Evidence is still produced, and mark its provenance
            // as heuristic (not authoritative) rather than silently
            // fabricating a real exit code.
            let (code, provenance) = match explicit_code {
                Some(code) => (
                    Some(code),
                    "claude_code:PostToolUse:Bash#tool_response".to_string(),
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
                            "claude_code:PostToolUse:Bash#heuristic:interrupted".to_string(),
                        )
                    } else if stderr_nonempty {
                        (
                            Some(1),
                            "claude_code:PostToolUse:Bash#heuristic:stderr_nonempty".to_string(),
                        )
                    } else if resp.get("stdout").is_some() {
                        (
                            Some(0),
                            "claude_code:PostToolUse:Bash#heuristic:stderr_empty".to_string(),
                        )
                    } else {
                        (None, String::new())
                    }
                }
            };

            if let Some(code) = code {
                out.push(IngestMessage::Evidence(Evidence {
                    id: Uuid::new_v4(),
                    session_id: session_id.clone(),
                    source_event_id: event_id,
                    kind: EvidenceKind::ExitCode,
                    observed_at: now.clone(),
                    payload: serde_json::json!({
                        "command": tool_input_command(raw),
                        "exit_code": code,
                        "heuristic": explicit_code.is_none(),
                    }),
                    provenance,
                }));
            }
        }
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

    out
}

/// Duplicated (not imported) on purpose: adapters must not depend on
/// fornax-verify (that would blur the "adapters are thin, verifiers own
/// domain logic" boundary) — this is only a cheap pre-filter so the daemon
/// isn't sent every Stop-event message as a candidate claim. The daemon's
/// verifier is the actual authority.
fn fornax_verify_claims_tests_passed(text: &str) -> bool {
    let t = text.to_lowercase();
    (t.contains("test") || t.contains("tests"))
        && (t.contains("passed") || t.contains("succeeded") || t.contains("all green"))
        && !t.contains("failed")
}

fn tool_input_command(raw: &serde_json::Value) -> serde_json::Value {
    raw.get("tool_input")
        .and_then(|ti| ti.get("command"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
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

    /// FORNX-155 AC4: the real `claude_capabilities()` this adapter sends,
    /// projected through the legacy wire shape (what `fornax-cli
    /// export-spool` actually emits), must reproduce the exact six bool
    /// values this adapter declared before the formalization — not just a
    /// hand-built fixture that happens to agree.
    #[test]
    fn claude_capabilities_legacy_projection_matches_pre_formalization_bools() {
        let legacy = fornax_types::LegacyCapabilitiesWire::from(&claude_capabilities("sess-1"));
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
        let msgs = translate(&raw);
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
        let msgs = translate(&raw);
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
        let msgs = translate(&raw);
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
        let msgs = translate(&raw);
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
        let msgs = translate(&raw);
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
        let msgs = translate(&raw);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], IngestMessage::Capabilities(_)));
        assert!(matches!(&msgs[1], IngestMessage::Event(_)));
    }

    #[test]
    fn unknown_hook_event_produces_nothing() {
        let raw = serde_json::json!({
            "hook_event_name": "SomethingClaudeCodeAddsLater",
            "session_id": "sess-1"
        });
        assert!(translate(&raw).is_empty());
    }

    #[test]
    fn user_prompt_submit_produces_one_event() {
        let raw = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-1",
            "prompt": "run the tests"
        });
        let msgs = translate(&raw);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], IngestMessage::Capabilities(_)));
        assert!(
            matches!(&msgs[1], IngestMessage::Event(e) if e.kind == EventKind::UserPromptSubmit)
        );
    }
}
