//! Local policy cache persistence (FORNX-119). SQLite-backed, mirroring
//! `retention.rs`'s precedent for store-crate persistence of a
//! `fornax-types` domain model: the pure decision logic
//! (`fornax_types::policy::cache::evaluate_activation`) lives in
//! `fornax-types`; this module is the sole executor of that decision
//! against real rows, and the sole place a policy bundle's envelope bytes
//! are written or read back.
//!
//! See `docs/adr/0008-local-policy-cache-and-activation.md` for the full
//! crash-safety argument: [`Store::submit_policy_bundle`] performs its
//! reads and writes inside one `BEGIN IMMEDIATE` transaction, committed
//! only once every row is written. A crash before commit leaves the
//! previous generation wholly intact (SQLite's own atomicity — an
//! uncommitted transaction is rolled back when the connection is dropped);
//! a crash after commit leaves the new one wholly intact. There is no
//! third state.

use chrono::{DateTime, Utc};
use fornax_types::{
    verify_bundle, ActivationDecision, ActivationOutcome, ActivationRejection, BoundRevision,
    BundleRejection, CacheGeneration, CacheSlotKind, CachedBundleRef, KeyId, PayloadDigest,
    PolicyCacheState, PolicyDiagnostic, PolicyId, RevisionDigest, SequenceHighWater,
    TrustedVerificationKeys, POLICY_CACHE_SCHEMA_VERSION,
};
use fornax_types::{DiagnosticCode, DiagnosticSeverity};
use sqlx::sqlite::SqliteConnection;
use uuid::Uuid;

use crate::{from_tag, tag, Result, Store, StoreError};

/// Result of [`Store::load_policy_cache`] -- purely local, no network call
/// on any path.
#[derive(Debug, Clone)]
pub struct PolicyCacheLoad {
    pub state: PolicyCacheState,
    pub usable: Vec<BoundRevision>,
    pub loaded_slot: Option<CacheSlotKind>,
    pub diagnostics: Vec<PolicyDiagnostic>,
}

fn unavailable_diagnostic(detail: impl Into<String>) -> PolicyDiagnostic {
    PolicyDiagnostic::new(
        DiagnosticCode::PolicyCacheUnavailable,
        DiagnosticSeverity::Warning,
        detail.into(),
        "import a policy bundle via `fornax policy import <path>`, or configure a trust store \
         so a previously-imported bundle can be re-verified",
    )
}

fn unverifiable_diagnostic(bundle_id: Uuid, detail: impl Into<String>) -> PolicyDiagnostic {
    PolicyDiagnostic::new(
        DiagnosticCode::PolicyCacheUnverifiable,
        DiagnosticSeverity::Warning,
        format!(
            "cached bundle {bundle_id} failed re-verification: {}",
            detail.into()
        ),
        "republish and re-import a bundle signed by a currently-trusted key",
    )
}

fn parse_rfc3339(field: &'static str, value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StoreError::PolicyCacheCorrupt(format!("{field} {value:?}: {e}")))
}

#[derive(sqlx::FromRow)]
struct SlotsRow {
    schema_version: i64,
    active_generation: Option<i64>,
    #[allow(dead_code)]
    pending_generation: Option<i64>,
    last_known_good_generation: Option<i64>,
    ever_configured: i64,
}

#[derive(sqlx::FromRow)]
struct GenerationRow {
    written_at: String,
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    bundle_id: String,
    issuer: String,
    sequence: i64,
    policy_id: String,
    revision: i64,
    revision_digest: String,
    payload_digest: String,
    verified_by: String,
    not_before: String,
    expires_at: String,
    first_activated_at: String,
    confirmed_at: String,
}

