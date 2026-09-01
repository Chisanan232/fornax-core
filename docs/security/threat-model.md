# Fornax threat model

Jira: FORNX-233. Durable and repo-level, not regenerated per candidate —
see [`docs/release-security-gate.md`](../release-security-gate.md) for how
this document is used inside the release security gate. This document
enumerates every trust boundary in the shared surface vocabulary
(`docs/release-candidate-evidence.md`, FORNX-231), its current controls,
and known residual risk, as of `threat_model_version` below.

`threat_model_version: 1`

## How this document is maintained

- Any release candidate classified `TRUST_BOUNDARY` or higher
  (`docs/release-assurance-policy.md`, FORNX-229) must append a "Changelog"
  entry (below) recording which boundary changed and how this document's
  description of that boundary was updated — or an explicit statement that
  the boundary's description was reviewed and confirmed unchanged.
- A `MAJOR_OR_GA` candidate requires every boundary below to be re-reviewed
  in the same pass, not only the touched ones, and the changelog entry must
  say so explicitly (`full_review: true`).
- Bumping `threat_model_version` is required whenever a boundary's
  *description* changes (new control, changed data flow, newly identified
  residual risk) — not required for a changelog entry that only confirms
  "reviewed, unchanged."

## Trust boundaries

Ten boundaries, matching the ten `trust boundary: yes` rows of
`docs/release-candidate-evidence.md`'s Shared surface vocabulary table.

### `daemon_socket` — local daemon/socket surface

**Description:** `fornax-daemon` listens on a local Unix domain socket;
adapters/hooks connect to it to report events and read verdicts.
**Current controls:** socket file permissions restricted to the invoking
user (see `docs/release/v0.0.1-qa-security-signoff.md` §"FORNX-57" for the
WAL/SHM sidecar hardening precedent); no network-facing listener exists —
the daemon does not bind a TCP/UDP port. All message processing is
serialized behind a single `AppState.processing` lock (FORNX-281 fix,
`docs/release/v0.0.1-qa-security-signoff.md` §7.6), closing a race where an
out-of-order write/read across independently-spawned per-connection tasks
could compute a wrong verdict.
**Known residual risk:** none currently open above Low. A future
multi-daemon or remote-daemon design would need this section revisited
before shipping.

### `adapter_provider_input` — adapter/provider input handling

