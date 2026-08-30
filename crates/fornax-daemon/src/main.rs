//! Fornax local daemon (FORNX-25). One process: UDS event intake, immutable
//! storage, deterministic verification, and a localhost HTTP surface for the
//! status line, detail command, and dashboard (FORNX-30/31/32). No cloud
//! dependency on the critical path (D2, ADR 0001).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use fornax_types::redact::{redact_json, redact_text};
use fornax_types::{IngestMessage, RuntimeCapabilities};
use fornax_verify::{TestResultVerifier, Verifier};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

fn fornax_home() -> PathBuf {
    std::env::var("FORNAX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home().join(".fornax"))
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Clone)]
struct AppState {
    store: fornax_store::Store,
    /// Per-session capabilities, announced once by the adapter. Also
    /// persisted to `store` (FORNX-62, `capabilities_for_session`) for
    /// `export-spool`/replay — kept here in-memory too so FORNX-53/55's
    /// live-session verdict computation never pays a DB round trip on the
    /// claim-verification hot path.
    caps: Arc<Mutex<HashMap<String, RuntimeCapabilities>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let home = fornax_home();
    std::fs::create_dir_all(&home)?;
    let db_path = home.join("fornax.db");
    let sock_path = home.join("fornax.sock");
    if sock_path.exists() {
        std::fs::remove_file(&sock_path)?;
    }

    let store = fornax_store::Store::open(&db_path).await?;
    let state = AppState {
        store,
        caps: Arc::new(Mutex::new(HashMap::new())),
    };

    let uds_state = state.clone();
    let uds_task = tokio::spawn(async move {
        if let Err(e) = run_uds_server(&sock_path, uds_state).await {
            tracing::error!(error = %e, "UDS server exited");
        }
    });

    let app = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/findings/recent", get(api_findings_recent))
        .route("/dashboard", get(dashboard))
        .with_state(state);

    let port: u16 = std::env::var("FORNAX_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4317);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "fornax localhost dashboard listening");
    axum::serve(listener, app).await?;

    uds_task.abort();
    Ok(())
}

async fn run_uds_server(sock_path: &PathBuf, state: AppState) -> anyhow::Result<()> {
    let listener = UnixListener::bind(sock_path)?;
    tracing::info!(path = %sock_path.display(), "UDS ingest listening");
    loop {
        let (stream, _addr) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                tracing::warn!(error = %e, "ingest connection ended with error");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, state: AppState) -> anyhow::Result<()> {
    let mut lines = BufReader::new(stream).lines();
    let mut session_hint: Option<String> = None;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: IngestMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "dropping malformed ingest line");
                continue;
            }
        };
        if let Err(e) = handle_message(&state, msg, &mut session_hint).await {
            tracing::warn!(error = %e, "failed to process ingest message");
        }
    }
    Ok(())
}

async fn handle_message(
    state: &AppState,
    msg: IngestMessage,
    session_hint: &mut Option<String>,
) -> anyhow::Result<()> {
    match msg {
        IngestMessage::Capabilities(caps) => {
            // Capabilities may arrive before any Event sets `session_hint`
            // (e.g. Codex sends it right after `session_meta`, before the
            // first translatable event) — the adapter stamps the session id
            // into `notes` for exactly this case.
            let sid = caps
                .notes
                .get("session_id")
                .cloned()
                .or_else(|| session_hint.clone());
            if let Some(sid) = sid {
                *session_hint = Some(sid.clone());
                state.store.upsert_capabilities(&sid, &caps).await?;
                state.caps.lock().await.insert(sid, caps);
            }
        }
        IngestMessage::Event(mut ev) => {
            *session_hint = Some(ev.session_id.clone());
            // Privacy boundary (FORNX-33): redact recognizable secrets from
            // raw tool output before it is ever persisted. Applied once,
            // here, not re-derived by every downstream reader.
            // FORNX-280: tool_input carries attacker/agent-controlled command
            // text exactly like tool_response and raw do (e.g. a secret typed
            // into a shell command) — it must go through the same boundary.
            ev.tool_input = ev.tool_input.as_ref().map(redact_json);
            ev.tool_response = ev.tool_response.as_ref().map(redact_json);
            ev.raw = redact_json(&ev.raw);
            state.store.insert_event(&ev).await?;
        }
        IngestMessage::Evidence(mut ev) => {
            *session_hint = Some(ev.session_id.clone());
            ev.payload = redact_json(&ev.payload);
            state.store.insert_evidence(&ev).await?;
        }
        IngestMessage::Claim(mut claim) => {
            *session_hint = Some(claim.session_id.clone());
            // FORNX-280: claim text is derived from agent/user transcript
            // content and had no redaction boundary at all — a secret
            // pasted into a prompt or echoed by the agent reached storage,
            // logs, and export-spool output unredacted. Apply the same
            // privacy boundary as Event/Evidence, once, before persistence.
            claim.text = redact_text(&claim.text);
            state.store.insert_claim(&claim).await?;

            let caps = state
                .caps
                .lock()
                .await
                .get(&claim.session_id)
                .cloned()
                .unwrap_or_else(default_unknown_caps);
            let evidence = state.store.evidence_for_session(&claim.session_id).await?;

            let verifiers: Vec<Box<dyn Verifier + Send + Sync>> =
                vec![Box::new(TestResultVerifier)];
            for verifier in verifiers.iter().filter(|v| v.applies_to(&claim)) {
                let finding = verifier.verify(&claim, &evidence, &caps);
                tracing::info!(verdict = ?finding.verdict, claim = %claim.text, "finding computed");
                state.store.insert_finding(&finding).await?;
            }
        }
    }
    Ok(())
}

