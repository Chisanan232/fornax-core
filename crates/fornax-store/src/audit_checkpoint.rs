//! Local persistence for verified signed audit checkpoints (FORNX-317,
//! ADR-0012), and the device-side divergence-detection comparison
//! (ADR-0012 §8.2) that anchors [`crate::audit_ledger::ChainVerification`]
//! against each stored checkpoint receipt's attested head.
//!
//! **A stored receipt is only ever written after
//! `fornax_types::verify_audit_checkpoint` returns `Ok`** -- never from an
//! unverified response (ADR-0012 §8.1). This module never verifies a
//! signature itself; that is entirely `fornax-types`' job. It also never
//! attests completeness or endpoint honesty -- see this crate's
//! `audit_ledger` module doc for the identical local-ledger trust
//! boundary this checkpoint mechanism only partially strengthens (a
//! second, external witness to the head, per ADR-0012 §1).

use fornax_types::VerifiedAuditCheckpoint;

use crate::audit_ledger::{ChainVerification, DivergenceKind};
use crate::{Result, Store, StoreError};

/// One persisted, previously-verified checkpoint receipt
/// (`migrations/0012_audit_checkpoints.sql`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditCheckpointReceipt {
    pub checkpoint_seq: u64,
    pub head_ledger_seq: i64,
    pub head_entry_hash: String,
    pub issued_at: String,
    /// The verified payload's own `device_id` (ADR-0012 §3.2) -- persisted
    /// so a later receipt's `device_id` can be cross-checked against the
    /// first-ever stored receipt's, per
    /// [`Store::store_audit_checkpoint_receipt`]'s doc comment.
    pub device_id: String,
    /// The raw, verified `SignedAuditCheckpoint` envelope JSON, kept
    /// verbatim for read-back (ADR-0012 §7.4 parity) rather than
    /// re-serialized from typed fields.
    pub envelope: String,
}

#[derive(sqlx::FromRow)]
struct AuditCheckpointRow {
    checkpoint_seq: i64,
    head_ledger_seq: i64,
    head_entry_hash: String,
    issued_at: String,
    device_id: String,
    envelope: String,
}

impl From<AuditCheckpointRow> for AuditCheckpointReceipt {
    fn from(row: AuditCheckpointRow) -> Self {
        AuditCheckpointReceipt {
            checkpoint_seq: row.checkpoint_seq as u64,
            head_ledger_seq: row.head_ledger_seq,
            head_entry_hash: row.head_entry_hash,
            issued_at: row.issued_at,
            device_id: row.device_id,
            envelope: row.envelope,
        }
    }
}

/// Result of comparing one stored [`AuditCheckpointReceipt`] against the
/// current [`ChainVerification`] result and (when the chain is `Valid`)
/// the row currently occupying `audit_events.seq == head_ledger_seq`, if
/// any. See [`evaluate_checkpoint_consistency`]'s doc comment for the
/// exact, normative evaluation order (ADR-0012 §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointConsistencyVerdict {
    /// The chain is internally valid, and the row at the attested
    /// `head_ledger_seq` has exactly the attested `head_entry_hash`. The
    /// only consistent verdict.
    Consistent,
    /// The chain is valid, but no row occupies `head_ledger_seq` any
    /// more -- the ledger was truncated past a point the cloud witnessed.
    AnchorMissing,
    /// The chain is valid, a row occupies `head_ledger_seq`, but its
    /// `entry_hash` no longer matches what was attested -- history was
    /// rewritten at exactly the anchored position.
    AnchorRewritten { attested: String, found: String },
    /// The chain diverged strictly AFTER the attested head -- the
    /// attested prefix is still intact; the damage is later.
    DivergedAfterAnchor {
        first_bad_ledger_seq: i64,
        kind: DivergenceKind,
    },
    /// The chain diverged AT OR BEFORE the attested head (inclusive) --
    /// the corruption lies inside the range the cloud already witnessed.
    /// The strongest finding.
    AttestedPrefixCorrupted {
        first_bad_ledger_seq: i64,
        kind: DivergenceKind,
    },
}

