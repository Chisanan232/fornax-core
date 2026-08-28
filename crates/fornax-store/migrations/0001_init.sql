-- Fornax local evidence store schema (FORNX-26).
-- Immutable-by-convention: rows are inserted, never updated, except findings
-- which may be superseded by a new row (old rows kept) if a verifier reruns.

CREATE TABLE IF NOT EXISTS agent_events (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    provider        TEXT NOT NULL,
    kind            TEXT NOT NULL,
    observed_at     TEXT NOT NULL,
    tool_name       TEXT,
    tool_input      TEXT,
    tool_response   TEXT,
    raw             TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_events_session ON agent_events(session_id);
CREATE INDEX IF NOT EXISTS idx_agent_events_observed_at ON agent_events(observed_at);

CREATE TABLE IF NOT EXISTS claims (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    source_event_id TEXT NOT NULL REFERENCES agent_events(id),
    text            TEXT NOT NULL,
    subject         TEXT NOT NULL,
    claimed_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_claims_session ON claims(session_id);

CREATE TABLE IF NOT EXISTS evidence (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    source_event_id TEXT NOT NULL REFERENCES agent_events(id),
    kind            TEXT NOT NULL,
    observed_at     TEXT NOT NULL,
    payload         TEXT NOT NULL,
    provenance      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_evidence_session ON evidence(session_id);

CREATE TABLE IF NOT EXISTS findings (
    id              TEXT PRIMARY KEY,
    claim_id        TEXT NOT NULL REFERENCES claims(id),
    verdict         TEXT NOT NULL,
    evidence_ids    TEXT NOT NULL, -- JSON array of evidence ids
    verifier_name   TEXT NOT NULL,
    rationale       TEXT NOT NULL,
    computed_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_findings_claim ON findings(claim_id);
