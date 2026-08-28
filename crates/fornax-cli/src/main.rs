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
    }
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
