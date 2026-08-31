//! Fornax local daemon (FORNX-25). One process: UDS event intake, immutable
//! storage, deterministic verification, and a localhost HTTP surface for the
//! status line, detail command, and dashboard (FORNX-30/31/32). No cloud
//! dependency on the critical path (D2, ADR 0001).

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use fornax_types::redact::{redact_json, redact_text};
use fornax_types::{IngestMessage, RuntimeCapabilities};
use fornax_verify::{
    CommandExecutedVerifier, CommandSuccessVerifier, TestResultVerifier, Verifier,
};
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

/// Single-instance guard (FORNX-12): a stale socket file left behind by an
/// unclean shutdown must not be confused with a second `fornaxd` already
/// live. Probe the existing path with a real connection attempt — a live
/// listener accepts it, a stale file refuses it — instead of unconditionally
/// deleting the path and silently stealing the socket out from under a
/// running daemon.
async fn ensure_single_instance(sock_path: &PathBuf) -> anyhow::Result<()> {
    if sock_path.exists() {
        match UnixStream::connect(sock_path).await {
            Ok(_) => {
                anyhow::bail!(
                    "fornaxd is already running: another daemon is live on {} — \
                     stop it first (e.g. `fornax daemon stop`) before starting a new one",
                    sock_path.display()
                );
            }
            Err(_) => {
                // Nothing is listening: a stale file from a previous unclean
                // shutdown (crash, kill -9, power loss). Safe to reclaim.
                tracing::warn!(
                    path = %sock_path.display(),
                    "removing stale socket file from a previous unclean shutdown"
                );
                std::fs::remove_file(sock_path)?;
            }
        }
    }
    Ok(())
}

