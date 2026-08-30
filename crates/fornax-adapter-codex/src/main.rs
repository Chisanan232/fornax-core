//! Codex CLI adapter (FORNX-29). Codex's hook system is opt-in/unstable and
//! admin-suppressible (docs/research/adapter-capability-matrix.md), so the
//! primary integration point here is tailing the always-on rollout JSONL
//! transcript at `~/.codex/sessions/**/*.jsonl`, not hooks. Thin adapter:
//! translates confirmed `RolloutLine` shapes into canonical `IngestMessage`s.
//!
//! Usage: `fornax-hook-codex [--file <rollout.jsonl>]` (defaults to the most
//! recently modified rollout file), runs until interrupted, tailing new lines.

use fornax_types::{
    AgentEvent, Claim, EventKind, Evidence, EvidenceKind, IngestMessage, Provider,
    RuntimeCapabilities,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use uuid::Uuid;

fn sock_path() -> PathBuf {
    let home = std::env::var("FORNAX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".fornax")
        });
    home.join("fornax.sock")
}

/// Codex's own hooks and PreToolUse input-rewrite are not usable today
/// (opt-in feature flag, admin-suppressible, no input rewrite — see the
/// capability matrix doc). Declared conservatively; never inferred as more
/// capable than confirmed.
///
/// Formalized (FORNX-155) from six fixed bools into an explicit
/// `fornax_types::SignalClass` -> `SignalAvailability` declaration.
/// `ToolInvocation` and `SubagentLifecycle` are `Unsupported`, not merely
/// absent: this adapter's chosen mechanism (rollout-tail, not Codex's
/// opt-in/admin-suppressible hooks) fundamentally cannot expose pre-execution
/// interception or subagent lifecycle events, which is a stronger, more
/// useful claim than ordinary absence. `ProcessResult` is new — no literal
/// exit code is exposed by the shapes this adapter parses today, so it is
/// `Unavailable` (the class exists for Codex in principle via
/// `exec_command_end.exit_code`, confirmed in the capability matrix doc; this
/// adapter's primary rollout-tail path just hasn't observed it — see
/// `translate_line`'s `custom_tool_call_output` handling).
fn codex_capabilities() -> RuntimeCapabilities {
    use fornax_types::{CapabilitySignal, SignalAvailability, SignalClass};
    RuntimeCapabilities {
        schema_version: fornax_types::CAPABILITY_SCHEMA_VERSION,
        provider: Provider::Codex,
        signals: vec![
            CapabilitySignal {
                class: SignalClass::ToolInvocation,
                state: SignalAvailability::Unsupported,
                detail: Some(
                    "Codex hooks (the only pre-execution interception mechanism) are \
                     opt-in and admin-suppressible, with no input-rewrite support \
                     (see docs/research/adapter-capability-matrix.md)"
                        .to_string(),
                ),
            },
            CapabilitySignal {
                class: SignalClass::ToolTrace,
                state: SignalAvailability::Available,
                detail: Some("via rollout custom_tool_call/_output pairing, not hooks".to_string()),
            },
            CapabilitySignal {
                class: SignalClass::ToolResultPayload,
                state: SignalAvailability::Available,
                detail: None,
            },
            CapabilitySignal {
                class: SignalClass::SessionLifecycle,
                state: SignalAvailability::Available,
                detail: Some("task_complete in rollout".to_string()),
            },
            CapabilitySignal {
                class: SignalClass::FinalResponse,
                state: SignalAvailability::Available,
                detail: None,
            },
            CapabilitySignal {
                class: SignalClass::SubagentLifecycle,
                state: SignalAvailability::Unsupported,
                detail: Some(
                    "this adapter's rollout-tail mechanism surfaces no subagent lines; \
                     Codex's own SubagentStart/Stop hooks exist but are opt-in/unstable/ \
                     admin-suppressible, same as ToolInvocation above"
                        .to_string(),
                ),
            },
            CapabilitySignal {
                class: SignalClass::ProcessResult,
                state: SignalAvailability::Unavailable,
                detail: Some(
                    "exec_command_end (literal exit_code) not emitted by codex-cli 0.147.0; \
                     only the heuristic script-completed marker via \
                     custom_tool_call_output is observed today"
                        .to_string(),
                ),
            },
        ],
        notes: [(
            "mechanism".to_string(),
            "rollout JSONL tail, not Codex hooks (opt-in/unstable, see adapter-capability-matrix.md)".to_string(),
        )]
        .into(),
    }
}

