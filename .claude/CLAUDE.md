# CLAUDE.md — fornax-core

Read `~/.claude/CLAUDE.md` (global baseline) first. This file overrides it
where they conflict.

## Repository identity

- Repo: `Chisanan232/fornax-core` (personal account; transfer to a dedicated
  Fornax GitHub Org pending — see FORNX-21). Public OSS.
- Language: Rust, edition 2021, workspace of crates under `crates/`.
- Jira: project `FORNX`, epic FORNX-20, discovery thesis HVDL-15.

## Architecture constraints

See `docs/adr/0001-architecture-invariants.md` — modular monolith, one local
daemon process, no cloud dependency on the local critical path, immutable
observation before interpretation, five-state verdict vocabulary never
collapsed, adapters stay thin.

## Commands

- Build: `cargo build --workspace`
- Test: `cargo test --workspace`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all` (check: `cargo fmt --all -- --check`)

## Testing

- `sqlx` runtime queries (not compile-time `query!` macros) — no
  `DATABASE_URL`/offline cache needed to build.
- Verifiers (`fornax-verify`) are pure — unit-tested with in-memory
  `Evidence`/`Claim` fixtures, no daemon/DB required.

## Merge strategy

PR-only to `main`, no direct pushes. Branch naming:
`v0.0.1/FORNX-<n>/<type>/<snake_case_slug>` (matches Horonom family
convention).

## Source of truth

- Jira: `FORNX` project, cloudId `f15c3ffb-740e-4db1-9b6b-12ccba3e897a`
  (site `lightning-dust-mite.atlassian.net`).
- Research: `docs/research/adapter-capability-matrix.md` — empirically
  confirmed Claude Code hook payload shapes and Codex rollout JSONL schema.
  Re-verify against the installed CLI version before trusting field names on
  the Codex side; that surface moves fast.