impl MemberRow {
    fn into_cached_bundle_ref(self) -> Result<CachedBundleRef> {
        Ok(CachedBundleRef {
            bundle_id: Uuid::parse_str(&self.bundle_id)
                .map_err(|e| StoreError::PolicyCacheCorrupt(e.to_string()))?,
            issuer: self.issuer,
            sequence: self.sequence as u64,
            policy_id: PolicyId(
                Uuid::parse_str(&self.policy_id)
                    .map_err(|e| StoreError::PolicyCacheCorrupt(e.to_string()))?,
            ),
            revision: self.revision as u32,
            revision_digest: from_tag::<RevisionDigest>(&self.revision_digest)?,
            payload_digest: from_tag::<PayloadDigest>(&self.payload_digest)?,
            verified_by: KeyId(self.verified_by),
            not_before: parse_rfc3339("not_before", &self.not_before)?,
            expires_at: parse_rfc3339("expires_at", &self.expires_at)?,
            first_activated_at: parse_rfc3339("first_activated_at", &self.first_activated_at)?,
            confirmed_at: parse_rfc3339("confirmed_at", &self.confirmed_at)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct HighWaterRow {
    issuer: String,
    policy_id: String,
    max_sequence: i64,
    last_bundle_id: String,
    last_payload_digest: String,
    last_seen_at: String,
}

impl HighWaterRow {
    fn into_high_water(self) -> Result<((String, PolicyId), SequenceHighWater)> {
        let policy_id = PolicyId(
            Uuid::parse_str(&self.policy_id)
                .map_err(|e| StoreError::PolicyCacheCorrupt(e.to_string()))?,
        );
        Ok((
            (self.issuer.clone(), policy_id),
            SequenceHighWater {
                issuer: self.issuer,
                policy_id,
                max_sequence: self.max_sequence as u64,
                last_bundle_id: Uuid::parse_str(&self.last_bundle_id)
                    .map_err(|e| StoreError::PolicyCacheCorrupt(e.to_string()))?,
                last_payload_digest: from_tag::<PayloadDigest>(&self.last_payload_digest)?,
                last_seen_at: parse_rfc3339("last_seen_at", &self.last_seen_at)?,
            },
        ))
    }
}

/// `policy_cache_generation_members` rows for one generation, unjoined --
/// this is the authoritative member list. Used alongside the `JOIN` below to
/// detect a dangling member reference (a member row whose `bundle_id`/
/// `payload_digest` no longer resolves to a stored bundle row) that an
/// `INNER JOIN` alone would silently drop instead of surfacing as corrupt.
/// This can only arise from direct database tampering/corruption --
/// `Store::submit_policy_bundle` always writes the bundle row before the
/// member row, in the same transaction -- but a generation must still never
/// be served as if that member simply didn't exist (see this module's doc
/// comment: "a generation is never served partially loaded").
#[derive(sqlx::FromRow)]
struct RawMemberRow {
    policy_id: String,
    bundle_id: String,
    payload_digest: String,
}

async fn load_generation(conn: &mut SqliteConnection, generation: i64) -> Result<CacheGeneration> {
    let row = sqlx::query_as::<_, GenerationRow>(
        "SELECT written_at FROM policy_cache_generations WHERE generation = ?1",
    )
    .bind(generation)
    .fetch_one(&mut *conn)
    .await?;

    let raw_rows = sqlx::query_as::<_, RawMemberRow>(
        "SELECT policy_id, bundle_id, payload_digest FROM policy_cache_generation_members
         WHERE generation = ?1 ORDER BY policy_id ASC",
    )
    .bind(generation)
    .fetch_all(&mut *conn)
    .await?;

    let member_rows = sqlx::query_as::<_, MemberRow>(
        "SELECT b.bundle_id, b.issuer, b.sequence, b.policy_id, b.revision, b.revision_digest,
                b.payload_digest, b.verified_by, b.not_before, b.expires_at,
                b.first_activated_at, b.confirmed_at
         FROM policy_cache_generation_members m
         JOIN policy_cache_bundles b
           ON b.bundle_id = m.bundle_id AND b.payload_digest = m.payload_digest
         WHERE m.generation = ?1
         ORDER BY b.policy_id ASC",
    )
    .bind(generation)
    .fetch_all(&mut *conn)
    .await?;

    let mut resolved: std::collections::HashMap<(String, String), MemberRow> = member_rows
        .into_iter()
        .map(|r| ((r.bundle_id.clone(), r.payload_digest.clone()), r))
        .collect();

    let mut members = Vec::with_capacity(raw_rows.len());
    for raw in raw_rows {
        let key = (raw.bundle_id.clone(), raw.payload_digest.clone());
        match resolved.remove(&key) {
            Some(resolved_row) => members.push(resolved_row.into_cached_bundle_ref()?),
            None => {
                // Dangling reference: no bundle row for this member. Stand
                // in a ref carrying only the fields we actually have --
                // `bundle_id`/`payload_digest` are correct, so
                // `try_generation_usable`'s own envelope lookup by those two
                // keys will correctly find nothing and reject this member
                // (and therefore the whole generation) exactly as it would
                // for a member whose envelope bytes went missing any other
                // way. The remaining fields are placeholders, never read on
                // this path.
                members.push(CachedBundleRef {
                    bundle_id: Uuid::parse_str(&raw.bundle_id)
                        .map_err(|e| StoreError::PolicyCacheCorrupt(e.to_string()))?,
                    issuer: String::new(),
                    sequence: 0,
                    policy_id: PolicyId(
                        Uuid::parse_str(&raw.policy_id)
                            .map_err(|e| StoreError::PolicyCacheCorrupt(e.to_string()))?,
                    ),
                    revision: 0,
                    revision_digest: from_tag::<RevisionDigest>(&format!(
                        "sha256:{}",
                        "0".repeat(64)
                    ))?,
                    payload_digest: from_tag::<PayloadDigest>(&raw.payload_digest)?,
                    verified_by: KeyId(String::new()),
                    not_before: DateTime::<Utc>::UNIX_EPOCH,
                    expires_at: DateTime::<Utc>::UNIX_EPOCH,
                    first_activated_at: DateTime::<Utc>::UNIX_EPOCH,
                    confirmed_at: DateTime::<Utc>::UNIX_EPOCH,
                });
            }
        }
    }

    Ok(CacheGeneration {
        generation: generation as u64,
        members,
        written_at: parse_rfc3339("written_at", &row.written_at)?,
    })
}

async fn load_state_from_conn(conn: &mut SqliteConnection) -> Result<PolicyCacheState> {
    let slots = sqlx::query_as::<_, SlotsRow>(
        "SELECT schema_version, active_generation, pending_generation,
                last_known_good_generation, ever_configured
         FROM policy_cache_slots WHERE id = 1",
    )
    .fetch_optional(&mut *conn)
    .await?;

    let high_water_rows = sqlx::query_as::<_, HighWaterRow>(
        "SELECT issuer, policy_id, max_sequence, last_bundle_id, last_payload_digest, last_seen_at
         FROM policy_sequence_high_water",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut high_water = std::collections::BTreeMap::new();
    for row in high_water_rows {
        let (key, value) = row.into_high_water()?;
        high_water.insert(key, value);
    }

    let Some(slots) = slots else {
        return Ok(PolicyCacheState {
            schema_version: POLICY_CACHE_SCHEMA_VERSION,
            active: None,
            pending: None,
            last_known_good: None,
            high_water,
            ever_configured: false,
        });
    };

    let active = match slots.active_generation {
        Some(g) => Some(load_generation(conn, g).await?),
        None => None,
    };
    let last_known_good = match slots.last_known_good_generation {
        Some(g) => Some(load_generation(conn, g).await?),
        None => None,
    };

    Ok(PolicyCacheState {
        schema_version: slots.schema_version as u32,
        active,
        // ALWAYS None in v0.6.0 -- modelled, never populated, no API can
        // set it (see `fornax_types::policy::cache::PolicyCacheState` doc).
        pending: None,
        last_known_good,
        high_water,
        ever_configured: slots.ever_configured != 0,
    })
}

/// Attempts to re-verify every member of `generation`'s stored envelope
/// bytes. Returns `Some(usable bound revisions)` only if EVERY member is
/// usable -- a generation is never served partially loaded. See
/// [`Store::load_policy_cache`]'s doc for the reload/rewind algorithm this
/// implements.
async fn try_generation_usable(
    conn: &mut SqliteConnection,
    generation: &CacheGeneration,
    trusted: &TrustedVerificationKeys,
    now: DateTime<Utc>,
    diagnostics: &mut Vec<PolicyDiagnostic>,
) -> Result<Option<Vec<BoundRevision>>> {
    let mut usable = Vec::new();
    for member in &generation.members {
        let envelope: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT envelope FROM policy_cache_bundles WHERE bundle_id = ?1 AND payload_digest = ?2",
        )
        .bind(member.bundle_id.to_string())
        .bind(tag(&member.payload_digest)?)
        .fetch_optional(&mut *conn)
        .await?;

        let Some(envelope) = envelope else {
            diagnostics.push(unverifiable_diagnostic(
                member.bundle_id,
                "no stored envelope bytes for this member",
            ));
            return Ok(None);
        };

        match verify_bundle(&envelope, trusted, now) {
            Ok(verified) => match verified.into_bound_revisions() {
                Ok(bound) => usable.extend(bound),
                Err(_report) => {
                    diagnostics.push(unverifiable_diagnostic(
                        member.bundle_id,
                        "bindings became unusable on reload",
                    ));
                    return Ok(None);
                }
            },
            Err(BundleRejection::BundleExpired { expires_at, .. }) => {
                // Authenticated data -- came out of signed bytes, confirmed
                // at verify_bundle's step 9, after signature verification.
                // Re-verify a second time at that instant.
                let rewind_now = match DateTime::parse_from_rfc3339(&expires_at) {
                    Ok(dt) => dt.with_timezone(&Utc),
                    Err(_) => {
                        diagnostics.push(unverifiable_diagnostic(
                            member.bundle_id,
                            "expires_at on the rejection was itself unparseable",
                        ));
                        return Ok(None);
                    }
                };
                match verify_bundle(&envelope, trusted, rewind_now) {
                    Ok(verified) => match verified.into_bound_revisions() {
                        Ok(bound) => usable.extend(bound),
                        Err(_report) => {
                            diagnostics.push(unverifiable_diagnostic(
                                member.bundle_id,
                                "bindings became unusable on reload (rewound)",
                            ));
                            return Ok(None);
                        }
                    },
                    Err(e) => {
                        // Any OTHER error at the rewound instant (KeyRetired,
                        // UnknownKeyId, SignatureInvalid, ...) -> no further
                        // rewind, member is unusable. This is what makes key
                        // retirement a real revocation lever on reload.
                        diagnostics.push(unverifiable_diagnostic(member.bundle_id, e.to_string()));
                        return Ok(None);
                    }
                }
            }
            Err(e) => {
                // Any other error at real `now` (KeyRetired, UnknownKeyId,
                // SignatureInvalid, tampering, ...) -> no rewind.
                diagnostics.push(unverifiable_diagnostic(member.bundle_id, e.to_string()));
                return Ok(None);
            }
        }
    }
    Ok(Some(usable))
}

impl Store {
    /// See this module's doc comment for the crash-safety argument. Never
    /// returns a startup-fatal error for a policy reason (D2) -- only a
    /// genuine sqlx failure propagates; the daemon logs and continues even
    /// then.
    ///
    /// Reload algorithm: re-verify every member of the active generation.
    /// All usable -> active stands. Any unusable -> fall back to
    /// last-known-good as a whole, evaluated identically, one level, no
    /// recursion -- a generation is never served partially loaded. Both
    /// unusable/absent -> `usable = []`, `loaded_slot = None`.
    pub async fn load_policy_cache(
        &self,
        trusted: Option<&TrustedVerificationKeys>,
        now: DateTime<Utc>,
    ) -> Result<PolicyCacheLoad> {
        let mut conn = self.pool.acquire().await?;
        let state = load_state_from_conn(&mut conn).await?;
        let mut diagnostics = Vec::new();

        let Some(trusted) = trusted else {
            diagnostics.push(unavailable_diagnostic(
                "no trust store configured; cached bundles cannot be re-verified",
            ));
            return Ok(PolicyCacheLoad {
                state,
                usable: Vec::new(),
                loaded_slot: None,
                diagnostics,
            });
        };

        if let Some(active) = &state.active {
            if let Some(usable) =
                try_generation_usable(&mut conn, active, trusted, now, &mut diagnostics).await?
            {
                return Ok(PolicyCacheLoad {
                    state,
                    usable,
                    loaded_slot: Some(CacheSlotKind::Active),
                    diagnostics,
                });
            }
        }

        if let Some(lkg) = &state.last_known_good {
            if let Some(usable) =
                try_generation_usable(&mut conn, lkg, trusted, now, &mut diagnostics).await?
            {
                return Ok(PolicyCacheLoad {
                    state,
                    usable,
                    loaded_slot: Some(CacheSlotKind::LastKnownGood),
                    diagnostics,
                });
            }
        }

        diagnostics.push(unavailable_diagnostic(
            "neither the active nor last-known-good generation has any usable member",
        ));
        Ok(PolicyCacheLoad {
            state,
            usable: Vec::new(),
            loaded_slot: None,
            diagnostics,
        })
    }

    /// Normative order: verify (outside any transaction -- an invalid
    /// bundle never opens a transaction, so it can never touch
    /// last-known-good); `BEGIN IMMEDIATE` (serializes concurrent submits);
    /// load state inside the transaction; `evaluate_activation`; persist
    /// the decision; commit. See this module's doc comment.
    pub async fn submit_policy_bundle(
        &self,
        envelope_bytes: &[u8],
        trusted: &TrustedVerificationKeys,
        now: DateTime<Utc>,
    ) -> Result<ActivationOutcome> {
        let candidate = match verify_bundle(envelope_bytes, trusted, now) {
            Ok(c) => c,
            Err(e) => {
                let active_generation = self.current_active_generation().await?;
                return Ok(ActivationOutcome::Rejected {
                    rejection: ActivationRejection::NotVerified(e),
                    active_generation,
                });
            }
        };

        let mut conn = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;

        let state = match load_state_from_conn(&mut conn).await {
            Ok(s) => s,
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(e);
            }
        };
        let active_generation = state.active.as_ref().map(|g| g.generation);

        let decision = match fornax_types::evaluate_activation(&candidate, &state, now) {
            Ok(d) => d,
            Err(rejection) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Ok(ActivationOutcome::Rejected {
                    rejection,
                    active_generation,
                });
            }
        };

        match decision {
            ActivationDecision::Activate { members, replaced } => {
                let persisted =
                    persist_activation(&mut conn, envelope_bytes, &candidate, &members, now).await;
                if let Err(e) = persisted {
                    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                    return Err(e);
                }
                let new_gen_num = persisted.unwrap();
                match sqlx::query("COMMIT").execute(&mut *conn).await {
                    Ok(_) => Ok(ActivationOutcome::Activated {
                        generation: new_gen_num,
                        superseded: active_generation,
                        replaced_member: replaced,
                    }),
                    Err(e) => Ok(ActivationOutcome::Rejected {
                        rejection: ActivationRejection::Persistence {
                            detail: e.to_string(),
                        },
                        active_generation,
                    }),
                }
            }
            ActivationDecision::Confirm { policy_id, .. } => {
                if let Err(e) = persist_confirm(&mut conn, &candidate, policy_id, now).await {
                    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                    return Err(e);
                }
                match sqlx::query("COMMIT").execute(&mut *conn).await {
                    Ok(_) => Ok(ActivationOutcome::Confirmed {
                        generation: active_generation.unwrap_or(0),
                        policy_id,
                        confirmed_at: now,
                    }),
                    Err(e) => Ok(ActivationOutcome::Rejected {
                        rejection: ActivationRejection::Persistence {
                            detail: e.to_string(),
                        },
                        active_generation,
                    }),
                }
            }
        }
    }

    /// `active <- last_known_good`; `pending <- NULL`. High-water rows are
    /// deliberately UNTOUCHED -- otherwise a rollback could reopen a
    /// downgrade window for an attacker who can induce a post-activation
    /// failure. One transaction.
    pub async fn rollback_policy_to_last_known_good(&self, now: DateTime<Utc>) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;

        let state = match load_state_from_conn(&mut conn).await {
            Ok(s) => s,
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(e);
            }
        };

        let lkg_generation = state.last_known_good.as_ref().map(|g| g.generation as i64);
        let result = sqlx::query(
            "UPDATE policy_cache_slots SET active_generation = ?1, pending_generation = NULL, updated_at = ?2 WHERE id = 1",
        )
        .bind(lkg_generation)
        .bind(now.to_rfc3339())
        .execute(&mut *conn)
        .await;

        if let Err(e) = result {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            return Err(StoreError::Db(e));
        }

        sqlx::query("COMMIT").execute(&mut *conn).await?;
        Ok(())
    }

