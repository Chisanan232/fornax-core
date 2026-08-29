# CLAUDE.md — fornax-core

Read `~/.claude/CLAUDE.md` (global baseline) first. This file overrides it
where they conflict. See `CONTRIBUTING.md` for full development conventions
(branching, commits, PRs, merge, self-review) — not repeated here.

## Repository identity

- Repo: `horonomy/fornax-core`. Fornax is a Horonomy product — canonical org
  is `horonomy`, not a dedicated Fornax org. Public OSS.
- Language: Rust, edition 2021, workspace of crates under `crates/`.
- Jira: project `FORNX`, epic FORNX-20, discovery thesis HVDL-15.
- History: `main` was normalized 2026-08-28 (FORNX-52, owner-authorized
  one-time exception to the no-force-push-main rule). Prior implementation
  is preserved at tag `archive-v0.0.1-bootstrap` and is being replayed
  ticket-by-ticket as reviewed PRs — see `docs/migration/0001-pr-governance-migration.md`.

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
- Use `CARGO_TARGET_DIR=./target` — the machine's shared
  `~/.cargo/shared-target` has tens of thousands of unrelated `deps` entries
  from other projects and makes builds extremely slow.

## Merge strategy

PR-only to `main`, create-a-merge-commit (never squash/rebase-merge). Branch
naming: `v0.0.1/FORNX-<n>/<type>/<snake_case_slug>`.

## Source of truth

- Jira: `FORNX` project, cloudId `f15c3ffb-740e-4db1-9b6b-12ccba3e897a`
  (site `lightning-dust-mite.atlassian.net`).
- `docs/research/adapter-capability-matrix.md` — empirically confirmed
  Claude Code hook payload shapes and Codex rollout JSONL schema. Re-verify
  against the installed CLI version before trusting field names on the
  Codex side; that surface moves fast.
