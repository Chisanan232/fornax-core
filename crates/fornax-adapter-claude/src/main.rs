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

    let mut out = vec![IngestMessage::Event(event)];

    // PostToolUse for a Bash call: if Claude Code's tool_response carries an
    // exit-code-shaped field, extract it as Evidence. Field name is not
    // stable across CC versions (see docs/research/adapter-capability-matrix.md);
    // check a small set of plausible keys rather than assume one.
    if kind == EventKind::PostToolUse && tool_name.as_deref() == Some("Bash") {
        if let Some(resp) = &tool_response {
            let exit_code = ["exit_code", "exitCode", "returncode", "status"]
                .iter()
                .find_map(|k| resp.get(k).and_then(|v| v.as_i64()));
            if let Some(code) = exit_code {
                out.push(IngestMessage::Evidence(Evidence {
                    id: Uuid::new_v4(),
                    session_id: session_id.clone(),
                    source_event_id: event_id,
                    kind: EvidenceKind::ExitCode,
                    observed_at: now.clone(),
                    payload: serde_json::json!({
                        "command": tool_input_command(raw),
                        "exit_code": code,
                    }),
                    provenance: "claude_code:PostToolUse:Bash#tool_response".to_string(),
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
        if let Some(text) = entry
            .pointer("/message/content/0/text")
            .and_then(|v| v.as_str())
        {
            last_text = Some(text.to_string());
        }
    }
    last_text
}

#[cfg(test)]
mod tests {
    use super::*;
    use fornax_types::{EventKind, IngestMessage, Provider};

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
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            IngestMessage::Event(e) => {
                assert_eq!(e.provider, Provider::ClaudeCode);
                assert_eq!(e.kind, EventKind::PostToolUse);
                assert_eq!(e.session_id, "sess-1");
            }
            other => panic!("expected Event, got {other:?}"),
        }
        match &msgs[1] {
            IngestMessage::Evidence(ev) => {
                assert_eq!(ev.payload["exit_code"], 1);
            }
            other => panic!("expected Evidence, got {other:?}"),
        }
    }

    #[test]
    fn post_tool_use_without_exit_code_produces_only_event() {
        let raw = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
            "tool_response": {"stdout": "hi\n"}
        });
        let msgs = translate(&raw);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn stop_event_without_transcript_path_produces_only_event() {
        let raw = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "sess-1"
        });
        let msgs = translate(&raw);
        assert_eq!(msgs.len(), 1);
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
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(&msgs[0], IngestMessage::Event(e) if e.kind == EventKind::UserPromptSubmit)
        );
    }
}
