//! Canonical Fornax domain types (FORNX-24).
//!
//! Provider-native Claude Code / Codex events are thin adapter inputs; core
//! code (verifiers, storage, status/detail/dashboard) consumes only these
//! normalized concepts. Evidence must preserve provenance. Missing signals
//! are represented explicitly as `Unavailable`, never inferred or dropped.
//!
//! Grounded in observed payload shapes (see docs/research/adapter-capability-matrix.md):
//! Claude Code hook stdin JSON carries `session_id`, `transcript_path`, `cwd`,
//! `hook_event_name`, plus event-specific `tool_name`/`tool_input`/`tool_response`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Which coding-agent runtime an event/capability originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    ClaudeCode,
    Codex,
}

/// Normalized lifecycle event kind. One variant per canonical concept a
/// provider *might* expose; a provider that doesn't expose a given kind
/// simply never emits it (see `RuntimeCapabilities`), it is not synthesized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    SubagentStart,
    SubagentStop,
    Notification,
}

/// An immutable, provider-normalized observation. Persisted verbatim
/// (including `raw`) before any claim/verification logic runs, so sessions
/// can be replayed against future verifiers/calibration (FORNX-49).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub id: Uuid,
    pub session_id: String,
    pub provider: Provider,
    pub kind: EventKind,
    /// RFC3339 timestamp this event was observed locally (not provider time).
    pub observed_at: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    /// Provider's own serialized result for the tool call, when the provider
    /// exposes one (e.g. Claude Code's PostToolUse `tool_response`). This is
    /// the provider's summarized view, not a guaranteed raw capture.
    pub tool_response: Option<serde_json::Value>,
    /// Full untouched provider payload for this event, for replay/debugging.
    pub raw: serde_json::Value,
}

/// A natural-language or structured assertion extracted from agent output
/// (e.g. "All tests passed"). Claims are hypotheses to check, not facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: Uuid,
    pub session_id: String,
    pub source_event_id: Uuid,
    pub text: String,
    /// Coarse claim category a verifier can match on, e.g. "test_result",
    /// "file_written", "command_succeeded". Open-ended by design (string, not
    /// enum) until enough verifier families exist to justify closing it.
    pub subject: String,
    pub claimed_at: String,
}

/// Evidence kind — what was directly observed, independent of any claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    ToolResult,
    ExitCode,
    FileDiff,
    ProcessObservation,
    TranscriptExcerpt,
}

/// A single piece of observed, provenance-carrying evidence. Evidence exists
/// independent of any claim; a verifier links relevant evidence to a claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: Uuid,
    pub session_id: String,
    pub source_event_id: Uuid,
    pub kind: EvidenceKind,
    pub observed_at: String,
    pub payload: serde_json::Value,
    /// Human-readable provenance, e.g. "claude_code:PostToolUse:Bash#tool_response".
    pub provenance: String,
}

/// The five-state verdict vocabulary (HVDL-15 / FORNX-20). Never collapsed
/// to a boolean or a score in v0.0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Verified,
    Unverified,
    Contradicted,
    Review,
    Unavailable,
}

/// Output of `Claim + Evidence[] + RuntimeCapabilities -> Finding`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: Uuid,
    pub claim_id: Uuid,
    pub verdict: Verdict,
    pub evidence_ids: Vec<Uuid>,
    pub verifier_name: String,
    pub rationale: String,
    pub computed_at: String,
}

/// One line of the newline-delimited JSON protocol adapters speak to the
/// daemon over the Unix Domain Socket (FORNX-25). Adapters stay thin: they
/// translate provider payloads into these messages and nothing else — claim
/// extraction/verification happens daemon-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IngestMessage {
    Event(AgentEvent),
    Claim(Claim),
    Evidence(Evidence),
    /// Adapter announces what its runtime can observe, once per connection.
    Capabilities(RuntimeCapabilities),
}

/// What a given provider integration can actually observe. Verifiers consult
/// this before deciding `Unavailable` vs. attempting verification — a missing
/// capability must never be silently treated as a pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub provider: Provider,
    pub supports_pre_tool_use: bool,
    pub supports_post_tool_use: bool,
    pub supports_tool_response_capture: bool,
    pub supports_session_stop_event: bool,
    pub supports_transcript_tail: bool,
    pub supports_subagent_lifecycle: bool,
    /// Free-form notes on partial/uncertain support, for the capability
    /// matrix doc — not machine-consumed, kept for provenance in Findings.
    pub notes: HashMap<String, String>,
}