/// Resolves to `()` once SIGTERM or SIGINT/Ctrl-C is received, for use with
/// `axum::serve(..).with_graceful_shutdown(..)` (FORNX-12: no signal handling
/// existed before this — the process could only be killed uncleanly).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl-C, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
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
    /// FORNX-281: each hook invocation is a fresh UDS connection handled by
    /// its own spawned task, with no ack from the daemon back to the hook —
    /// so nothing guarantees an earlier event (e.g. PostToolUse, carrying
    /// the exit-code Evidence a claim needs) finishes its DB write before a
    /// later message (e.g. Stop's Claim, which verifies against whatever
    /// Evidence already exists) starts processing on a different task. This
    /// is a single local daemon serving one user's sequential agent
    /// actions (ADR 0001) — not a system that needs concurrent throughput —
    /// so the correct fix is to make message *processing* strictly
    /// serialized in arrival order, not to make verification tolerant of
    /// partial evidence. Held for the full duration of `handle_message`.
    processing: Arc<Mutex<()>>,
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
    let pid_path = home.join("fornaxd.pid");
    ensure_single_instance(&sock_path).await?;
    // Written for `fornax daemon stop` to find this process (FORNX-12). Best
    // effort: a daemon that can't write its own pid file still runs, it just
    // can't be stopped via the CLI convenience command.
    if let Err(e) = std::fs::write(&pid_path, std::process::id().to_string()) {
        tracing::warn!(error = %e, "failed to write pid file");
    }

    let store = fornax_store::Store::open(&db_path).await?;
    let state = AppState {
        store,
        caps: Arc::new(Mutex::new(HashMap::new())),
        processing: Arc::new(Mutex::new(())),
    };

    let uds_sock_path = sock_path.clone();
    let uds_state = state.clone();
    let uds_task = tokio::spawn(async move {
        if let Err(e) = run_uds_server(&uds_sock_path, uds_state).await {
            tracing::error!(error = %e, "UDS server exited");
        }
    });

    let app = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/findings/recent", get(api_findings_recent))
        .route("/dashboard", get(dashboard))
        .route("/dashboard/session/:session_id", get(dashboard_session))
        .route("/dashboard/finding/:finding_id", get(dashboard_finding))
        .with_state(state);

    let port: u16 = std::env::var("FORNAX_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4317);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "fornax localhost dashboard listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // FORNX-12: on a clean shutdown (SIGTERM/Ctrl-C), stop accepting new UDS
    // connections and reclaim both filesystem artifacts so the next start
    // doesn't have to distinguish "stale from a clean stop" from "stale from
    // a crash" — there's nothing left to distinguish.
    uds_task.abort();
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&pid_path);
    tracing::info!("fornaxd stopped");
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
    // FORNX-281: serialize processing across every connection/task so a
    // later message's read of already-persisted state (e.g. a Claim
    // reading Evidence written by an earlier Event) can never race an
    // earlier message's still-in-flight write. See the `processing` field
    // doc comment for why this is the correct fix for a single local
    // daemon rather than making verification tolerant of partial evidence.
    let _serialize = state.processing.lock().await;
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
            let evidence_read = state.store.evidence_for_session(&claim.session_id).await?;
            if !evidence_read.failed.is_empty() {
                tracing::warn!(
                    session_id = %claim.session_id,
                    failed = evidence_read.failed.len(),
                    total = evidence_read.evidence.len() + evidence_read.failed.len(),
                    "skipping evidence rows that failed to deserialize"
                );
            }
            let evidence = evidence_read.evidence;

            // FORNX-14: registry stays a flat Vec, per the ticket's own
            // maintainability requirement ("verifier registry/dispatch only
            // as complex as the first real verifier set requires") — three
            // verifiers dispatched by `applies_to` doesn't yet justify more.
            let verifiers: Vec<Box<dyn Verifier + Send + Sync>> = vec![
                Box::new(TestResultVerifier),
                Box::new(CommandExecutedVerifier),
                Box::new(CommandSuccessVerifier),
            ];
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
/// never assume a signal is observable (D4/D7). Every class reads back
/// `Unknown` (ordinary absence, an empty `signals` list), which is
/// behaviorally identical to today's all-`false` bools at every verifier
/// gate — this fallback still never opens a gate it shouldn't.
///
/// FORNX-288 (found during FORNX-155's `RuntimeCapabilities` formalization,
/// disclosed there, fixed here): `provider` used to be hardcoded to `Codex`
/// regardless of which provider's session this actually is — a fabricated
/// provider identity for a session whose adapter simply hasn't spoken yet.
/// This now uses a real `Provider::Unknown` variant instead of guessing.
/// That variant is deliberately narrow-purpose: it is never written by
/// `Store::upsert_capabilities` (only a real announced `Capabilities`
/// message reaches that path) and never exported to `fornax-cloud`'s
/// separate, closed ingest enum, which doesn't know this variant — this
/// value only ever flows into `Verifier::verify`, and `Finding` carries no
/// provider field. A session→provider store lookup remains out of scope
/// here: it would put a DB round trip on the verify hot path the `caps`
/// in-memory cache exists specifically to avoid. Further provider plumbing
/// through the claim path, if ever needed, is FORNX-138 scope.
fn default_unknown_caps() -> RuntimeCapabilities {
    RuntimeCapabilities {
        schema_version: fornax_types::CAPABILITY_SCHEMA_VERSION,
        provider: fornax_types::Provider::Unknown,
        signals: vec![],
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

/// Shared page chrome (FORNX-18): every dashboard page gets the same dark
/// monospace look the original flat-table dashboard established, plus the
/// provider badge styling used by session list/detail. Provider badges are
/// styled generically by a `data-provider` attribute holding the raw
/// snake_case tag (`claude_code`, `codex`, `open_code`, `unknown`, ...) —
/// new colors can be added here without any Rust branch on `Provider`, and
/// an unrecognized future provider still renders (falls through to the
/// default badge style) rather than needing a code change (FORNX-18 AC:
/// "distinguishable without provider-specific UI forks").
const PAGE_STYLE: &str = "body{font-family:monospace;margin:2rem;background:#0b0e14;color:#d6dee8}\
     table{border-collapse:collapse;width:100%}td,th{padding:.4rem .6rem;border-bottom:1px solid #2a3140;text-align:left}\
     a{color:#4a9dd6}\
     .verified{color:#4caf50}.contradicted{color:#e5534b}.unverified{color:#c9a227}.review{color:#4a9dd6}.unavailable{color:#7d8798}\
     .badge{display:inline-block;padding:.1rem .5rem;border-radius:.75rem;font-size:.85em;border:1px solid #4a9dd6;color:#4a9dd6}\
     .badge[data-provider=\"claude_code\"]{border-color:#c9a227;color:#c9a227}\
     .badge[data-provider=\"codex\"]{border-color:#4a9dd6;color:#4a9dd6}\
     .badge[data-provider=\"open_code\"]{border-color:#4caf50;color:#4caf50}\
     .badge[data-provider=\"unknown\"]{border-color:#7d8798;color:#7d8798}\
     .section{margin-top:1.5rem}\
     .empty{color:#7d8798;font-style:italic}\
     .breadcrumb{margin-bottom:1rem}\
     code{color:#c9a227}";

fn page(title: &str, body: &str) -> axum::response::Html<String> {
    axum::response::Html(format!(
        "<html><head><title>{title}</title><style>{PAGE_STYLE}</style></head><body>{body}</body></html>",
        title = html_escape(title),
    ))
}

fn provider_badge(provider: &str) -> String {
    format!(
        "<span class=\"badge\" data-provider=\"{tag}\">{label}</span>",
        tag = html_escape(provider),
        label = html_escape(&provider.replace('_', " "))
    )
}

/// Session-list/overview page (FORNX-18 AC: "session list/overview").
async fn dashboard(State(state): State<AppState>) -> axum::response::Html<String> {
    let sessions = state.store.sessions_overview().await.unwrap_or_default();
    let mut body = String::from("<h2>🛡 Fornax — sessions</h2>");
    if sessions.is_empty() {
        body.push_str("<p class=\"empty\">No sessions recorded yet.</p>");
    } else {
        body.push_str(
            "<table><tr><th>Session</th><th>Provider</th><th>Events</th><th>Claims</th>\
             <th>Findings</th><th>Last activity</th></tr>",
        );
        for s in sessions {
            body.push_str(&format!(
                "<tr><td><a href=\"/dashboard/session/{id_enc}\">{id}</a></td><td>{provider}</td>\
                 <td>{events}</td><td>{claims}</td><td>{findings}</td><td>{when}</td></tr>",
                id_enc = url_path_escape(&s.session_id),
                id = html_escape(&s.session_id),
                provider = provider_badge(&s.provider),
                events = s.event_count,
                claims = s.claim_count,
                findings = s.finding_count,
                when = html_escape(&s.last_activity),
            ));
        }
        body.push_str("</table>");
    }
    page("Fornax — sessions", &body)
}

/// Session-detail page (FORNX-18 AC: session detail, host/runtime
/// capability availability).
async fn dashboard_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> axum::response::Html<String> {
    let claims = state
        .store
        .claims_for_session(&session_id)
        .await
        .unwrap_or_default();
    let findings = state
        .store
        .findings_for_session(&session_id)
        .await
        .unwrap_or_default();
    let capabilities = state
        .store
        .capabilities_for_session(&session_id)
        .await
        .unwrap_or_default();

    let mut body = format!(
        "<div class=\"breadcrumb\"><a href=\"/dashboard\">&larr; sessions</a></div>\
         <h2>Session <code>{id}</code></h2>",
        id = html_escape(&session_id)
    );

    body.push_str("<div class=\"section\"><h3>Host/runtime capabilities</h3>");
    if capabilities.is_empty() {
        body.push_str("<p class=\"empty\">No capabilities announced for this session yet.</p>");
    } else {
        for caps in &capabilities {
            body.push_str(&format!(
                "<p>{badge}</p><table><tr><th>Signal</th><th>State</th><th>Detail</th></tr>",
                badge = provider_badge(&provider_tag(&caps.provider)),
            ));
            for sig in &caps.signals {
                body.push_str(&format!(
                    "<tr><td>{class}</td><td>{state}</td><td>{detail}</td></tr>",
                    class = html_escape(&format!("{:?}", sig.class)),
                    state = html_escape(&format!("{:?}", sig.state)),
                    detail = html_escape(sig.detail.as_deref().unwrap_or("")),
                ));
            }
            body.push_str("</table>");
        }
    }
    body.push_str("</div>");

    body.push_str("<div class=\"section\"><h3>Claims &amp; findings</h3>");
    if claims.is_empty() {
        body.push_str("<p class=\"empty\">No claims recorded for this session.</p>");
    } else {
        body.push_str(
            "<table><tr><th>Claim</th><th>Verdict</th><th>Verifier</th><th>When</th></tr>",
        );
        for c in &claims {
            // A claim may have zero, one, or (if re-verified) more findings;
            // render one row per finding, or one placeholder row if none
            // exist yet — a claim is never silently omitted.
            let claim_findings: Vec<_> = findings
                .iter()
                .filter(|f| f.claim_id == c.id.to_string())
                .collect();
            if claim_findings.is_empty() {
                body.push_str(&format!(
                    "<tr><td>{claim}</td><td class=\"empty\">no finding yet</td><td></td><td>{when}</td></tr>",
                    claim = html_escape(&c.text),
                    when = html_escape(&c.claimed_at),
                ));
            } else {
                for f in claim_findings {
                    body.push_str(&format!(
                        "<tr><td>{claim}</td><td class=\"{v}\"><a href=\"/dashboard/finding/{fid}\">{v}</a></td>\
                         <td>{verifier}</td><td>{when}</td></tr>",
                        claim = html_escape(&c.text),
                        v = html_escape(&f.verdict),
                        fid = url_path_escape(&f.id),
                        verifier = html_escape(&f.verifier_name),
                        when = html_escape(&f.computed_at),
                    ));
                }
            }
        }
        body.push_str("</table>");
    }
    body.push_str("</div>");

    page(&format!("Fornax — session {session_id}"), &body)
}

/// Finding-detail page (FORNX-18 AC: "supporting/contradicting/missing/
/// unavailable evidence with provenance"). The five-state verdict
/// vocabulary (ADR 0001) maps 1:1 onto the evidence category shown: a
/// verifier only ever attaches `evidence_ids` to support the verdict it
/// actually reached, so which category applies falls out of `verdict`
/// alone rather than needing separate classification logic.
async fn dashboard_finding(
    State(state): State<AppState>,
    Path(finding_id): Path<String>,
) -> axum::response::Html<String> {
    let Ok(Some(finding)) = state.store.finding_by_id(&finding_id).await else {
        return page(
            "Fornax — finding not found",
            "<p class=\"empty\">No such finding.</p>",
        );
    };

    let linked_ids: Vec<String> = serde_json::from_str(&finding.evidence_ids).unwrap_or_default();
    let session_evidence = state
        .store
        .evidence_for_session(&finding.session_id)
        .await
        .map(|o| o.evidence)
        .unwrap_or_default();
    let (linked, other): (Vec<_>, Vec<_>) = session_evidence
        .into_iter()
        .partition(|e| linked_ids.contains(&e.id.to_string()));

    let evidence_label = match finding.verdict.as_str() {
        "verified" => "Supporting evidence",
        "contradicted" => "Contradicting evidence",
        "review" => "Evidence requiring review",
        "unverified" => "Missing evidence",
        "unavailable" => "Unavailable evidence",
        _ => "Linked evidence",
    };

    let mut body = format!(
        "<div class=\"breadcrumb\"><a href=\"/dashboard/session/{sid_enc}\">&larr; session {sid}</a></div>\
         <h2>Finding <code>{fid}</code></h2>\
         <p><strong>Claim:</strong> {claim}</p>\
         <p><strong>Verdict:</strong> <span class=\"{v}\">{v}</span></p>\
         <p><strong>Verifier:</strong> {verifier}</p>\
         <p><strong>Rationale:</strong> {rationale}</p>\
         <p><strong>Computed at:</strong> {when}</p>",
        sid_enc = url_path_escape(&finding.session_id),
        sid = html_escape(&finding.session_id),
        fid = html_escape(&finding.id),
        claim = html_escape(&finding.claim_text),
        v = html_escape(&finding.verdict),
        verifier = html_escape(&finding.verifier_name),
        rationale = html_escape(&finding.rationale),
        when = html_escape(&finding.computed_at),
    );

    body.push_str(&format!("<div class=\"section\"><h3>{evidence_label}</h3>"));
    body.push_str(&evidence_table(
        &linked,
        "No evidence is linked to this finding.",
    ));
    body.push_str("</div>");

    body.push_str("<div class=\"section\"><h3>Other evidence observed in this session</h3>");
    body.push_str(&evidence_table(
        &other,
        "No other evidence was observed for this session.",
    ));
    body.push_str("</div>");

    page(&format!("Fornax — finding {finding_id}"), &body)
}

fn evidence_table(evidence: &[fornax_types::Evidence], empty_message: &str) -> String {
    if evidence.is_empty() {
        return format!("<p class=\"empty\">{}</p>", html_escape(empty_message));
    }
    let mut out = String::from(
        "<table><tr><th>Kind</th><th>Payload</th><th>Provenance</th><th>Source</th><th>When</th></tr>",
    );
    for e in evidence {
        let source = e
            .source
            .as_ref()
            .map(|s| {
                let trust_class = format!("{:?}", s.trust_class);
                let collection_method = format!("{:?}", s.collection_method);
                format!("{} / {trust_class} ({collection_method})", s.sensor_name)
            })
            .unwrap_or_else(|| "no sensor provenance recorded".to_string());
        out.push_str(&format!(
            "<tr><td>{kind}</td><td>{payload}</td><td>{provenance}</td><td>{source}</td><td>{when}</td></tr>",
            kind = html_escape(&format!("{:?}", e.kind)),
            payload = html_escape(&e.payload.to_string()),
            provenance = html_escape(&e.provenance),
            source = html_escape(&source),
            when = html_escape(&e.observed_at),
        ));
    }
    out.push_str("</table>");
    out
}

/// Bare snake_case tag for a `Provider`, matching how it's stored/serialized
/// elsewhere (`fornax_types::Provider`'s `#[serde(rename_all = "snake_case")]`)
/// — used to drive `provider_badge`'s generic, data-attribute-based styling
/// without a per-variant match arm.
fn provider_tag(provider: &fornax_types::Provider) -> String {
    match serde_json::to_value(provider) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_string(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Percent-encodes a string for safe use as one path segment inside an
/// `href="..."` attribute value (FORNX-18 hardening). `session_id`/
/// `finding_id` values originate from untrusted hook payloads (see the
/// adversarial corpus's hostile-session-id cases) and, unlike the plain-text
/// contexts `html_escape` was written for, these ids are interpolated
/// directly into an HTML attribute — `html_escape` alone does not escape
/// `"` or `'`, so a session id containing a quote could break out of the
/// `href` attribute and inject a new one (e.g. `onmouseover=`). Restricting
/// the output to the URL-safe unreserved character set closes that off
/// entirely: every byte outside `[A-Za-z0-9._~-]` becomes `%XX`, so the
/// result can never contain a quote, angle bracket, or `=` and is also a
/// correctly encoded single path segment (a literal `/` in the id can no
/// longer be mistaken for a path separator).
fn url_path_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
            processing: Arc::new(Mutex::new(())),
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

    /// FORNX-12 regression: no socket file at all is the ordinary first-boot
    /// case — must not error.
    #[tokio::test]
    async fn ensure_single_instance_allows_first_boot_with_no_socket() {
        let sock_path = std::env::temp_dir().join(format!("fornax-test-{}.sock", Uuid::new_v4()));
        assert!(!sock_path.exists());
        ensure_single_instance(&sock_path)
            .await
            .expect("no existing socket must never block startup");
    }

    /// FORNX-12 regression: a leftover socket *file* with nothing listening
    /// on it (the daemon's previous process crashed/was killed -9) must be
    /// reclaimed, not mistaken for a live instance.
    #[tokio::test]
    async fn ensure_single_instance_reclaims_a_stale_socket_file() {
        let sock_path = std::env::temp_dir().join(format!("fornax-test-{}.sock", Uuid::new_v4()));
        // A plain file at the socket path (not an actual bound socket) is
        // exactly what a crash leaves behind: the inode exists, nothing
        // accepts connections on it.
        std::fs::write(&sock_path, b"stale").expect("write stale socket file");

        ensure_single_instance(&sock_path)
            .await
            .expect("a stale socket file must be reclaimed, not treated as a live instance");
        assert!(
            !sock_path.exists(),
            "stale socket file should have been removed"
        );
    }

    /// FORNX-12 regression: a second daemon must refuse to start — loudly —
    /// rather than deleting the first daemon's live socket out from under it.
    #[tokio::test]
    async fn ensure_single_instance_refuses_when_another_daemon_is_live() {
        let sock_path = std::env::temp_dir().join(format!("fornax-test-{}.sock", Uuid::new_v4()));
        let listener = UnixListener::bind(&sock_path).expect("bind first instance's socket");
        // Keep the listener alive for the duration of the check by accepting
        // in the background; the guard only needs *something* live on the
        // other end of `connect`.
        let _accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let result = ensure_single_instance(&sock_path).await;
        assert!(
            result.is_err(),
            "starting a second instance while one is live must be refused"
        );
        assert!(
            sock_path.exists(),
            "the first instance's live socket must not be deleted"
        );

        let _ = std::fs::remove_file(&sock_path);
    }

    /// FORNX-288 regression: a session that submits a Claim before any
    /// Capabilities announcement must fall back to a real `Provider::Unknown`
    /// value, not a fabricated guess at a specific provider.
    #[test]
    fn default_unknown_caps_uses_unknown_provider() {
        let caps = default_unknown_caps();
        assert_eq!(caps.provider, Provider::Unknown);
    }

    // ---------------------------------------------------------------
    // FORNX-18: dashboard session-list / session-detail / finding-detail
    // ---------------------------------------------------------------

    fn sample_capabilities(session_id: &str) -> RuntimeCapabilities {
        use fornax_types::{CapabilitySignal, SignalAvailability, SignalClass};
        RuntimeCapabilities {
            schema_version: fornax_types::CAPABILITY_SCHEMA_VERSION,
            provider: Provider::ClaudeCode,
            signals: vec![CapabilitySignal {
                class: SignalClass::ToolTrace,
                state: SignalAvailability::Available,
                detail: None,
            }],
            notes: [("session_id".to_string(), session_id.to_string())].into(),
        }
    }

    /// Drives a full PostToolUse(exit 0) + "All tests passed" claim through
    /// `handle_message` exactly as a real adapter connection would (a
    /// Capabilities announcement, then the Event, then the matching
    /// Evidence, then the Claim) so a real `VERIFIED` finding exists to
    /// render — this test module calls `handle_message` directly rather
    /// than spawning a real daemon (that's what
    /// `tests/adversarial_daemon_input.rs`/`tests/concurrent_hook_submission.rs`
    /// are for); it only needs a durable finding to check the HTML against.
    async fn seed_verified_session(state: &AppState, session_id: &str) -> String {
        let mut hint = None;
        handle_message(
            state,
            IngestMessage::Capabilities(sample_capabilities(session_id)),
            &mut hint,
        )
        .await
        .expect("handle capabilities");

        let event_id = Uuid::new_v4();
        let event = AgentEvent {
            id: event_id,
            session_id: session_id.to_string(),
            provider: Provider::ClaudeCode,
            kind: EventKind::PostToolUse,
            observed_at: "2026-08-31T00:00:00Z".to_string(),
            tool_name: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": "cargo test --workspace"})),
            tool_response: Some(serde_json::json!({"exit_code": 0})),
            raw: serde_json::json!({}),
        };
        handle_message(state, IngestMessage::Event(event), &mut hint)
            .await
            .expect("handle event");

        let evidence = fornax_types::Evidence {
            id: Uuid::new_v4(),
            session_id: session_id.to_string(),
            source_event_id: event_id,
            kind: fornax_types::EvidenceKind::ExitCode,
            observed_at: "2026-08-31T00:00:01Z".to_string(),
            payload: serde_json::json!({"command": "cargo test --workspace", "exit_code": 0}),
            provenance: "claude_code:PostToolUse:Bash#tool_response".to_string(),
            source: None,
            extension: None,
        };
        handle_message(state, IngestMessage::Evidence(evidence), &mut hint)
            .await
            .expect("handle evidence");

        let claim = Claim {
            id: Uuid::new_v4(),
            session_id: session_id.to_string(),
            source_event_id: event_id,
            text: "All tests passed.".to_string(),
            subject: "test_result".to_string(),
            claimed_at: "2026-08-31T00:00:02Z".to_string(),
        };
        handle_message(state, IngestMessage::Claim(claim), &mut hint)
            .await
            .expect("handle claim");

        let findings = state
            .store
            .findings_for_session(session_id)
            .await
            .expect("findings for session");
        assert_eq!(findings.len(), 1, "expected exactly one seeded finding");
        assert_eq!(findings[0].verdict, "verified");
        findings[0].id.clone()
    }

    #[tokio::test]
    async fn dashboard_lists_the_session_with_its_provider_badge() {
        let state = test_state().await;
        let session_id = "fornx-18-session-list";
        seed_verified_session(&state, session_id).await;

        let html = dashboard(State(state)).await.0;
        assert!(
            html.contains(session_id),
            "session list must show the session id: {html}"
        );
        assert!(
            html.contains("data-provider=\"claude_code\""),
            "session list must badge the session's provider: {html}"
        );
        assert!(html.contains(&format!("/dashboard/session/{session_id}")));
    }

    /// FORNX-18 hardening regression: `session_id` is untrusted (it arrives
    /// verbatim from hook payloads — see the adversarial corpus's hostile
    /// session-id cases) and is interpolated into an `href="..."` attribute,
    /// not just page text. `html_escape` alone does not escape `"`, so a
    /// naive implementation lets a quote in the id break out of the
    /// attribute and inject a new one. Assert the raw payload never appears
    /// unescaped in the `href` attribute value.
    #[tokio::test]
    async fn dashboard_session_link_is_safe_against_a_quote_breakout_session_id() {
        let state = test_state().await;
        let session_id = "fornx-18-xss\" onmouseover=\"alert(1)";
        seed_verified_session(&state, session_id).await;

        let html = dashboard(State(state)).await.0;
        assert!(
            !html.contains("onmouseover=\"alert(1)\">"),
            "hostile session id must not break out of the href attribute: {html}"
        );
        assert!(
            html.contains(&url_path_escape(session_id)),
            "href must contain the percent-encoded session id: {html}"
        );
        // Percent-encoding never produces a literal `"`.
        assert!(!url_path_escape(session_id).contains('"'));
    }

    #[tokio::test]
    async fn dashboard_session_links_to_its_finding() {
        let state = test_state().await;
        let session_id = "fornx-18-session-detail";
        let finding_id = seed_verified_session(&state, session_id).await;

        let html = dashboard_session(State(state), Path(session_id.to_string()))
            .await
            .0;
        assert!(html.contains("All tests passed."));
        assert!(html.contains(&format!("/dashboard/finding/{finding_id}")));
        // Host/runtime capability availability (FORNX-18 AC).
        assert!(html.contains("ToolTrace"));
    }

    #[tokio::test]
    async fn dashboard_finding_shows_supporting_evidence_with_provenance_for_verified() {
        let state = test_state().await;
        let session_id = "fornx-18-finding-detail-verified";
        let finding_id = seed_verified_session(&state, session_id).await;

        let html = dashboard_finding(State(state), Path(finding_id)).await.0;
        assert!(html.contains("Supporting evidence"));
        assert!(html.contains("claude_code:PostToolUse:Bash#tool_response"));
        assert!(html.contains("verified"));
    }

    #[tokio::test]
    async fn dashboard_finding_shows_missing_evidence_label_for_unverified() {
        let state = test_state().await;
        let session_id = "fornx-18-finding-detail-unverified";
        let mut hint = None;
        // Capabilities announced, but no PostToolUse/Evidence at all — the
        // TestResultVerifier's "no test-runner invocation observed" path.
        handle_message(
            &state,
            IngestMessage::Capabilities(sample_capabilities(session_id)),
            &mut hint,
        )
        .await
        .expect("handle capabilities");

        // `claims.source_event_id` is a foreign key into `agent_events` — a
        // real UserPromptSubmit-style event exists even when there's no
        // PostToolUse/Evidence for the verifier to find.
        let event_id = Uuid::new_v4();
        let event = AgentEvent {
            id: event_id,
            session_id: session_id.to_string(),
            provider: Provider::ClaudeCode,
            kind: EventKind::UserPromptSubmit,
            observed_at: "2026-08-31T00:00:00Z".to_string(),
            tool_name: None,
            tool_input: None,
            tool_response: None,
            raw: serde_json::json!({}),
        };
        handle_message(&state, IngestMessage::Event(event), &mut hint)
            .await
            .expect("handle event");

        let claim = Claim {
            id: Uuid::new_v4(),
            session_id: session_id.to_string(),
            source_event_id: event_id,
            text: "All tests passed.".to_string(),
            subject: "test_result".to_string(),
            claimed_at: "2026-08-31T00:00:01Z".to_string(),
        };
        handle_message(&state, IngestMessage::Claim(claim), &mut hint)
            .await
            .expect("handle claim");

        let findings = state
            .store
            .findings_for_session(session_id)
            .await
            .expect("findings");
        assert_eq!(findings[0].verdict, "unverified");

        let html = dashboard_finding(State(state), Path(findings[0].id.clone()))
            .await
            .0;
        assert!(html.contains("Missing evidence"));
        assert!(html.contains("No evidence is linked to this finding."));
    }

    #[tokio::test]
    async fn dashboard_finding_not_found_renders_a_placeholder_not_a_panic() {
        let state = test_state().await;
        let html = dashboard_finding(State(state), Path("no-such-finding".to_string()))
            .await
            .0;
        assert!(html.contains("No such finding"));
    }
}