fn default_rollout_file() -> Option<PathBuf> {
    let sessions_dir = PathBuf::from(std::env::var("HOME").ok()?).join(".codex/sessions");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in walk_jsonl(&sessions_dir) {
        if let Ok(meta) = std::fs::metadata(&entry) {
            if let Ok(modified) = meta.modified() {
                if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                    newest = Some((modified, entry));
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

fn walk_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_jsonl(&path));
        } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            out.push(path);
        }
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let file = args
        .iter()
        .position(|a| a == "--file")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(default_rollout_file);

    let Some(file) = file else {
        eprintln!("fornax-hook-codex: no rollout file found under ~/.codex/sessions");
        return Ok(());
    };
    eprintln!("fornax-hook-codex: tailing {}", file.display());

    let mut stream: Option<UnixStream> = None;
    let mut session_id: Option<String> = None;
    let mut caps_sent = false;
    let mut offset: u64 = 0;
    // call_id -> best-effort command text, so a later custom_tool_call_output
    // can be paired back to the exec it answers (FORNX-55).
    let mut pending_calls: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    loop {
        if stream.is_none() {
            stream = UnixStream::connect(sock_path()).await.ok();
        }

        let content = tokio::fs::read_to_string(&file).await.unwrap_or_default();
        if (content.len() as u64) > offset {
            let new_part = &content[offset as usize..];
            for line in new_part.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if session_id.is_none() {
                    if let Some(sid) = entry
                        .pointer("/payload/session_id")
                        .and_then(|v| v.as_str())
                    {
                        session_id = Some(sid.to_string());
                    }
                }
                let sid = session_id
                    .clone()
                    .unwrap_or_else(|| file.display().to_string());

                let Some(s) = stream.as_mut() else { continue };

                if !caps_sent {
                    let mut caps = codex_capabilities();
                    caps.notes.insert("session_id".to_string(), sid.clone());
                    send(s, &IngestMessage::Capabilities(caps)).await;
                    caps_sent = true;
                }

                if let Some(msgs) = translate_line(&entry, &sid, &mut pending_calls) {
                    for m in msgs {
                        send(s, &m).await;
                    }
                }
            }
            offset = content.len() as u64;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn send(stream: &mut UnixStream, msg: &IngestMessage) {
    if let Ok(mut line) = serde_json::to_string(msg) {
        line.push('\n');
        let _ = stream.write_all(line.as_bytes()).await;
    }
}

fn translate_line(
    entry: &serde_json::Value,
    session_id: &str,
    pending_calls: &mut std::collections::HashMap<String, String>,
) -> Option<Vec<IngestMessage>> {
    let top_type = entry.get("type").and_then(|v| v.as_str())?;

    // Confirmed against a real `codex exec` turn (codex-cli 0.147.0,
    // 2026-08-29 live capture, FORNX-55): this installed version never
    // emits `event_msg{type:"exec_command_end"}` at all — shell execution
    // is wrapped as a `response_item` pair, `custom_tool_call` (the
    // invocation) followed later by `custom_tool_call_output` (the
    // result), matched by `call_id`. Handled here alongside the
    // `event_msg` path rather than assuming only one wire shape exists.
    if top_type == "response_item" {
        let payload = entry.get("payload")?;
        let sub_type = payload.get("type").and_then(|v| v.as_str())?;
        return match sub_type {
            "custom_tool_call" if payload.get("name").and_then(|v| v.as_str()) == Some("exec") => {
                if let (Some(call_id), Some(input)) = (
                    payload.get("call_id").and_then(|v| v.as_str()),
                    payload.get("input").and_then(|v| v.as_str()),
                ) {
                    pending_calls.insert(call_id.to_string(), extract_cmd(input));
                }
                None
            }
            "custom_tool_call_output" => {
                let call_id = payload.get("call_id").and_then(|v| v.as_str())?;
                let command = pending_calls.remove(call_id).unwrap_or_default();
                let output_text: String = payload
                    .get("output")
                    .and_then(|v| v.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();

                let now = chrono::Utc::now().to_rfc3339();
                let event_id = Uuid::new_v4();
                let event = AgentEvent {
                    id: event_id,
                    session_id: session_id.to_string(),
                    provider: Provider::Codex,
                    kind: EventKind::PostToolUse,
                    observed_at: now.clone(),
                    tool_name: Some("exec_command".to_string()),
                    tool_input: Some(serde_json::json!({"command": command})),
                    tool_response: Some(payload.clone()),
                    raw: entry.clone(),
                };
                let mut out = vec![IngestMessage::Event(event)];

                // No literal exit code is exposed in this shape at all
                // (unlike exec_command_end, which at least had the field
                // even if this adapter's earlier guess for it was moot).
                // "Script completed" is the only confirmed real success
                // marker observed so far; a real failing-command capture
                // to confirm the failure-path marker is still outstanding
                // (tracked as residual FORNX-55 follow-up) — so an
                // unrecognized shape produces no Evidence rather than a
                // guessed verdict in either direction.
                if output_text.contains("Script completed") {
                    out.push(IngestMessage::Evidence(Evidence {
                        id: Uuid::new_v4(),
                        session_id: session_id.to_string(),
                        source_event_id: event_id,
                        kind: EvidenceKind::ExitCode,
                        observed_at: now,
                        payload: serde_json::json!({
                            "command": command,
                            "exit_code": 0,
                            "heuristic": true,
                        }),
                        provenance:
                            "codex:rollout:custom_tool_call_output#heuristic:script_completed"
                                .to_string(),
                    }));
                }
                Some(out)
            }
            _ => None,
        };
    }

    if top_type != "event_msg" {
        return None;
    }
    let payload = entry.get("payload")?;
    let sub_type = payload.get("type").and_then(|v| v.as_str())?;
    let now = chrono::Utc::now().to_rfc3339();
    let event_id = Uuid::new_v4();

    match sub_type {
        "exec_command_end" => {
            let event = AgentEvent {
                id: event_id,
                session_id: session_id.to_string(),
                provider: Provider::Codex,
                kind: EventKind::PostToolUse,
                observed_at: now.clone(),
                tool_name: Some("exec_command".to_string()),
                tool_input: payload.get("command").cloned(),
                tool_response: Some(payload.clone()),
                raw: entry.clone(),
            };
            let mut out = vec![IngestMessage::Event(event)];
            if let Some(code) = payload.get("exit_code").and_then(|v| v.as_i64()) {
                out.push(IngestMessage::Evidence(Evidence {
                    id: Uuid::new_v4(),
                    session_id: session_id.to_string(),
                    source_event_id: event_id,
                    kind: EvidenceKind::ExitCode,
                    observed_at: now,
                    payload: serde_json::json!({
                        "command": payload.get("command").cloned().unwrap_or_default(),
                        "exit_code": code,
                    }),
                    provenance: "codex:rollout:exec_command_end".to_string(),
                }));
            }
            Some(out)
        }
        "task_complete" => {
            let event = AgentEvent {
                id: event_id,
                session_id: session_id.to_string(),
                provider: Provider::Codex,
                kind: EventKind::SessionEnd,
                observed_at: now.clone(),
                tool_name: None,
                tool_input: None,
                tool_response: None,
                raw: entry.clone(),
            };
            let mut out = vec![IngestMessage::Event(event)];
            if let Some(text) = payload.get("last_agent_message").and_then(|v| v.as_str()) {
                if claims_tests_passed(text) {
                    out.push(IngestMessage::Claim(Claim {
                        id: Uuid::new_v4(),
                        session_id: session_id.to_string(),
                        source_event_id: event_id,
                        text: text.to_string(),
                        subject: "test_result".to_string(),
                        claimed_at: now,
                    }));
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Best-effort extraction of the shell command from a `custom_tool_call`'s
/// `input`, which is a JS snippet string like
/// `const r = await tools.exec_command({cmd:"echo hi",workdir:...}); ...`
/// — not structured JSON. No regex dependency; a small manual scan for the
/// `cmd:"..."` argument is enough for provenance purposes. Falls back to
/// the raw input string if the pattern isn't found, rather than dropping
/// the command entirely.
fn extract_cmd(input: &str) -> String {
    if let Some(start) = input.find("cmd:") {
        let rest = &input[start + 4..];
        let rest = rest.trim_start();
        if let Some(quote) = rest.chars().next() {
            if quote == '"' || quote == '\'' {
                let body = &rest[1..];
                let mut result = String::new();
                let mut chars = body.chars();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        if let Some(next) = chars.next() {
                            result.push(next);
                        }
                        continue;
                    }
                    if c == quote {
                        return result;
                    }
                    result.push(c);
                }
            }
        }
    }
    input.to_string()
}

fn claims_tests_passed(text: &str) -> bool {
    let t = text.to_lowercase();
    (t.contains("test") || t.contains("tests"))
        && (t.contains("passed") || t.contains("succeeded") || t.contains("all green"))
        && !t.contains("failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FORNX-155 AC4: the real `codex_capabilities()` this adapter sends,
    /// projected through the legacy wire shape (what `fornax-cli
    /// export-spool` actually emits), must reproduce the exact six bool
    /// values this adapter declared before the formalization — not just a
    /// hand-built fixture that happens to agree.
    #[test]
    fn codex_capabilities_legacy_projection_matches_pre_formalization_bools() {
        let legacy = fornax_types::LegacyCapabilitiesWire::from(&codex_capabilities());
        assert!(!legacy.supports_pre_tool_use);
        assert!(legacy.supports_post_tool_use);
        assert!(legacy.supports_tool_response_capture);
        assert!(legacy.supports_session_stop_event);
        assert!(legacy.supports_transcript_tail);
        assert!(!legacy.supports_subagent_lifecycle);
    }

    #[test]
    fn exec_command_end_with_nonzero_exit_produces_event_and_evidence() {
        let entry = serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "exec_command_end",
                "command": ["pytest"],
                "exit_code": 1,
                "aggregated_output": "7 failed, 1 passed"
            }
        });
        let msgs = translate_line(&entry, "sess-1", &mut std::collections::HashMap::new())
            .expect("should translate");
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            IngestMessage::Event(e) => {
                assert_eq!(e.provider, Provider::Codex);
                assert_eq!(e.kind, EventKind::PostToolUse);
            }
            other => panic!("expected Event, got {other:?}"),
        }
        match &msgs[1] {
            IngestMessage::Evidence(ev) => {
                assert_eq!(ev.kind, EvidenceKind::ExitCode);
                assert_eq!(ev.payload["exit_code"], 1);
            }
            other => panic!("expected Evidence, got {other:?}"),
        }
    }

    #[test]
    fn exec_command_end_without_exit_code_produces_only_event() {
        let entry = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "exec_command_end", "command": ["pwd"]}
        });
        let msgs = translate_line(&entry, "sess-1", &mut std::collections::HashMap::new())
            .expect("should translate");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn task_complete_with_passing_claim_produces_event_and_claim() {
        let entry = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "last_agent_message": "All tests passed."}
        });
        let msgs = translate_line(&entry, "sess-1", &mut std::collections::HashMap::new())
            .expect("should translate");
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[1], IngestMessage::Claim(c) if c.subject == "test_result"));
    }

    #[test]
    fn task_complete_without_a_claim_producing_message_is_event_only() {
        let entry = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "last_agent_message": "Done, see the diff."}
        });
        let msgs = translate_line(&entry, "sess-1", &mut std::collections::HashMap::new())
            .expect("should translate");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn non_event_msg_lines_are_ignored() {
        let entry = serde_json::json!({"type": "session_meta", "payload": {"session_id": "s"}});
        assert!(translate_line(&entry, "sess-1", &mut std::collections::HashMap::new()).is_none());
    }

    #[test]
    fn unknown_event_msg_subtype_is_ignored() {
        let entry = serde_json::json!({"type": "event_msg", "payload": {"type": "token_count"}});
        assert!(translate_line(&entry, "sess-1", &mut std::collections::HashMap::new()).is_none());
    }

    #[test]
    fn custom_tool_call_then_output_correlate_into_heuristic_evidence() {
        // Confirmed real codex-cli 0.147.0 shapes (2026-08-29 live capture,
        // FORNX-55): shell exec has no exec_command_end event at all; it is
        // this response_item pair matched by call_id.
        let mut pending = std::collections::HashMap::new();
        let call = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "call_id": "call_1",
                "name": "exec",
                "input": "const r = await tools.exec_command({cmd:\"echo hi\",workdir:\"/tmp\"}); text(r.output);\n"
            }
        });
        assert!(translate_line(&call, "sess-1", &mut pending).is_none());
        assert_eq!(pending.get("call_1").map(String::as_str), Some("echo hi"));

        let output = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call_1",
                "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
                    {"type": "input_text", "text": "hi\n"}
                ]
            }
        });
        let msgs = translate_line(&output, "sess-1", &mut pending).expect("should translate");
        assert_eq!(msgs.len(), 2);
        assert!(pending.is_empty(), "call_id should be consumed");
        match &msgs[1] {
            IngestMessage::Evidence(ev) => {
                assert_eq!(ev.payload["exit_code"], 0);
                assert_eq!(ev.payload["command"], "echo hi");
                assert_eq!(ev.payload["heuristic"], true);
                assert!(ev.provenance.contains("script_completed"));
            }
            other => panic!("expected Evidence, got {other:?}"),
        }
    }

    #[test]
    fn custom_tool_call_output_without_recognized_marker_produces_only_event() {
        let mut pending = std::collections::HashMap::new();
        let output = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call_unknown",
                "output": [{"type": "input_text", "text": "something unexpected\n"}]
            }
        });
        let msgs = translate_line(&output, "sess-1", &mut pending).expect("should translate");
        assert_eq!(msgs.len(), 1);
    }
}