    /// Read-only helper: the current active generation number, if any,
    /// with no transaction (used only to annotate a `Rejected{NotVerified}`
    /// outcome -- an invalid bundle never opens a transaction).
    async fn current_active_generation(&self) -> Result<Option<u64>> {
        let mut conn = self.pool.acquire().await?;
        let state = load_state_from_conn(&mut conn).await?;
        Ok(state.active.map(|g| g.generation))
    }
}

/// Persists an `Activate` decision inside the caller's already-open
/// transaction: idempotent envelope insert, a freshly allocated generation,
/// its member rows, and the slot/high-water updates -- all in step 7's
/// single transaction. Returns the newly allocated generation number.
async fn persist_activation(
    conn: &mut SqliteConnection,
    envelope_bytes: &[u8],
    candidate: &fornax_types::VerifiedPolicyBundle,
    members: &[CachedBundleRef],
    now: DateTime<Utc>,
) -> Result<u64> {
    let new_member = members
        .iter()
        .find(|m| m.bundle_id == candidate.payload().bundle_id)
        .expect("evaluate_activation always includes the candidate's own member");

    sqlx::query(
        "INSERT OR IGNORE INTO policy_cache_bundles
            (bundle_id, payload_digest, envelope, issuer, sequence, policy_id, revision,
             revision_digest, verified_by, not_before, expires_at, first_activated_at, confirmed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )
    .bind(new_member.bundle_id.to_string())
    .bind(tag(&new_member.payload_digest)?)
    .bind(envelope_bytes)
    .bind(&new_member.issuer)
    .bind(new_member.sequence as i64)
    .bind(new_member.policy_id.0.to_string())
    .bind(new_member.revision as i64)
    .bind(tag(&new_member.revision_digest)?)
    .bind(&new_member.verified_by.0)
    .bind(new_member.not_before.to_rfc3339())
    .bind(new_member.expires_at.to_rfc3339())
    .bind(new_member.first_activated_at.to_rfc3339())
    .bind(new_member.confirmed_at.to_rfc3339())
    .execute(&mut *conn)
    .await?;

    let max_gen: Option<i64> =
        sqlx::query_scalar("SELECT MAX(generation) FROM policy_cache_generations")
            .fetch_one(&mut *conn)
            .await?;
    let new_gen_num = max_gen.unwrap_or(0) + 1;

    sqlx::query("INSERT INTO policy_cache_generations (generation, written_at) VALUES (?1, ?2)")
        .bind(new_gen_num)
        .bind(now.to_rfc3339())
        .execute(&mut *conn)
        .await?;

    for member in members {
        sqlx::query(
            "INSERT INTO policy_cache_generation_members (generation, policy_id, bundle_id, payload_digest)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(new_gen_num)
        .bind(member.policy_id.0.to_string())
        .bind(member.bundle_id.to_string())
        .bind(tag(&member.payload_digest)?)
        .execute(&mut *conn)
        .await?;
    }

    let slots = sqlx::query_as::<_, SlotsRow>(
        "SELECT schema_version, active_generation, pending_generation, last_known_good_generation, ever_configured
         FROM policy_cache_slots WHERE id = 1",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let previous_active = slots.as_ref().and_then(|s| s.active_generation);

    sqlx::query(
        "INSERT INTO policy_cache_slots
            (id, schema_version, active_generation, pending_generation, last_known_good_generation, ever_configured, updated_at)
         VALUES (1, ?1, ?2, NULL, ?3, 1, ?4)
         ON CONFLICT(id) DO UPDATE SET
            schema_version = excluded.schema_version,
            active_generation = excluded.active_generation,
            pending_generation = excluded.pending_generation,
            last_known_good_generation = excluded.last_known_good_generation,
            ever_configured = 1,
            updated_at = excluded.updated_at",
    )
    .bind(POLICY_CACHE_SCHEMA_VERSION as i64)
    .bind(new_gen_num)
    .bind(previous_active)
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        "INSERT INTO policy_sequence_high_water
            (issuer, policy_id, max_sequence, last_bundle_id, last_payload_digest, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(issuer, policy_id) DO UPDATE SET
            max_sequence = excluded.max_sequence,
            last_bundle_id = excluded.last_bundle_id,
            last_payload_digest = excluded.last_payload_digest,
            last_seen_at = excluded.last_seen_at",
    )
    .bind(&candidate.payload().provenance.issuer)
    .bind(candidate.revision().body().policy_id.0.to_string())
    .bind(candidate.payload().sequence as i64)
    .bind(candidate.payload().bundle_id.to_string())
    .bind(tag(candidate.payload_digest())?)
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;

    Ok(new_gen_num as u64)
}

/// Persists a `Confirm` decision: only `confirmed_at` on the bundle row and
/// `last_seen_at` on the high-water row change -- nothing else.
async fn persist_confirm(
    conn: &mut SqliteConnection,
    candidate: &fornax_types::VerifiedPolicyBundle,
    _policy_id: PolicyId,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "UPDATE policy_cache_bundles SET confirmed_at = ?1 WHERE bundle_id = ?2 AND payload_digest = ?3",
    )
    .bind(now.to_rfc3339())
    .bind(candidate.payload().bundle_id.to_string())
    .bind(tag(candidate.payload_digest())?)
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        "UPDATE policy_sequence_high_water SET last_seen_at = ?1 WHERE issuer = ?2 AND policy_id = ?3",
    )
    .bind(now.to_rfc3339())
    .bind(&candidate.payload().provenance.issuer)
    .bind(candidate.revision().body().policy_id.0.to_string())
    .execute(&mut *conn)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fornax_types::policy::{
        BundlePayload, BundleProvenance, BundleSignature, SignatureAlgorithm,
    };
    use fornax_types::{
        ActionClass, CacheScope, EnforcementOutcome, EnforcementRule, PolicyContent, PolicyDraft,
        PolicyId as FtPolicyId, RiskClass, RiskClassSeconds, TargetScope, TargetSelector,
        VerdictOutcomes,
    };

    fn tmp_db_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fornax-store-policy-cache-test-{name}-{}.db",
            Uuid::new_v4()
        ))
    }

    const TEST_SEED: [u8; 32] = [
        0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
        0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e,
        0x6f, 0x70,
    ];

    fn signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&TEST_SEED)
    }

    fn trust_key(
        key_id: &str,
        sk: &ed25519_dalek::SigningKey,
        not_after: Option<&str>,
    ) -> fornax_types::policy::TrustedKey {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        fornax_types::policy::TrustedKey {
            key_id: KeyId(key_id.to_string()),
            algorithm: SignatureAlgorithm::Ed25519,
            public_key_b64: STANDARD.encode(sk.verifying_key().to_bytes()),
            not_before: None,
            not_after: not_after.map(str::to_string),
            comment: None,
        }
    }

    fn trust_store(key_id: &str, sk: &ed25519_dalek::SigningKey) -> TrustedVerificationKeys {
        TrustedVerificationKeys {
            schema_version: 1,
            keys: vec![trust_key(key_id, sk, None)],
        }
    }

    fn trust_store_multi(keys: Vec<fornax_types::policy::TrustedKey>) -> TrustedVerificationKeys {
        TrustedVerificationKeys {
            schema_version: 1,
            keys,
        }
    }

    fn build_envelope(
        issuer: &str,
        sequence: u64,
        policy_id: Uuid,
        key_id: &str,
        sk: &ed25519_dalek::SigningKey,
        not_before: &str,
        expires_at: &str,
    ) -> Vec<u8> {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        use ed25519_dalek::Signer;

        let mut content = PolicyContent::default();
        content.egress.cloud_sync_allowed = Some(true);
        content.enforcement.rules = Some(vec![EnforcementRule {
            action_class: ActionClass::ShellCommand,
            risk_class: RiskClass::High,
            outcomes: VerdictOutcomes::uniform(EnforcementOutcome::Warn),
        }]);
        content.cache = CacheScope {
            max_age_seconds_by_risk: Some(RiskClassSeconds {
                low: 86_400,
                elevated: 21_600,
                high: 3_600,
                critical: 900,
            }),
            offline_grace_seconds: Some(604_800),
        };

        let draft = PolicyDraft {
            policy_id: FtPolicyId(policy_id),
            revision: 1,
            supersedes: None,
            display_name: "policy-cache-store-test".to_string(),
            content,
            pinned_fields: std::collections::BTreeSet::new(),
        };
        let revision = draft
            .publish("2026-01-01T00:00:00Z".to_string())
            .expect("draft should publish");

        let binding = fornax_types::policy::PolicyBinding {
            binding_id: Uuid::new_v4(),
            scope: TargetScope::Org {
                org_id: "org-1".to_string(),
            },
            selector: TargetSelector::default(),
            revision_ref: revision.reference(),
        };

        let payload = BundlePayload {
            bundle_schema_version: 1,
            bundle_id: Uuid::new_v4(),
            sequence,
            issued_at: "2026-01-01T00:00:00Z".to_string(),
            not_before: not_before.to_string(),
            expires_at: expires_at.to_string(),
            provenance: BundleProvenance {
                issuer: issuer.to_string(),
                audit_ref: None,
                authorized_by: None,
            },
            revision,
            bindings: vec![binding],
        };
        let payload_bytes = serde_json::to_vec(&payload).unwrap();

        let mut signed_message = Vec::new();
        signed_message.extend_from_slice(fornax_types::policy::BUNDLE_SIGNING_DOMAIN);
        signed_message.extend_from_slice(&payload_bytes);
        let signature = sk.sign(&signed_message);

        let envelope = fornax_types::policy::SignedPolicyBundle {
            bundle_schema_version: 1,
            payload_b64: STANDARD.encode(&payload_bytes),
            signatures: vec![BundleSignature {
                key_id: KeyId(key_id.to_string()),
                algorithm: SignatureAlgorithm::Ed25519,
                signature_b64: STANDARD.encode(signature.to_bytes()),
            }],
        };
        serde_json::to_vec(&envelope).unwrap()
    }

    fn now() -> DateTime<Utc> {
        "2026-01-01T00:10:00Z".parse().unwrap()
    }

    #[tokio::test]
    async fn t71_crash_mid_activation_leaves_previous_generation_intact() {
        let path = tmp_db_path("crash-mid-activation");
        let sk = signing_key();
        let trust = trust_store("k1", &sk);
        let policy_id = Uuid::new_v4();

        {
            let store = Store::open(&path).await.expect("open db");
            let env1 = build_envelope(
                "issuer-a",
                1,
                policy_id,
                "k1",
                &sk,
                "2026-01-01T00:00:00Z",
                "2027-01-01T00:00:00Z",
            );
            let outcome = store
                .submit_policy_bundle(&env1, &trust, now())
                .await
                .expect("first activation");
            assert!(matches!(
                outcome,
                ActivationOutcome::Activated { generation: 1, .. }
            ));
        }

        // Simulate a crash mid-second-activation: open a transaction, write
        // a competing generation's rows, then drop the connection WITHOUT
        // committing. SQLite rolls back an uncommitted transaction when its
        // connection closes.
        {
            let store = Store::open(&path).await.expect("reopen db");
            let mut conn = store.pool.acquire().await.expect("acquire conn");
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *conn)
                .await
                .expect("begin immediate");
            sqlx::query(
                "INSERT INTO policy_cache_generations (generation, written_at) VALUES (2, ?1)",
            )
            .bind(now().to_rfc3339())
            .execute(&mut *conn)
            .await
            .expect("insert uncommitted generation");
            sqlx::query(
                "UPDATE policy_cache_slots SET active_generation = 2, updated_at = ?1 WHERE id = 1",
            )
            .bind(now().to_rfc3339())
            .execute(&mut *conn)
            .await
            .expect("update uncommitted slots");
            // Deliberately drop without COMMIT -- simulated crash.
            drop(conn);
            drop(store);
        }

        let store = Store::open(&path)
            .await
            .expect("reopen after simulated crash");
        let load = store
            .load_policy_cache(Some(&trust), now())
            .await
            .expect("load policy cache");
        assert_eq!(
            load.state.active.as_ref().map(|g| g.generation),
            Some(1),
            "the uncommitted generation 2 must not have survived"
        );
        let hw = load
            .state
            .high_water
            .get(&("issuer-a".to_string(), FtPolicyId(policy_id)))
            .expect("high-water for generation 1 must exist");
        assert_eq!(hw.max_sequence, 1, "high-water must not have advanced");

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn t72_offline_startup_makes_no_network_calls() {
        // Structural proof: fornax-store's own dependency surface carries
        // no HTTP client, and load_policy_cache succeeds against a fresh
        // local DB with no reachable network dependency -- see this crate's
        // Cargo.toml (sqlx/tokio/serde/uuid/chrono/thiserror/tracing/
        // fornax-types only). A unit test cannot prove the absence of a
        // network call directly; this substitutes a dependency-surface
        // assertion plus a successful purely-local load.
        let cargo_toml = include_str!("../Cargo.toml");
        for forbidden in ["reqwest", "hyper", "curl", "ureq"] {
            assert!(
                !cargo_toml.contains(forbidden),
                "fornax-store must not depend on an HTTP client: found {forbidden:?}"
            );
        }

        let path = tmp_db_path("offline-startup");
        let store = Store::open(&path).await.expect("open db");
        let load = store
            .load_policy_cache(None, now())
            .await
            .expect("load policy cache with no trust store");
        assert!(load.usable.is_empty());
        assert_eq!(load.loaded_slot, None);
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn t73_reload_after_expiry_rewinds_and_preserves_content() {
        let path = tmp_db_path("reload-after-expiry");
        let sk = signing_key();
        let trust = trust_store("k1", &sk);
        let policy_id = Uuid::new_v4();

        let store = Store::open(&path).await.expect("open db");
        let env = build_envelope(
            "issuer-a",
            1,
            policy_id,
            "k1",
            &sk,
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
        );
        store
            .submit_policy_bundle(&env, &trust, now())
            .await
            .expect("activate");

        // Reload well past expires_at.
        let far_future: DateTime<Utc> = "2027-01-01T00:00:00Z".parse().unwrap();
        let load = store
            .load_policy_cache(Some(&trust), far_future)
            .await
            .expect("load policy cache after expiry");
        assert_eq!(load.loaded_slot, Some(CacheSlotKind::Active));
        assert_eq!(
            load.usable.len(),
            1,
            "content must be preserved, not discarded"
        );

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn t74_key_retirement_at_reload_falls_back_to_last_known_good() {
        let path = tmp_db_path("key-retirement");
        let sk1 = signing_key();
        let sk2 = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let policy_id_1 = Uuid::new_v4();
        let policy_id_2 = Uuid::new_v4();

        let store = Store::open(&path).await.expect("open db");
        let trust1 = trust_store("k1", &sk1);
        let env1 = build_envelope(
            "issuer-a",
            1,
            policy_id_1,
            "k1",
            &sk1,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        let outcome1 = store
            .submit_policy_bundle(&env1, &trust1, now())
            .await
            .expect("first activation");
        assert!(matches!(outcome1, ActivationOutcome::Activated { .. }));

        // Second activation with key `k2`, signed after k1's bundle --
        // this becomes generation 2 (active), generation 1 becomes LKG.
        let trust_both = trust_store("k2", &sk2);
        let env2 = build_envelope(
            "issuer-a",
            1,
            policy_id_2,
            "k2",
            &sk2,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        let outcome2 = store
            .submit_policy_bundle(&env2, &trust_both, now())
            .await
            .expect("second activation");
        assert!(matches!(
            outcome2,
            ActivationOutcome::Activated { generation: 2, .. }
        ));

        // At reload time, k2 is still *present* in the trust store but its
        // `not_after` has lapsed -- this is retirement (`BundleRejection::
        // KeyRetired`), a distinct code path from `k2` being altogether
        // absent (`UnknownKeyId`). `k1` remains valid (no `not_after`), so
        // the reload must fall back to LKG (generation 1, signed by k1) and
        // that fallback must succeed against this same trust store.
        let trust_k1_valid_k2_retired = trust_store_multi(vec![
            trust_key("k1", &sk1, None),
            trust_key("k2", &sk2, Some("2025-12-01T00:00:00Z")),
        ]);
        let load = store
            .load_policy_cache(Some(&trust_k1_valid_k2_retired), now())
            .await
            .expect("load policy cache");
        assert_eq!(
            load.loaded_slot,
            Some(CacheSlotKind::LastKnownGood),
            "generation 2 (signed by now-retired k2) must be unusable, falling back to LKG"
        );
        assert_eq!(load.usable.len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn t75_partial_generation_refusal_falls_back_to_last_known_good() {
        let path = tmp_db_path("partial-generation");
        let sk1 = signing_key();
        let sk2 = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let p1 = Uuid::new_v4();
        let p2 = Uuid::new_v4();

        let store = Store::open(&path).await.expect("open db");

        let trust1 = trust_store("k1", &sk1);
        let env1 = build_envelope(
            "issuer-a",
            1,
            p1,
            "k1",
            &sk1,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        store
            .submit_policy_bundle(&env1, &trust1, now())
            .await
            .expect("activate p1");

        let trust2 = trust_store("k2", &sk2);
        let env2 = build_envelope(
            "issuer-a",
            1,
            p2,
            "k2",
            &sk2,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        store
            .submit_policy_bundle(&env2, &trust2, now())
            .await
            .expect(
                "activate p2 (generation 2, contains only p2 -- p1's lineage \
                     degrades on its own clock per this ticket's deviation #5, \
                     it is not re-included)",
            );

        // Directly test the "one member unusable -> whole generation
        // rejected" rule using a hand-built two-member generation.
        let mut conn = store.pool.acquire().await.expect("acquire");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO policy_cache_generations (generation, written_at) VALUES (99, ?1)",
        )
        .bind(now().to_rfc3339())
        .execute(&mut *conn)
        .await
        .unwrap();
        // Re-use p2's already-stored bundle row as one member, plus a
        // second member row pointing at a bundle_id/payload_digest that was
        // never persisted (guaranteed unusable -- no envelope to verify).
        sqlx::query(
            "INSERT INTO policy_cache_generation_members (generation, policy_id, bundle_id, payload_digest)
             SELECT 99, policy_id, bundle_id, payload_digest FROM policy_cache_generation_members WHERE generation = 2",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO policy_cache_generation_members (generation, policy_id, bundle_id, payload_digest)
             VALUES (99, ?1, ?2, ?3)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(Uuid::new_v4().to_string())
        .bind("sha256:missing")
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE policy_cache_slots SET active_generation = 99, updated_at = ?1 WHERE id = 1",
        )
        .bind(now().to_rfc3339())
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query("COMMIT").execute(&mut *conn).await.unwrap();
        drop(conn);

        // Trust both k1 and k2 at reload so the fallback to LKG (generation
        // 1, signed by k1) is not itself blocked by a missing key -- this
        // isolates the assertion to "one unusable member rejects the whole
        // generation", not "the reload trust store happens to be too narrow".
        let trust_both = trust_store_multi(vec![
            trust_key("k1", &sk1, None),
            trust_key("k2", &sk2, None),
        ]);
        let load = store
            .load_policy_cache(Some(&trust_both), now())
            .await
            .expect("load policy cache");
        assert_eq!(
            load.loaded_slot,
            Some(CacheSlotKind::LastKnownGood),
            "a generation with one unusable member must never be served -- \
             must fall back to LKG (generation 1), not merely avoid Active"
        );
        assert_eq!(load.usable.len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn t76_total_loss_reports_unavailable_but_keeps_ever_configured() {
        let path = tmp_db_path("total-loss");
        let sk = signing_key();
        let policy_id = Uuid::new_v4();

        let store = Store::open(&path).await.expect("open db");
        let trust = trust_store("k1", &sk);
        let env = build_envelope(
            "issuer-a",
            1,
            policy_id,
            "k1",
            &sk,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        store
            .submit_policy_bundle(&env, &trust, now())
            .await
            .expect("activate");

        // No trust store at all at reload -> both active and (absent) LKG
        // are unusable.
        let load = store
            .load_policy_cache(None, now())
            .await
            .expect("load policy cache");
        assert!(load.usable.is_empty());
        assert_eq!(load.loaded_slot, None);
        assert!(
            load.state.ever_configured,
            "ever_configured must stay sticky"
        );
        assert!(load
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::PolicyCacheUnavailable));

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn t77_rollback_restores_previous_generation_high_water_untouched() {
        let path = tmp_db_path("rollback");
        let sk = signing_key();
        let policy_id = Uuid::new_v4();
        let store = Store::open(&path).await.expect("open db");
        let trust = trust_store("k1", &sk);

        let env5 = build_envelope(
            "issuer-a",
            5,
            policy_id,
            "k1",
            &sk,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        store
            .submit_policy_bundle(&env5, &trust, now())
            .await
            .expect("activate seq5");

        let env7 = build_envelope(
            "issuer-a",
            7,
            policy_id,
            "k1",
            &sk,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        store
            .submit_policy_bundle(&env7, &trust, now())
            .await
            .expect("activate seq7");

        store
            .rollback_policy_to_last_known_good(now())
            .await
            .expect("rollback");

        let load = store
            .load_policy_cache(Some(&trust), now())
            .await
            .expect("load after rollback");
        let active = load
            .state
            .active
            .expect("active must be set after rollback");
        assert_eq!(
            active.members[0].sequence, 5,
            "must roll back to the seq5 generation"
        );

        let hw = load
            .state
            .high_water
            .get(&("issuer-a".to_string(), FtPolicyId(policy_id)))
            .expect("high-water must still exist");
        assert_eq!(
            hw.max_sequence, 7,
            "high-water must be untouched by rollback"
        );

        // Resubmitting seq6 must still be rejected (rollback did not lower
        // the high-water mark).
        let env6 = build_envelope(
            "issuer-a",
            6,
            policy_id,
            "k1",
            &sk,
            "2026-01-01T00:00:00Z",
            "2027-01-01T00:00:00Z",
        );
        let outcome = store
            .submit_policy_bundle(&env6, &trust, now())
            .await
            .expect("submit seq6");
        assert!(matches!(
            outcome,
            ActivationOutcome::Rejected {
                rejection: ActivationRejection::SequenceNotAdvanced { .. },
                ..
            }
        ));

        std::fs::remove_file(&path).ok();
    }
}
