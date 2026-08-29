-- Persisted RuntimeCapabilities announcements (FORNX-62).
--
-- Cardinality decision: keyed by (session_id, provider), unlike
-- agent_events/claims/evidence which are insert-only/append-only (see
-- 0001_init.sql). Capabilities describe what a provider adapter's *current*
-- connection can observe, not an immutable historical observation -- there
-- is exactly one adapter connection per session in v0.0.1, and a
-- re-announcement (e.g. an adapter reconnect within the same session) should
-- overwrite the previous row, not accumulate a history nobody reads. This
-- mirrors how fornax-daemon's in-memory HashMap<session_id, RuntimeCapabilities>
-- already behaves (last write wins) and the (device_id, provider) upsert
-- fornax-cloud's ingest already uses for the same concept one hop further
-- downstream.
CREATE TABLE IF NOT EXISTS runtime_capabilities (
    session_id                     TEXT NOT NULL,
    provider                       TEXT NOT NULL,
    supports_pre_tool_use          INTEGER NOT NULL,
    supports_post_tool_use         INTEGER NOT NULL,
    supports_tool_response_capture INTEGER NOT NULL,
    supports_session_stop_event    INTEGER NOT NULL,
    supports_transcript_tail       INTEGER NOT NULL,
    supports_subagent_lifecycle    INTEGER NOT NULL,
    notes                          TEXT NOT NULL, -- JSON object
    observed_at                    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (session_id, provider)
);
