//! `fornax` CLI (FORNX-31): compact status-line segment + detail drill-down,
//! reading from the same daemon-local API the dashboard uses (FORNX-32) — one
//! source of truth, not three interpretations of session integrity.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "fornax",
    version,
    about = "Fornax local evidence-integrity CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compact one-line integrity summary, suitable for embedding in a
    /// status line segment.
    Status,
    /// Full evidence/finding detail for recent sessions.
    Detail,
    /// Export one session's events/claims/evidence from the local store into
    /// a directory-based spool, as one wire-compatible envelope JSON file per
    /// message (FORNX-60). Reads `$FORNAX_HOME/fornax.db` directly — no
    /// daemon dependency, so this also works while the daemon is stopped.
    ///
    /// Written to `<out>/pending/<id>.json`, matching the layout a consumer
    /// such as fornax-cloud's uploader spool expects: one JSON object per
    /// file, internally tagged with a `"type"` field of `"event"`,
    /// `"claim"`, or `"evidence"`.
    ExportSpool {
        /// Session id to export.
        #[arg(long)]
        session: String,
        /// Spool root directory to write into (its `pending/` subdirectory
        /// is created if missing).
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

fn base_url() -> String {
    let port = std::env::var("FORNAX_HTTP_PORT").unwrap_or_else(|_| "4317".to_string());
    format!("http://127.0.0.1:{port}")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => match fetch_json(&format!("{}/api/status", base_url())).await {
            Ok(v) => println!("{}", render_status_line(&v)),
            Err(_) => println!("🛡 fornax: daemon unreachable"),
        },
        Commands::Detail => {
            match fetch_json(&format!("{}/api/findings/recent", base_url())).await {
                Ok(v) => print_detail(&v),
                Err(_) => println!("fornax: daemon unreachable (is `fornax-daemon` running?)"),
            }
        }
        Commands::ExportSpool { session, out } => export_spool(&session, &out).await?,
    }
    Ok(())
}

fn fornax_home() -> std::path::PathBuf {
    std::env::var("FORNAX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs_home().join(".fornax"))
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Writes one envelope JSON file into `<out>/pending/<id>.json`, internally
/// tagged with `"type"` alongside the value's own fields — matching how
/// `#[serde(tag = "type")]` on a newtype-of-struct enum variant serializes,
/// without depending on that enum type directly (it lives in a separate
/// repo/crate; fornax-core intentionally has no dependency on fornax-cloud).
fn write_envelope(
    pending_dir: &std::path::Path,
    type_tag: &str,
    id: uuid::Uuid,
    value: &impl serde::Serialize,
) -> anyhow::Result<()> {
    let mut obj = serde_json::to_value(value)?;
    obj.as_object_mut()
        .expect("envelope payload must serialize to a JSON object")
        .insert(
            "type".to_string(),
            serde_json::Value::String(type_tag.to_string()),
        );
    let final_path = pending_dir.join(format!("{id}.json"));
    let tmp_path = pending_dir.join(format!("{id}.json.tmp"));
    std::fs::write(&tmp_path, serde_json::to_vec(&obj)?)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

async fn export_spool(session: &str, out: &std::path::Path) -> anyhow::Result<()> {
    let db_path = fornax_home().join("fornax.db");
    let store = fornax_store::Store::open(&db_path).await?;

    let events = store.events_for_session(session).await?;
    let claims = store.claims_for_session(session).await?;
    let evidence = store.evidence_for_session(session).await?;

    let pending_dir = out.join("pending");
    std::fs::create_dir_all(&pending_dir)?;

    for e in &events {
        write_envelope(&pending_dir, "event", e.id, e)?;
    }
    for c in &claims {
        write_envelope(&pending_dir, "claim", c.id, c)?;
    }
    for ev in &evidence {
        write_envelope(&pending_dir, "evidence", ev.id, ev)?;
    }

    println!(
        "exported session {session}: {} event(s), {} claim(s), {} evidence -> {}",
        events.len(),
        claims.len(),
        evidence.len(),
        pending_dir.display()
    );
    Ok(())
}

async fn fetch_json(url: &str) -> anyhow::Result<serde_json::Value> {
    Ok(reqwest::get(url).await?.json::<serde_json::Value>().await?)
}

fn render_status_line(v: &serde_json::Value) -> String {
    let Some(latest) = v.get("latest").filter(|l| !l.is_null()) else {
        return "🛡 fornax: no findings yet".to_string();
    };
    let verdict = latest
        .get("verdict")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
    let icon = verdict_icon(verdict);
    format!("{icon} {}", verdict.to_uppercase())
}

fn verdict_icon(verdict: &str) -> &'static str {
    match verdict {
        "verified" => "🛡 ✓",
        "contradicted" => "🛡 ✕",
        "unverified" => "🛡 ?",
        "review" => "🛡 !",
        "unavailable" => "🛡 —",
        _ => "🛡",
    }
}

fn print_detail(v: &serde_json::Value) {
    let empty = vec![];
    let findings = v
        .get("findings")
        .and_then(|f| f.as_array())
        .unwrap_or(&empty);
    if findings.is_empty() {
        println!("No findings recorded yet.");
        return;
    }
    for f in findings {
        let verdict = f.get("verdict").and_then(|s| s.as_str()).unwrap_or("?");
        let claim = f.get("claim_text").and_then(|s| s.as_str()).unwrap_or("");
        let rationale = f.get("rationale").and_then(|s| s.as_str()).unwrap_or("");
        let verifier = f
            .get("verifier_name")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let when = f.get("computed_at").and_then(|s| s.as_str()).unwrap_or("");
        println!("{} {}", verdict_icon(verdict), verdict.to_uppercase());
        println!("  claim:     {claim}");
        println!("  rationale: {rationale}");
        println!("  verifier:  {verifier}");
        println!("  when:      {when}");
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_status_line_shows_no_findings_message_when_null() {
        let v = serde_json::json!({"latest": null});
        assert_eq!(render_status_line(&v), "🛡 fornax: no findings yet");
    }

    #[test]
    fn render_status_line_shows_contradicted_icon_and_verdict() {
        let v = serde_json::json!({"latest": {"verdict": "contradicted"}});
        assert_eq!(render_status_line(&v), "🛡 ✕ CONTRADICTED");
    }

    #[test]
    fn render_status_line_shows_verified_icon() {
        let v = serde_json::json!({"latest": {"verdict": "verified"}});
        assert_eq!(render_status_line(&v), "🛡 ✓ VERIFIED");
    }

    #[test]
    fn verdict_icon_covers_all_five_states_never_collapsing() {
        assert_eq!(verdict_icon("verified"), "🛡 ✓");
        assert_eq!(verdict_icon("unverified"), "🛡 ?");
        assert_eq!(verdict_icon("contradicted"), "🛡 ✕");
        assert_eq!(verdict_icon("review"), "🛡 !");
        assert_eq!(verdict_icon("unavailable"), "🛡 —");
    }
}