/// Pure evaluation of ADR-0012 §8.2's five-row table, most-severe-first:
///
/// 1. `Diverged` with `first_bad_seq <= head_ledger_seq` ->
///    [`CheckpointConsistencyVerdict::AttestedPrefixCorrupted`] (row 1's
///    `<=` is deliberate and inclusive: a corruption AT the attested head
///    itself is inside the attested range).
/// 2. `Diverged` with `first_bad_seq > head_ledger_seq` ->
///    [`CheckpointConsistencyVerdict::DivergedAfterAnchor`].
/// 3. `Valid` and no row at `head_ledger_seq` ->
///    [`CheckpointConsistencyVerdict::AnchorMissing`].
/// 4. `Valid`, row present, hash differs ->
///    [`CheckpointConsistencyVerdict::AnchorRewritten`].
/// 5. `Valid`, row present, hash matches (exact string equality, including
///    the `sha256:` prefix) -> [`CheckpointConsistencyVerdict::Consistent`].
///
/// Rows 1-2 use only `chain`, never `row_entry_hash_at_head` -- a
/// `Diverged` verdict is decided entirely by where the divergence sits
/// relative to the attested head, independent of what (if anything)
/// currently occupies that row.
pub fn evaluate_checkpoint_consistency(
    chain: &ChainVerification,
    head_ledger_seq: i64,
    head_entry_hash: &str,
    row_entry_hash_at_head: Option<&str>,
) -> CheckpointConsistencyVerdict {
    match chain {
        ChainVerification::Diverged {
            first_bad_seq,
            kind,
        } => {
            if *first_bad_seq <= head_ledger_seq {
                CheckpointConsistencyVerdict::AttestedPrefixCorrupted {
                    first_bad_ledger_seq: *first_bad_seq,
                    kind: *kind,
                }
            } else {
                CheckpointConsistencyVerdict::DivergedAfterAnchor {
                    first_bad_ledger_seq: *first_bad_seq,
                    kind: *kind,
                }
            }
        }
        ChainVerification::Valid => match row_entry_hash_at_head {
            None => CheckpointConsistencyVerdict::AnchorMissing,
            Some(found) if found != head_entry_hash => {
                CheckpointConsistencyVerdict::AnchorRewritten {
                    attested: head_entry_hash.to_string(),
                    found: found.to_string(),
                }
            }
            Some(_) => CheckpointConsistencyVerdict::Consistent,
        },
    }
}

const RECEIPT_COLUMNS: &str =
    "checkpoint_seq, head_ledger_seq, head_entry_hash, issued_at, device_id, envelope";

