-- FORNX-315: local append-only, hash-chained audit ledger persistence.
-- Additive only -- touches no existing table.
--
-- `audit_events` is the append-only chain itself: one row per appended
-- `AuditEvent` (FORNX-314, `fornax_types::audit::AuditEvent`), post-
-- redaction. `seq` is `INTEGER PRIMARY KEY`, which SQLite aliases to the
-- rowid -- a monotonically increasing integer assigned by
-- `Store::append_audit_event`, never by SQLite's own autoincrement
-- default (the value is computed explicitly from the previous row so the
-- hash chain and the sequence number can never drift apart). `recorded_at`
-- is when this store appended the row -- distinct from
-- `AuditEvent.occurred_at`, which is baked into `payload` and is whatever
-- the original event producer claimed.
--
-- `audit_ledger_high_water` is a single-row marker of the highest `seq`
-- ever assigned by this store, updated in the same transaction as every
-- append. Without it, deleting only the *trailing* rows of `audit_events`
-- leaves no internal gap and no broken `prev_hash` link for
-- `Store::verify_audit_chain` to notice -- the remaining rows still form a
-- perfectly coherent, shorter chain. This table exists purely so a
-- truncated tail is still detectable: see `verify_audit_chain`'s
-- `DivergenceKind::TruncatedTail` path.
--
-- No update or delete statement anywhere in this crate touches
-- `audit_events` -- `Store::append_audit_event` is the only write path (see
-- its own doc comment and the crate's
-- `no_write_path_other_than_append_audit_event_touches_audit_events` test).

CREATE TABLE IF NOT EXISTS audit_events (
    seq INTEGER PRIMARY KEY,
    event_id TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    prev_hash TEXT NOT NULL,
    entry_hash TEXT NOT NULL,
    export_class TEXT NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_ledger_high_water (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    max_seq INTEGER NOT NULL
);
