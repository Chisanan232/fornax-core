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
    /// Export one session's events/claims/evidence/capabilities from the
    /// local store into a directory-based spool, as one wire-compatible
    /// envelope JSON file per message (FORNX-60, FORNX-62). Reads
    /// `$FORNAX_HOME/fornax.db` directly — no daemon dependency, so this
    /// also works while the daemon is stopped.
    ///
    /// Written to `<out>/pending/<id>.json`, matching the layout a consumer
    /// such as fornax-cloud's uploader spool expects: one JSON object per
    /// file, internally tagged with a `"type"` field of `"event"`,
    /// `"claim"`, `"evidence"`, or `"capabilities"`. A `capabilities` file is
    /// only emitted when the session has at least one announcement on
    /// record (FORNX-62) — a session with none produces the same 3-category
    /// output as before this ticket.
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
    export_spool_from_store(&store, session, out).await
}

/// Does the actual read + write work for `export_spool`, taking an
/// already-open `Store` so it can be exercised in tests without touching
/// `$FORNAX_HOME` (which is process-global and unsafe to mutate per-test).
async fn export_spool_from_store(
    store: &fornax_store::Store,
    session: &str,
    out: &std::path::Path,
) -> anyhow::Result<()> {
    let events = store.events_for_session(session).await?;
    let claims = store.claims_for_session(session).await?;
    let evidence_read = store.evidence_for_session(session).await?;
    if !evidence_read.failed.is_empty() {
        eprintln!(
            "fornax: {} of {} evidence rows for session {session} failed to deserialize and were not exported: {}",
            evidence_read.failed.len(),
            evidence_read.evidence.len() + evidence_read.failed.len(),
            evidence_read
                .failed
                .iter()
                .map(|f| format!("{} ({})", f.id, f.error))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let evidence = evidence_read.evidence;
    let capabilities = store.capabilities_for_session(session).await?;

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
    for caps in &capabilities {
        // RuntimeCapabilities carries no id of its own on the wire (the
        // cloud backend keys these on (device_id, provider), never on an
        // envelope id — see fornax-cloud's fornax-uploader::types::IngestMessage
        // ::canonical_id doc comment) — synthesize one purely for the spool
        // filename, matching that same convention.
        //
        // FORNX-155: the domain `RuntimeCapabilities` now carries
        // `schema_version`/`signals`, which fornax-cloud's
        // `fornax-uploader::types::RuntimeCapabilities` (a separate,
        // out-of-scope repo) does not know about. Project to the frozen
        // flat-bool wire shape at the export boundary so the spool envelope
        // stays byte-for-byte wire-compatible — see
        // `fornax_types::capabilities::LegacyCapabilitiesWire`'s doc comment.
        let legacy = fornax_types::LegacyCapabilitiesWire::from(caps);
        write_envelope(&pending_dir, "capabilities", uuid::Uuid::new_v4(), &legacy)?;
    }

    println!(
        "exported session {session}: {} event(s), {} claim(s), {} evidence, {} capabilities -> {}",
        events.len(),
        claims.len(),
        evidence.len(),
        capabilities.len(),
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

    // FORNX-62: export-spool must emit a `capabilities` envelope for a
    // session that received a real Capabilities message, using the FORNX-53
    // aha-scenario fixture pattern (`caps()`/claim-with-exit-code style)
    // already established in fornax-verify's test suite.
    mod export_spool_capabilities {
        use super::*;
        use fornax_types::{
            CapabilitySignal, EventKind, EvidenceKind, Provider, RuntimeCapabilities,
            SignalAvailability, SignalClass,
        };
        use uuid::Uuid;

        fn tmp_db_path(name: &str) -> std::path::PathBuf {
            std::env::temp_dir().join(format!("fornax-cli-test-{name}-{}.db", Uuid::new_v4()))
        }

        fn aha_scenario_capabilities() -> RuntimeCapabilities {
            RuntimeCapabilities {
                schema_version: fornax_types::CAPABILITY_SCHEMA_VERSION,
                provider: Provider::Codex,
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
                        state: SignalAvailability::Unsupported,
                        detail: None,
                    },
                ],
                notes: [("session_id".to_string(), "s-aha".to_string())].into(),
            }
        }

        async fn seeded_store(path: &std::path::Path) -> fornax_store::Store {
            let store = fornax_store::Store::open(path).await.expect("open db");

            let event = fornax_types::AgentEvent {
                id: Uuid::new_v4(),
                session_id: "s-aha".into(),
                provider: Provider::Codex,
                kind: EventKind::PostToolUse,
                observed_at: "2026-01-01T00:00:00Z".into(),
                tool_name: Some("exec_command".into()),
                tool_input: Some(serde_json::json!(["pytest"])),
                tool_response: Some(serde_json::json!({"exit_code": 1})),
                raw: serde_json::json!({"type": "exec_command_end"}),
            };
            store.insert_event(&event).await.expect("insert event");

            let evidence = fornax_types::Evidence {
                id: Uuid::new_v4(),
                session_id: "s-aha".into(),
                source_event_id: event.id,
                kind: EvidenceKind::ExitCode,
                observed_at: "2026-01-01T00:00:01Z".into(),
                payload: serde_json::json!({"command": ["pytest"], "exit_code": 1}),
                provenance: "codex:rollout:exec_command_end".into(),
                source: None,
                extension: None,
            };
            store
                .insert_evidence(&evidence)
                .await
                .expect("insert evidence");

            let claim = fornax_types::Claim {
                id: Uuid::new_v4(),
                session_id: "s-aha".into(),
                source_event_id: event.id,
                text: "All tests passed.".into(),
                subject: "test_result".into(),
                claimed_at: "2026-01-01T00:00:02Z".into(),
            };
            store.insert_claim(&claim).await.expect("insert claim");

            store
        }

        fn read_pending_types(pending_dir: &std::path::Path) -> Vec<String> {
            std::fs::read_dir(pending_dir)
                .expect("read pending dir")
                .map(|e| e.expect("dir entry"))
                .map(|e| std::fs::read_to_string(e.path()).expect("read envelope file"))
                .map(|contents| {
                    let v: serde_json::Value =
                        serde_json::from_str(&contents).expect("envelope is valid json");
                    v["type"].as_str().expect("type field present").to_string()
                })
                .collect()
        }

        #[tokio::test]
        async fn emits_capabilities_envelope_when_announced() {
            let db_path = tmp_db_path("with-caps");
            let store = seeded_store(&db_path).await;
            store
                .upsert_capabilities("s-aha", &aha_scenario_capabilities())
                .await
                .expect("upsert capabilities");

            let out_dir = std::env::temp_dir().join(format!("fornax-spool-{}", Uuid::new_v4()));
            export_spool_from_store(&store, "s-aha", &out_dir)
                .await
                .expect("export spool");

            let pending_dir = out_dir.join("pending");
            let types = read_pending_types(&pending_dir);
            assert_eq!(
                types.iter().filter(|t| *t == "capabilities").count(),
                1,
                "expected exactly one capabilities envelope, got: {types:?}"
            );
            assert!(types.contains(&"event".to_string()));
            assert!(types.contains(&"claim".to_string()));
            assert!(types.contains(&"evidence".to_string()));

            // The emitted capabilities file must be wire-compatible with
            // fornax-cloud's fornax-uploader::types::RuntimeCapabilities:
            // the flat field set below, plus "type" — no extra fields such
            // as a store-internal session_id/id (the cloud backend keys
            // capabilities on (device_id, provider), never on an envelope
            // id — see that crate's IngestMessage::canonical_id doc).
            let caps_file = std::fs::read_dir(&pending_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    let v: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
                    v["type"] == "capabilities"
                })
                .expect("capabilities file exists");
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&caps_file).unwrap()).unwrap();
            let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec![
                    "notes",
                    "provider",
                    "supports_post_tool_use",
                    "supports_pre_tool_use",
                    "supports_session_stop_event",
                    "supports_subagent_lifecycle",
                    "supports_tool_response_capture",
                    "supports_transcript_tail",
                    "type",
                ]
            );

            std::fs::remove_file(&db_path).ok();
            std::fs::remove_dir_all(&out_dir).ok();
        }

        #[tokio::test]
        async fn no_capabilities_envelope_when_none_announced() {
            let db_path = tmp_db_path("without-caps");
            let store = seeded_store(&db_path).await;
            // No `upsert_capabilities` call — this session never announced.

            let out_dir = std::env::temp_dir().join(format!("fornax-spool-{}", Uuid::new_v4()));
            export_spool_from_store(&store, "s-aha", &out_dir)
                .await
                .expect("export spool");

            let pending_dir = out_dir.join("pending");
            let types = read_pending_types(&pending_dir);
            assert!(
                !types.contains(&"capabilities".to_string()),
                "expected no capabilities envelope, got: {types:?}"
            );
            assert!(types.contains(&"event".to_string()));

            std::fs::remove_file(&db_path).ok();
            std::fs::remove_dir_all(&out_dir).ok();
        }

        /// FORNX-157 AC: "Provenance/trust metadata survives ... cloud-safe
        /// projection." Unlike `RuntimeCapabilities` (which projects through
        /// `LegacyCapabilitiesWire` to a frozen bool set for wire-compat with
        /// an out-of-repo consumer), `Evidence` has no such frozen-shape
        /// contract — it is spooled as-is (see `export_spool_from_store`).
        /// So "cloud-safe projection" for `Evidence::source` means: the
        /// structured metadata ships through unmodified, not stripped.
        #[tokio::test]
        async fn evidence_envelope_carries_source_metadata_through_export() {
            let db_path = tmp_db_path("evidence-source-export");
            let store = fornax_store::Store::open(&db_path).await.expect("open db");

            let event = fornax_types::AgentEvent {
                id: Uuid::new_v4(),
                session_id: "s-source".into(),
                provider: Provider::Codex,
                kind: EventKind::PostToolUse,
                observed_at: "2026-01-01T00:00:00Z".into(),
                tool_name: Some("exec_command".into()),
                tool_input: Some(serde_json::json!(["pytest"])),
                tool_response: Some(serde_json::json!({"exit_code": 0})),
                raw: serde_json::json!({"type": "exec_command_end"}),
            };
            store.insert_event(&event).await.expect("insert event");

            let evidence = fornax_types::Evidence {
                id: Uuid::new_v4(),
                session_id: "s-source".into(),
                source_event_id: event.id,
                kind: EvidenceKind::ExitCode,
                observed_at: "2026-01-01T00:00:01Z".into(),
                payload: serde_json::json!({"command": ["pytest"], "exit_code": 0}),
                provenance: "codex:rollout:exec_command_end".into(),
                source: Some(fornax_types::EvidenceSource {
                    sensor_name: "codex_exec_command_end_sensor_v1".into(),
                    trust_class: fornax_types::TrustClass::AgentAdjacent,
                    collected_at: "2026-01-01T00:00:01Z".into(),
                    provider: Some(Provider::Codex),
                    collection_method: fornax_types::CollectionMethod::FilePoll,
                    collector_version: Some("codex-adapter-0.1.0".into()),
                    freshness: fornax_types::Freshness {
                        clock_source: fornax_types::ClockSource::HostClock,
                        caveat: None,
                    },
                    tamper_boundary: fornax_types::TamperBoundary::for_trust_class(
                        &fornax_types::TrustClass::AgentAdjacent,
                        &fornax_types::CollectionMethod::FilePoll,
                    ),
                }),
                extension: None,
            };
            store
                .insert_evidence(&evidence)
                .await
                .expect("insert evidence");

            let out_dir = std::env::temp_dir().join(format!("fornax-spool-{}", Uuid::new_v4()));
            export_spool_from_store(&store, "s-source", &out_dir)
                .await
                .expect("export spool");

            let pending_dir = out_dir.join("pending");
            let evidence_file = std::fs::read_dir(&pending_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    let v: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
                    v["type"] == "evidence"
                })
                .expect("evidence file exists");
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&evidence_file).unwrap()).unwrap();
            assert_eq!(
                v["source"]["sensor_name"], "codex_exec_command_end_sensor_v1",
                "structured sensor/trust metadata must survive the spool export projection"
            );
            assert_eq!(v["source"]["trust_class"], "agent_adjacent");
            assert_eq!(v["source"]["provider"], "codex");
            // FORNX-159: collection_method/collector_version/freshness/
            // tamper_boundary must survive the same cloud-safe projection
            // boundary as the FORNX-157 fields above.
            assert_eq!(v["source"]["collection_method"], "file_poll");
            assert_eq!(v["source"]["collector_version"], "codex-adapter-0.1.0");
            assert_eq!(v["source"]["freshness"]["clock_source"], "host_clock");
            assert!(v["source"]["tamper_boundary"]["description"].is_string());

            std::fs::remove_file(&db_path).ok();
            std::fs::remove_dir_all(&out_dir).ok();
        }

        /// FORNX-158 AC: provider-extension data must survive the same
        /// cloud-safe projection boundary as `EvidenceSource` above —
        /// `Evidence` spools as-is, so the extension envelope (including a
        /// preserved unknown field) should ship through unmodified.
        #[tokio::test]
        async fn evidence_envelope_carries_extension_data_through_export() {
            let db_path = tmp_db_path("evidence-extension-export");
            let store = fornax_store::Store::open(&db_path).await.expect("open db");

            let event = fornax_types::AgentEvent {
                id: Uuid::new_v4(),
                session_id: "s-ext".into(),
                provider: Provider::ClaudeCode,
                kind: EventKind::PostToolUse,
                observed_at: "2026-01-01T00:00:00Z".into(),
                tool_name: Some("Bash".into()),
                tool_input: Some(serde_json::json!({"command": ["pytest"]})),
                tool_response: Some(serde_json::json!({"stdout": ""})),
                raw: serde_json::json!({"hook_event_name": "PostToolUse"}),
            };
            store.insert_event(&event).await.expect("insert event");

            let mut extension = fornax_types::ExtensionEnvelope::new(
                Provider::ClaudeCode,
                "claude-adapter-0.3.0",
                fornax_types::ContentClass::ToolTelemetry,
                serde_json::json!({"cache_read_tokens": 128}),
            );
            // Prove unknown-field preservation across the persistence +
            // export boundary, not just an in-memory round trip.
            extension
                .unknown
                .insert("retention_hint_days".into(), serde_json::json!(30));

            let evidence = fornax_types::Evidence {
                id: Uuid::new_v4(),
                session_id: "s-ext".into(),
                source_event_id: event.id,
                kind: EvidenceKind::ExitCode,
                observed_at: "2026-01-01T00:00:01Z".into(),
                payload: serde_json::json!({"command": ["pytest"], "exit_code": 0}),
                provenance: "claude_code:PostToolUse:Bash#heuristic:stderr_empty".into(),
                source: None,
                extension: Some(extension),
            };
            store
                .insert_evidence(&evidence)
                .await
                .expect("insert evidence");

            let out_dir = std::env::temp_dir().join(format!("fornax-spool-{}", Uuid::new_v4()));
            export_spool_from_store(&store, "s-ext", &out_dir)
                .await
                .expect("export spool");

            let pending_dir = out_dir.join("pending");
            let evidence_file = std::fs::read_dir(&pending_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    let v: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
                    v["type"] == "evidence"
                })
                .expect("evidence file exists");
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&evidence_file).unwrap()).unwrap();
            assert_eq!(
                v["extension"]["schema_version"],
                fornax_types::EXTENSION_SCHEMA_VERSION
            );
            assert_eq!(v["extension"]["content_class"], "tool_telemetry");
            assert_eq!(v["extension"]["fields"]["cache_read_tokens"], 128);
            assert_eq!(
                v["extension"]["retention_hint_days"], 30,
                "unknown extension field must survive the store + export round trip, not be dropped"
            );

            std::fs::remove_file(&db_path).ok();
            std::fs::remove_dir_all(&out_dir).ok();
        }
    }
}