impl Store {
    /// Persists one previously-[`fornax_types::verify_audit_checkpoint`]-verified
    /// checkpoint receipt. Callers must never call this with an unverified
    /// envelope -- see this module's doc comment.
    ///
    /// **Does not itself cross-check `device_id`** against any prior
    /// receipt -- callers (see `fornax-daemon`'s `audit_checkpoint_submit`)
    /// must compare `verified.device_id()` against
    /// [`Self::first_audit_checkpoint_receipt`]'s `device_id` (the
    /// bootstrap anchor) BEFORE calling this, and refuse to store a
    /// mismatched response (ADR-0012 §3.2: "A device must check this
    /// equals its own"). The very first receipt this device ever stores
    /// has no prior anchor to check against -- that one gap is
    /// unavoidable and is documented in the implementation report, not
    /// silently assumed away.
    pub async fn store_audit_checkpoint_receipt(
        &self,
        verified: &VerifiedAuditCheckpoint,
        envelope_json: &str,
    ) -> Result<()> {
        let head = verified.head();
        sqlx::query(
            "INSERT INTO audit_checkpoints
                (checkpoint_seq, head_ledger_seq, head_entry_hash, issued_at, device_id, envelope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(checkpoint_seq) DO UPDATE SET
                head_ledger_seq = excluded.head_ledger_seq,
                head_entry_hash = excluded.head_entry_hash,
                issued_at = excluded.issued_at,
                device_id = excluded.device_id,
                envelope = excluded.envelope",
        )
        .bind(verified.checkpoint_seq() as i64)
        .bind(head.ledger_seq)
        .bind(&head.entry_hash)
        .bind(verified.issued_at())
        .bind(verified.device_id())
        .bind(envelope_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every stored checkpoint receipt, oldest (`checkpoint_seq`) first.
    pub async fn audit_checkpoint_receipts(&self) -> Result<Vec<AuditCheckpointReceipt>> {
        let rows = sqlx::query_as::<_, AuditCheckpointRow>(&format!(
            "SELECT {RECEIPT_COLUMNS} FROM audit_checkpoints ORDER BY checkpoint_seq ASC"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(AuditCheckpointReceipt::from).collect())
    }

    /// The lowest-`checkpoint_seq` stored receipt, if any -- the bootstrap
    /// anchor a later receipt's `device_id` is cross-checked against (see
    /// [`Self::store_audit_checkpoint_receipt`]'s doc comment).
    pub async fn first_audit_checkpoint_receipt(&self) -> Result<Option<AuditCheckpointReceipt>> {
        let row = sqlx::query_as::<_, AuditCheckpointRow>(&format!(
            "SELECT {RECEIPT_COLUMNS} FROM audit_checkpoints ORDER BY checkpoint_seq ASC LIMIT 1"
        ))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(AuditCheckpointReceipt::from))
    }

    /// The highest-`checkpoint_seq` stored receipt, if any.
    pub async fn latest_audit_checkpoint_receipt(&self) -> Result<Option<AuditCheckpointReceipt>> {
        let row = sqlx::query_as::<_, AuditCheckpointRow>(&format!(
            "SELECT {RECEIPT_COLUMNS} FROM audit_checkpoints ORDER BY checkpoint_seq DESC LIMIT 1"
        ))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(AuditCheckpointReceipt::from))
    }

    /// Runs ADR-0012 §8.2's comparison for every stored checkpoint receipt
    /// against the CURRENT `verify_audit_chain()` result, run once and
    /// shared across all receipts. Called on daemon start and before each
    /// new checkpoint submission -- see [`evaluate_checkpoint_consistency`]'s
    /// doc comment for the per-receipt evaluation order.
    pub async fn evaluate_all_checkpoint_receipts(
        &self,
    ) -> Result<Vec<(AuditCheckpointReceipt, CheckpointConsistencyVerdict)>> {
        let chain = self.verify_audit_chain().await?;
        let receipts = self.audit_checkpoint_receipts().await?;

        let mut results = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            let row_entry_hash_at_head: Option<String> =
                sqlx::query_scalar("SELECT entry_hash FROM audit_events WHERE seq = ?1")
                    .bind(receipt.head_ledger_seq)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(StoreError::Db)?;

            let verdict = evaluate_checkpoint_consistency(
                &chain,
                receipt.head_ledger_seq,
                &receipt.head_entry_hash,
                row_entry_hash_at_head.as_deref(),
            );
            results.push((receipt, verdict));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ChainVerification {
        ChainVerification::Valid
    }

    fn diverged(first_bad_seq: i64, kind: DivergenceKind) -> ChainVerification {
        ChainVerification::Diverged {
            first_bad_seq,
            kind,
        }
    }

    const ATTESTED: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const FOUND: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// §8.2 row 5: `Valid` + row present + hash matches -> `Consistent`.
    #[test]
    fn row5_valid_chain_with_matching_row_is_consistent() {
        let verdict = evaluate_checkpoint_consistency(&valid(), 5, ATTESTED, Some(ATTESTED));
        assert_eq!(verdict, CheckpointConsistencyVerdict::Consistent);
    }

    /// §8.2 row 4: `Valid` + row present + hash differs -> `AnchorRewritten`.
    #[test]
    fn row4_valid_chain_with_mismatched_row_is_anchor_rewritten() {
        let verdict = evaluate_checkpoint_consistency(&valid(), 5, ATTESTED, Some(FOUND));
        assert_eq!(
            verdict,
            CheckpointConsistencyVerdict::AnchorRewritten {
                attested: ATTESTED.to_string(),
                found: FOUND.to_string(),
            }
        );
    }

    /// §8.2 row 3: `Valid` + no row at head -> `AnchorMissing`.
    #[test]
    fn row3_valid_chain_with_no_row_at_head_is_anchor_missing() {
        let verdict = evaluate_checkpoint_consistency(&valid(), 5, ATTESTED, None);
        assert_eq!(verdict, CheckpointConsistencyVerdict::AnchorMissing);
    }

    /// §8.2 row 2: `Diverged` strictly after the attested head ->
    /// `DivergedAfterAnchor`.
    #[test]
    fn row2_divergence_after_attested_head_is_diverged_after_anchor() {
        let chain = diverged(9, DivergenceKind::HashMismatch);
        let verdict = evaluate_checkpoint_consistency(&chain, 5, ATTESTED, Some(ATTESTED));
        assert_eq!(
            verdict,
            CheckpointConsistencyVerdict::DivergedAfterAnchor {
                first_bad_ledger_seq: 9,
                kind: DivergenceKind::HashMismatch,
            }
        );
    }

    /// §8.2 row 1: `Diverged` at-or-before the attested head (inclusive)
    /// -> `AttestedPrefixCorrupted`. Exercised at exactly `first_bad_seq ==
    /// head_ledger_seq` to prove the `<=` boundary is inclusive, not `<`.
    #[test]
    fn row1_divergence_at_or_before_attested_head_is_attested_prefix_corrupted() {
        let chain = diverged(5, DivergenceKind::RelinkedPrevHash);
        let verdict = evaluate_checkpoint_consistency(&chain, 5, ATTESTED, Some(ATTESTED));
        assert_eq!(
            verdict,
            CheckpointConsistencyVerdict::AttestedPrefixCorrupted {
                first_bad_ledger_seq: 5,
                kind: DivergenceKind::RelinkedPrevHash,
            }
        );

        // Strictly before is also inclusive.
        let chain_before = diverged(3, DivergenceKind::MissingSeq);
        let verdict_before =
            evaluate_checkpoint_consistency(&chain_before, 5, ATTESTED, Some(ATTESTED));
        assert_eq!(
            verdict_before,
            CheckpointConsistencyVerdict::AttestedPrefixCorrupted {
                first_bad_ledger_seq: 3,
                kind: DivergenceKind::MissingSeq,
            }
        );
    }

    /// Rows 1-2 never consult `row_entry_hash_at_head` -- a `Diverged`
    /// verdict is unaffected even when a (fabricated) matching row is
    /// supplied.
    #[test]
    fn diverged_verdicts_ignore_row_entry_hash_at_head() {
        let chain = diverged(9, DivergenceKind::TruncatedTail);
        let with_none = evaluate_checkpoint_consistency(&chain, 5, ATTESTED, None);
        let with_some = evaluate_checkpoint_consistency(&chain, 5, ATTESTED, Some(FOUND));
        assert_eq!(with_none, with_some);
    }
}
