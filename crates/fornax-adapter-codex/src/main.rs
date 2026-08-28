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
fn codex_capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        provider: Provider::Codex,
        supports_pre_tool_use: false,
        supports_post_tool_use: true, // via rollout exec_command_end, not hooks
        supports_tool_response_capture: true,
        supports_session_stop_event: true, // task_complete in rollout
        supports_transcript_tail: true,
        supports_subagent_lifecycle: false,
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

                if let Some(msgs) = translate_line(&entry, &sid) {
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

fn translate_line(entry: &serde_json::Value, session_id: &str) -> Option<Vec<IngestMessage>> {
    let top_type = entry.get("type").and_then(|v| v.as_str())?;
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

fn claims_tests_passed(text: &str) -> bool {
    let t = text.to_lowercase();
    (t.contains("test") || t.contains("tests"))
        && (t.contains("passed") || t.contains("succeeded") || t.contains("all green"))
        && !t.contains("failed")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let msgs = translate_line(&entry, "sess-1").expect("should translate");
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
        let msgs = translate_line(&entry, "sess-1").expect("should translate");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn task_complete_with_passing_claim_produces_event_and_claim() {
        let entry = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "last_agent_message": "All tests passed."}
        });
        let msgs = translate_line(&entry, "sess-1").expect("should translate");
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[1], IngestMessage::Claim(c) if c.subject == "test_result"));
    }

    #[test]
    fn task_complete_without_a_claim_producing_message_is_event_only() {
        let entry = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "last_agent_message": "Done, see the diff."}
        });
        let msgs = translate_line(&entry, "sess-1").expect("should translate");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn non_event_msg_lines_are_ignored() {
        let entry = serde_json::json!({"type": "session_meta", "payload": {"session_id": "s"}});
        assert!(translate_line(&entry, "sess-1").is_none());
    }

    #[test]
    fn unknown_event_msg_subtype_is_ignored() {
        let entry = serde_json::json!({"type": "event_msg", "payload": {"type": "token_count"}});
        assert!(translate_line(&entry, "sess-1").is_none());
    }
}
