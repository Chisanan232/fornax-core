# Changelog

All notable changes to Fornax are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow the
canonical Fornax release sequence (`v0.0.1` → … → `v0.1.0` GA) tracked in
Jira epic FORNX-20.

## [Unreleased]

Nothing yet — v0.0.2 planning has not started implementation.

## [v0.0.1] — Local Evidence MVP

Frozen candidate (see `release/v0.0.1-candidate-manifest.json` and
`docs/release/v0.0.1-qa-security-signoff.md`):

| Repo | Commit |
|---|---|
| `horonomy/fornax-core` | `1c078ed31c23ffac8f515e8a46c97c1888c76457` |
| `horonomy/fornax-cloud` | `84f02a0c8c0a14ca64176c22799ba9f4b50c1b4f` |
| `horonomy/fornax-infra` | `13f17453776dcfa63971e18b2b64bec4aa621abc` |
| `horonomy/fornax-docs` | `d7c42731c16b27caf2149817263ba78969ab5937` |
| `horonomy/fornax-website` | `e351cd579adc9d55362ee15a7b07c735223c9f74` |

### Capabilities

- Local daemon (`fornax-daemon`) owning an on-disk SQLite store, a Unix
  domain socket, and a localhost-only HTTP API + dashboard on `:4317`. No
  cloud dependency on this path.
- Real-time evidence capture from **Claude Code** (`fornax-hook-claude`, via
  `PreToolUse`/`PostToolUse`/`SessionStart`/`UserPromptSubmit`/
  `SubagentStart`/`SubagentStop` hooks) and **Codex CLI**
  (`fornax-hook-codex`, primarily via rollout-file tailing — see
  `docs/research/adapter-capability-matrix.md` for the exact, empirically
  verified capability differences between the two adapters; they are not
  equivalent).
- Claim-vs-evidence verification producing exactly one of five verdicts:
  `VERIFIED` / `UNVERIFIED` / `CONTRADICTED` / `REVIEW` / `UNAVAILABLE` —
  never a numeric trust score, never collapsed to fewer states.
- `fornax status` / `fornax detail` CLI, plus the daemon's `/dashboard` view,
  showing the same verdict/claim/evidence/rationale consistently.
- `fornax export-spool` for opt-in sync of a local session to a running
  `fornax-cloud` stack (`fornax-uploader` → ingest → Pub/Sub emulator →
  backend → Postgres → SaaS UI, all runnable locally per
  `horonomy/fornax-infra`'s README).

### Supported environments

- macOS/Linux, Rust workspace built with `cargo build --workspace`.
- Claude Code (hooks wired via `~/.claude/settings.json`) and Codex CLI
  (rollout-file tailer; hooks are opt-in on the Codex side and not required).

### Known limitations

- Cloud sync is **opt-in and off by default**
  (`FORNAX_CLOUD_SYNC_ENABLED`) — nothing in the local Quick Start requires
  it, and there is no hosted Beta or production SaaS offering at this
  version.
- No enterprise governance features, no causal verification, no
  deception/lie-detection capability. Fornax checks agent claims against
  captured tool-call evidence only.
- Codex integration has real, documented gaps versus Claude Code (see
  `docs/research/adapter-capability-matrix.md`): no universal tool-call
  interception with input rewriting, no stable versioned hook schema, hooks
  are opt-in and can be admin-disabled — the rollout-file tailer is the
  primary/durable integration path for Codex, not hooks.
- All data stays local under `$FORNAX_HOME` (default `~/.fornax`, an
  on-disk SQLite database) unless cloud sync is explicitly enabled.

### Upgrade expectations

This is the first tagged release; there is no prior version to upgrade
from. `$FORNAX_HOME`'s on-disk schema is not yet guaranteed stable across
versions — back up or discard `$FORNAX_HOME` before upgrading past v0.0.1
until a migration policy is published.

See `README.md` for the Quick Start and `docs/release/v0.0.1-release-notes.md`
for the canonical public release notes.