/// Conservative default when an adapter hasn't announced capabilities yet —
/// never assume a signal is observable (D4/D7).
fn default_unknown_caps() -> RuntimeCapabilities {
    RuntimeCapabilities {
        provider: fornax_types::Provider::Codex,
        supports_pre_tool_use: false,
        supports_post_tool_use: false,
        supports_tool_response_capture: false,
        supports_session_stop_event: false,
        supports_transcript_tail: false,
        supports_subagent_lifecycle: false,
        notes: [(
            "reason".to_string(),
            "no capabilities announced by adapter yet".to_string(),
        )]
        .into(),
    }
}

async fn api_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.store.recent_findings(1).await {
        Ok(rows) if !rows.is_empty() => Json(serde_json::json!({ "latest": rows[0] })),
        Ok(_) => Json(serde_json::json!({ "latest": null })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

async fn api_findings_recent(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.store.recent_findings(50).await {
        Ok(rows) => Json(serde_json::json!({ "findings": rows })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

async fn dashboard(State(state): State<AppState>) -> axum::response::Html<String> {
    let rows = state.store.recent_findings(50).await.unwrap_or_default();
    let mut body = String::from(
        "<html><head><title>Fornax — local evidence dashboard</title>\
         <style>body{font-family:monospace;margin:2rem;background:#0b0e14;color:#d6dee8}\
         table{border-collapse:collapse;width:100%}td,th{padding:.4rem .6rem;border-bottom:1px solid #2a3140;text-align:left}\
         .verified{color:#4caf50}.contradicted{color:#e5534b}.unverified{color:#c9a227}.review{color:#4a9dd6}.unavailable{color:#7d8798}\
         </style></head><body><h2>🛡 Fornax — recent findings</h2><table>\
         <tr><th>Verdict</th><th>Claim</th><th>Verifier</th><th>Rationale</th><th>When</th></tr>",
    );
    for r in rows {
        body.push_str(&format!(
            "<tr><td class=\"{v}\">{v}</td><td>{claim}</td><td>{verifier}</td><td>{rationale}</td><td>{when}</td></tr>",
            v = html_escape(&r.verdict),
            claim = html_escape(&r.claim_text),
            verifier = html_escape(&r.verifier_name),
            rationale = html_escape(&r.rationale),
            when = html_escape(&r.computed_at),
        ));
    }
    body.push_str("</table></body></html>");
    axum::response::Html(body)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fornax_types::{AgentEvent, Claim, EventKind, Provider};
    use uuid::Uuid;

    async fn test_state() -> AppState {
        let db_path = std::env::temp_dir().join(format!("fornax-test-{}.db", Uuid::new_v4()));
        let store = fornax_store::Store::open(&db_path)
            .await
            .expect("open test store");
        AppState {
            store,
            caps: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// FORNX-280 regression: a high-entropy secret in `tool_input` or claim
    /// text must never reach storage unredacted. Uses the same canary
    /// technique as the manual gap-closure repro: a random-hex marker shaped
    /// to trip `redact_json`/`redact_text`'s generic high-entropy detector,
    /// not a human-readable string a detector would legitimately ignore.
    #[tokio::test]
    async fn tool_input_and_claim_text_are_redacted_before_storage() {
        let state = test_state().await;
        let mut hint = None;
        let marker = format!("FORNAX-CANARY-{}-DO-NOT-LEAK", Uuid::new_v4().simple());
        let session_id = "fornx-280-regression".to_string();

        let event_id = Uuid::new_v4();
        let event = AgentEvent {
            id: event_id,
            session_id: session_id.clone(),
            provider: Provider::ClaudeCode,
            kind: EventKind::PreToolUse,
            observed_at: "2026-08-30T00:00:00Z".to_string(),
            tool_name: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": format!("echo {marker}")})),
            tool_response: None,
            raw: serde_json::json!({}),
        };
        handle_message(&state, IngestMessage::Event(event), &mut hint)
            .await
            .expect("handle event");

        let claim = Claim {
            id: Uuid::new_v4(),
            session_id: session_id.clone(),
            source_event_id: event_id,
            text: format!("All tests passed. Session secret for audit: {marker}"),
            subject: "test_result".to_string(),
            claimed_at: "2026-08-30T00:00:00Z".to_string(),
        };
        handle_message(&state, IngestMessage::Claim(claim), &mut hint)
            .await
            .expect("handle claim");

        let stored_events = state
            .store
            .events_for_session(&session_id)
            .await
            .expect("read back events");
        let stored_input = stored_events[0]
            .tool_input
            .as_ref()
            .expect("tool_input present")
            .to_string();
        assert!(
            !stored_input.contains(&marker),
            "raw canary marker leaked into stored tool_input: {stored_input}"
        );
        assert!(
            stored_input.contains("REDACTED"),
            "expected a redacted placeholder in stored tool_input: {stored_input}"
        );

        let stored_claims = state
            .store
            .claims_for_session(&session_id)
            .await
            .expect("read back claims");
        assert!(
            !stored_claims[0].text.contains(&marker),
            "raw canary marker leaked into stored claim text: {}",
            stored_claims[0].text
        );
        assert!(
            stored_claims[0].text.contains("REDACTED"),
            "expected a redacted placeholder in stored claim text: {}",
            stored_claims[0].text
        );
    }
}