**Description:** `fornax-adapter-claude`, `fornax-adapter-codex`,
`fornax-adapter-opencode` parse hook/event payloads from their respective
CLI tools (Claude Code, Codex, opencode) and forward normalized events to
the daemon.
**Current controls:** adversarial daemon/adapter input corpus (malformed
JSON, missing/wrong-type/unknown fields, deep nesting, oversized strings,
control characters, path-traversal-looking strings, shell metacharacters,
replayed event IDs) exercised against a real daemon + `fornax-hook-claude`
with zero crashes/panics/subprocess-spawns/filesystem-escapes
(`crates/fornax-daemon/tests/adversarial_daemon_input.rs`, fornax-core#30).
No `std::process::Command`/shell-invocation surface exists anywhere in
production source for untrusted input to reach (verified by exhaustive
grep, `docs/release/v0.0.1-qa-security-signoff.md` §3.2).
**Known residual risk:** Informational — `fornax-adapter-claude`'s
`last_assistant_text` reads a `Stop` hook's `transcript_path` via
`std::fs::read_to_string` with no confinement to `$FORNAX_HOME`; a
`../../`-style path could be read if it happens to parse as the expected
transcript JSONL. No privilege boundary is crossed (the hook runs as the
invoking user). Tracked as a future confinement check, not currently
blocking.

### `evidence_provenance` — evidence/provenance integrity

**Description:** the daemon's SQLite store persists observed events and
computed claims/verdicts; this data is the evidentiary record the product's
five-state verdict vocabulary (ADR-0001 D4) is built on.
**Current controls:** WAL/SHM sidecar file permissions hardening
(FORNX-57); serialized message processing (FORNX-281) prevents an
evidence write racing a read of the same session's prior state.
**Known residual risk:** none currently open above Low.

### `egress_redaction` — egress/redaction behavior

**Description:** any path by which locally-observed data (event payloads,
claim text, transcript content) leaves the local trust boundary — export
spool, uploader, cloud ingest.
**Current controls:** `redact_json`/`redact_text` applied at the daemon's
ingestion boundary to every field that reaches storage, including
`tool_input`/`Claim.text` (fixed after FORNX-280, see "Known residual risk"
below for what was found); `fornax-uploader`'s `guard.rs` is an independent
last-line-of-defense check against unredacted secrets reaching the cloud,
covered by its own unit suite (14 passed, `docs/release/v0.0.1-qa-security-signoff.md`
§3.4) plus a live full-stack secret-egress canary re-verification.
**Known residual risk:** Critical, found and fixed — FORNX-280: a
random-hex canary reached the daemon log, SQLite store, and
`fornax export-spool` output unredacted via `AgentEvent.tool_input` and
`Claim.text`, before the ingestion-boundary fix above. Re-verified clean
after the fix. The full downstream `fornax-uploader` → ingest/Pub-Sub →
backend → Postgres → SaaS-UI pipeline was not re-exercised end-to-end after
the fix (a real, disclosed gap, not rounded up to PASS) — tracked in the
FORNX-239 Jira thread's coverage reconciliation.

### `cloud_identity_tenant` — cloud identity/tenant authorization

**Description:** owned by `fornax-cloud`, not this repo — the SaaS
backend's tenant isolation and authentication/authorization surface.
**Current controls:** out of this repo's scope; see `fornax-cloud`'s own
threat-model/security documentation once it exists.
**Known residual risk:** not assessed in this document. A candidate
spanning `fornax-cloud` with a `TRUST_BOUNDARY`-or-higher change to this
boundary needs a `fornax-cloud`-side threat-model entry, cross-referenced
here rather than duplicated.

### `browser_rendering_injection` — browser rendering/injection surface

**Description:** the `fornax-cloud` React frontend (Evidence dashboard)
renders claim/rationale/evidence text sourced from adapter-observed data.
**Current controls:** static sink enumeration found zero
`dangerouslySetInnerHTML`, zero `innerHTML`/`insertAdjacentHTML`/
`document.write`/`new Function`, no raw-HTML markdown renderer, no
`javascript:`-URI construction; rendered fields use plain JSX text-child
expressions, which React escapes by default
(`docs/release/v0.0.1-qa-security-signoff.md` §3.3). Confirmed live: a
Playwright session injected four XSS payloads
(`<script>`, `<img onerror>`, attribute-breakout `">`, `javascript:` URI)
into claim text end-to-end through the real frontend; zero execution, all
rendered as inert text (`docs/release/0001-fornx-238-dashboard-xss-check.md`).
**Known residual risk:** Informational — the dashboard's `html_escape`
helper does not escape quotes; safe today only because no rendered field is
placed inside an HTML attribute. Revisit if that changes.

### `event_transport` — event transport

**Description:** the path an event takes from adapter/hook to daemon
(local socket) and from daemon/spool to cloud ingest (network).
**Current controls:** see `daemon_socket` and `egress_redaction` above —
this boundary's controls are the union of both; no independent
event-transport-specific control exists beyond them today.
**Known residual risk:** see the two boundaries above.

### `judge_replay_execution` — judge/replay execution

**Description:** any component that replays or re-evaluates prior evidence
to (re)compute a verdict.
**Current controls:** not yet a distinct implemented surface in this repo
as of `threat_model_version: 1` — no dedicated judge/replay execution
component exists in the current crate set (`crates/`). This section is a
placeholder pending that capability's design.
**Known residual risk:** not assessed — nothing to assess yet. Adding a
judge/replay component requires a `TRUST_BOUNDARY`-or-higher classification
and a threat-model changelog entry here before release.

### `enterprise_policy_deployment` — enterprise policy/deployment

**Description:** any enterprise-managed policy or deployment configuration
surface (e.g. managed settings, org-wide policy enforcement).
**Current controls:** not yet a distinct implemented surface in this repo
as of `threat_model_version: 1`.
**Known residual risk:** not assessed — nothing to assess yet, same
placeholder status as `judge_replay_execution`.

### `sdk_plugin_trust` — SDK/plugin trust

**Description:** trust granted to third-party SDK integrations or plugins
consuming Fornax data or extending its behavior.
**Current controls:** not yet a distinct implemented surface in this repo
as of `threat_model_version: 1`.
**Known residual risk:** not assessed — nothing to assess yet, same
placeholder status as the two boundaries above.

## Changelog

| Version | Date | Candidate | Change |
|---|---|---|---|
| 1 | 2026-09-01 | (retroactive, v0.0.1) | Initial threat model, FORNX-233. Boundary descriptions for `daemon_socket`, `adapter_provider_input`, `evidence_provenance`, `egress_redaction`, `browser_rendering_injection`, `event_transport` populated retroactively from v0.0.1's actual sign-off evidence (`docs/release/v0.0.1-qa-security-signoff.md`, `docs/release/0001-fornx-238-dashboard-xss-check.md`). `cloud_identity_tenant`, `judge_replay_execution`, `enterprise_policy_deployment`, `sdk_plugin_trust` recorded as not-yet-implemented placeholders pending those capabilities' design. |
