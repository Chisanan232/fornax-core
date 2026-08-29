//! Local immutable evidence store (FORNX-26). SQLite in WAL mode. Rows are
//! inserted, never mutated — sessions can be replayed against future
//! verifiers (FORNX-49) from this store alone, with no network/adapter
//! dependency.

use fornax_types::{AgentEvent, Claim, Evidence, Finding};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Serialize a simple string-like enum (serde `rename_all = "snake_case"`) to
/// its bare tag, e.g. `Verdict::Contradicted` -> `"contradicted"`, not the
/// JSON-quoted `"\"contradicted\""` that plain `to_string()` would produce.
fn tag(v: &impl serde::Serialize) -> Result<String> {
    match serde_json::to_value(v)? {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

fn from_tag<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_string(),
    ))?)
}

/// `<path>` with `suffix` appended to the file name, e.g.
/// `append_ext("a/db.sqlite", "-wal")` -> `a/db.sqlite-wal` (SQLite's own
/// naming convention for WAL-mode sidecar files, not a `.` extension).
#[cfg(unix)]
fn append_ext(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

/// Best-effort 0600 chmod. Silently does nothing if the file doesn't exist
/// yet (WAL/SHM sidecars may not be created until the first checkpoint) or
/// the chmod fails for another reason — never block store startup on this.
#[cfg(unix)]
fn chmod_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if absent) the SQLite store at `path` in WAL mode, and
    /// run migrations. On Unix, the file is created with 0600 permissions —
    /// evidence payloads may carry secrets from raw tool output (see
    /// docs/research/adapter-capability-matrix.md), so the store must never
    /// default to world-readable (FORNX-33 acceptance).
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(StoreError::Db)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| StoreError::Db(e.into()))?;

        #[cfg(unix)]
        {
            // In WAL mode the most recently written rows live in the
            // `-wal` sidecar (and its `-shm` index) until a checkpoint, not
            // in the main file — chmod all three or the 0600 guarantee
            // above is defeated for exactly the newest evidence/claims.
            for sidecar in [
                path.to_path_buf(),
                append_ext(path, "-wal"),
                append_ext(path, "-shm"),
            ] {
                chmod_owner_only(&sidecar);
            }
        }

        Ok(Self { pool })
    }

    pub async fn insert_event(&self, e: &AgentEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO agent_events (id, session_id, provider, kind, observed_at, tool_name, tool_input, tool_response, raw)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(e.id.to_string())
        .bind(&e.session_id)
        .bind(tag(&e.provider)?)
        .bind(tag(&e.kind)?)
        .bind(&e.observed_at)
        .bind(&e.tool_name)
        .bind(e.tool_input.as_ref().map(|v| v.to_string()))
        .bind(e.tool_response.as_ref().map(|v| v.to_string()))
        .bind(e.raw.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_claim(&self, c: &Claim) -> Result<()> {
        sqlx::query(
            "INSERT INTO claims (id, session_id, source_event_id, text, subject, claimed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(c.id.to_string())
        .bind(&c.session_id)
        .bind(c.source_event_id.to_string())
        .bind(&c.text)
        .bind(&c.subject)
        .bind(&c.claimed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_evidence(&self, ev: &Evidence) -> Result<()> {
        sqlx::query(
            "INSERT INTO evidence (id, session_id, source_event_id, kind, observed_at, payload, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(ev.id.to_string())
        .bind(&ev.session_id)
        .bind(ev.source_event_id.to_string())
        .bind(tag(&ev.kind)?)
        .bind(&ev.observed_at)
        .bind(ev.payload.to_string())
        .bind(&ev.provenance)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_finding(&self, f: &Finding) -> Result<()> {
        sqlx::query(
            "INSERT INTO findings (id, claim_id, verdict, evidence_ids, verifier_name, rationale, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(f.id.to_string())
        .bind(f.claim_id.to_string())
        .bind(tag(&f.verdict)?)
        .bind(serde_json::to_string(&f.evidence_ids)?)
        .bind(&f.verifier_name)
        .bind(&f.rationale)
        .bind(&f.computed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// All evidence rows for a session, oldest first — the input a verifier
    /// needs alongside a claim.
    pub async fn evidence_for_session(&self, session_id: &str) -> Result<Vec<Evidence>> {
        let rows = sqlx::query_as::<_, EvidenceRow>(
            "SELECT id, session_id, source_event_id, kind, observed_at, payload, provenance
             FROM evidence WHERE session_id = ?1 ORDER BY observed_at ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Most recent findings across all sessions, for the localhost dashboard
    /// (FORNX-32) and detail command (FORNX-31).
    pub async fn recent_findings(&self, limit: i64) -> Result<Vec<FindingRow>> {
        let rows = sqlx::query_as::<_, FindingRow>(
            "SELECT f.id, f.claim_id, f.verdict, f.evidence_ids, f.verifier_name, f.rationale, f.computed_at,
                    c.text as claim_text, c.session_id as session_id
             FROM findings f JOIN claims c ON c.id = f.claim_id
             ORDER BY f.computed_at DESC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

#[derive(sqlx::FromRow)]
struct EvidenceRow {
    id: String,
    session_id: String,
    source_event_id: String,
    kind: String,
    observed_at: String,
    payload: String,
    provenance: String,
}

impl TryFrom<EvidenceRow> for Evidence {
    type Error = StoreError;
    fn try_from(r: EvidenceRow) -> Result<Self> {
        Ok(Evidence {
            id: uuid::Uuid::parse_str(&r.id).unwrap_or_default(),
            session_id: r.session_id,
            source_event_id: uuid::Uuid::parse_str(&r.source_event_id).unwrap_or_default(),
            kind: from_tag(&r.kind)?,
            observed_at: r.observed_at,
            payload: serde_json::from_str(&r.payload)?,
            provenance: r.provenance,
        })
    }
}

/// Denormalized finding + claim text, for read-only display surfaces (status
/// line, detail command, dashboard). Not the canonical `Finding` type.
#[derive(sqlx::FromRow, serde::Serialize)]
pub struct FindingRow {
    pub id: String,
    pub claim_id: String,
    pub verdict: String,
    pub evidence_ids: String,
    pub verifier_name: String,
    pub rationale: String,
    pub computed_at: String,
    pub claim_text: String,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fornax_types::{EventKind, EvidenceKind, Provider, Verdict};
    use uuid::Uuid;

    fn tmp_db_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fornax-store-test-{name}-{}.db", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn fresh_db_creates_with_owner_only_permissions() {
        let path = tmp_db_path("perms");
        let _store = Store::open(&path).await.expect("open fresh db");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "DB file must not be group/world readable");
        }
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn event_evidence_claim_finding_round_trip() {
        let path = tmp_db_path("roundtrip");
        let store = Store::open(&path).await.expect("open db");

        let event = AgentEvent {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            provider: Provider::Codex,
            kind: EventKind::PostToolUse,
            observed_at: "2026-01-01T00:00:00Z".into(),
            tool_name: Some("exec_command".into()),
            tool_input: Some(serde_json::json!(["pytest"])),
            tool_response: Some(serde_json::json!({"exit_code": 1})),
            raw: serde_json::json!({"type": "exec_command_end"}),
        };
        store.insert_event(&event).await.expect("insert event");

        let evidence = Evidence {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            source_event_id: event.id,
            kind: EvidenceKind::ExitCode,
            observed_at: "2026-01-01T00:00:01Z".into(),
            payload: serde_json::json!({"command": ["pytest"], "exit_code": 1}),
            provenance: "test".into(),
        };
        store
            .insert_evidence(&evidence)
            .await
            .expect("insert evidence");

        let claim = Claim {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            source_event_id: event.id,
            text: "All tests passed.".into(),
            subject: "test_result".into(),
            claimed_at: "2026-01-01T00:00:02Z".into(),
        };
        store.insert_claim(&claim).await.expect("insert claim");

        let finding = Finding {
            id: Uuid::new_v4(),
            claim_id: claim.id,
            verdict: Verdict::Contradicted,
            evidence_ids: vec![evidence.id],
            verifier_name: "test_result_verifier_v1".into(),
            rationale: "exit_code=1".into(),
            computed_at: "2026-01-01T00:00:03Z".into(),
        };
        store
            .insert_finding(&finding)
            .await
            .expect("insert finding");

        let fetched_evidence = store
            .evidence_for_session("s1")
            .await
            .expect("query evidence");
        assert_eq!(fetched_evidence.len(), 1);
        assert_eq!(fetched_evidence[0].id, evidence.id);
        assert_eq!(fetched_evidence[0].payload["exit_code"], 1);

        let recent = store.recent_findings(10).await.expect("query findings");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].verdict, "contradicted");
        assert_eq!(recent[0].claim_text, "All tests passed.");

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn reopening_existing_db_preserves_prior_data() {
        let path = tmp_db_path("restart");
        {
            let store = Store::open(&path).await.expect("open db first time");
            let event = AgentEvent {
                id: Uuid::new_v4(),
                session_id: "s2".into(),
                provider: Provider::ClaudeCode,
                kind: EventKind::SessionStart,
                observed_at: "2026-01-01T00:00:00Z".into(),
                tool_name: None,
                tool_input: None,
                tool_response: None,
                raw: serde_json::json!({}),
            };
            store.insert_event(&event).await.expect("insert event");
        }
        // Simulate a daemon restart: reopen the same path.
        let store = Store::open(&path).await.expect("reopen db after restart");
        let evidence = store
            .evidence_for_session("s2")
            .await
            .expect("query after restart");
        assert!(
            evidence.is_empty(),
            "no evidence was inserted, only an event"
        );

        std::fs::remove_file(&path).ok();
    }
}
