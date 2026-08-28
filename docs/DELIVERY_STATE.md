# Fornax delivery state

Last updated: 2026-08-28 (post-build-fix). Resume by reading this file +
`git log` + `docs/adr/*` before re-deriving anything — do not re-plan from
zero.

**Build/test status as of last commit: GREEN.** `cargo build --workspace`,
`cargo test --workspace` (9/9 pass), `cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets -- -D warnings` all clean. Use
`CARGO_TARGET_DIR=./target` — the machine's shared `~/.cargo/shared-target`
has ~65k unrelated `deps` entries and makes builds extremely slow.

Repo pushed: https://github.com/Chisanan232/fornax-core (main, 11 commits).
Jira FORNX-22..29/31/32 marked Done; FORNX-30/33/34 marked In Progress
(partial/unproven, see their Jira comments); FORNX-35..51 untouched (To Do).

## Jira snapshot (FORNX-20 epic, 31 children, all started "To Do")

| Ticket | Status this session |
|---|---|
| FORNX-21 GitHub org | Blocked on human (org creation needs browser). Comment posted. Repo created under personal account instead (`Chisanan232/fornax-core`), transfer later. |
| FORNX-22 Architecture/OSS invariants | Done — `docs/adr/0001-architecture-invariants.md` |
| FORNX-23 Repo/build skeleton | Done — Cargo workspace, CI, README/CLAUDE.md/AGENTS.md/LICENSE |
| FORNX-24 Canonical types | Done — `crates/fornax-types`, empirically grounded (see capability matrix) |
| FORNX-25 Daemon bootstrap | Done — `crates/fornax-daemon` (UDS intake + axum HTTP, single process) |
| FORNX-26 Immutable local store | Done — `crates/fornax-store` (SQLite/WAL, 0600 perms) |
| FORNX-27 Deterministic verifier | Done — `crates/fornax-verify`, `TestResultVerifier` (the epic's canonical aha scenario), unit-tested |
| FORNX-28 Claude Code adapter | Done (code) — `crates/fornax-adapter-claude`; **not wired into `~/.claude/settings.json`** (global user config, left as documented manual step) |
| FORNX-29 Codex adapter | Done (code) — `crates/fornax-adapter-codex`, rollout-tail based (hooks are opt-in/unstable, see capability matrix) |
| FORNX-30 Status-line | Partial — `fornax status` produces the compact segment; not wired as a status-line segment in any global config (same reasoning as FORNX-28) |
| FORNX-31 Detail command | Done — `fornax detail` |
| FORNX-32 Localhost dashboard | Done — `GET /dashboard` on the daemon's axum server |
| FORNX-33 Privacy/redaction | **Partial.** `fornax_types::redact` — pattern-based secret classifier (GitHub tokens, `TOKEN=`/`SECRET=`-style assignments, generic high-entropy tokens) applied at the daemon's ingest boundary before persistence. 4 unit tests. No policy allowlist, no cloud-egress gating (moot — no cloud code exists). Real non-blocking finding recorded on this ticket: Codex rollout files can leak secrets in captured command output (owner: risk accepted/deferred). Store file permissions (0600) implemented as a partial mitigation for Fornax's *own* DB. |
| FORNX-34 LOCAL READY gate | **Proven with a synthetic rollout file, not a real live session.** Full pipeline verified: synthetic Codex-shaped `exec_command_end{exit_code:1}` + `task_complete{last_agent_message:"All tests passed."}` → daemon → `CONTRADICTED` → identical result via `fornax status`, `fornax detail`, `/dashboard`. Real live Claude Code (Stop hook wired) and real live Codex session runs still needed before this gate can actually close. |
| FORNX-35..50 (cloud/SaaS/docs/website/payment/research/validation) | **Not started.** Sequenced after Gate 2 (LOCAL READY) per the epic's hard gate — correctly out of scope for this session. |
| FORNX-51 Jira admin metadata | Not started. |

## What actually works right now (once `cargo build --workspace` finishes)

```
fornax-daemon                    # UDS + http://127.0.0.1:4317
fornax-hook-codex                # tails newest ~/.codex/sessions/**/*.jsonl
fornax status / fornax detail    # CLI reading the daemon's API
http://127.0.0.1:4317/dashboard  # HTML view of recent findings
```

`fornax-hook-claude` exists but requires the user to add hook entries to
`~/.claude/settings.json` (see the comment block at the top of
`crates/fornax-adapter-claude/src/main.rs`) — this was deliberately not done
automatically since it's the user's global config, not this repo.

## Next session — pick up here

1. Confirm `cargo build --workspace` is green (`CARGO_TARGET_DIR=./target` —
   the machine's shared cargo target dir at `~/.cargo/shared-target` has
   ~65k `deps` entries from unrelated projects and makes builds extremely
   slow; use a local `target/` for this repo, or investigate whether the
   shared-target trade-off documented in `~/CLAUDE.md` needs a fornax-specific
   carve-out).
2. Run `cargo test --workspace` — `fornax-verify`'s unit tests are the
   acceptance evidence for FORNX-27.
3. First commit(s): the repo is `git init`'d locally but has **zero commits**
   as of this write-up — everything above is uncommitted working-tree state.
   Commit in the granular style the global CLAUDE.md commit policy requires
   (one commit per crate/concern), then push to `Chisanan232/fornax-core`
   (GitHub repo creation was blocked by the permission classifier this
   session — needs the user's explicit go-ahead or a manual `gh repo create` /
   push, since `git remote` isn't configured yet either).
4. Prove FORNX-34 live: run a real Codex session that runs a failing
   `pytest`/`cargo test` and then claims success in `last_agent_message`;
   confirm the daemon logs a `CONTRADICTED` finding and `/dashboard` shows it,
   with `FORNAX_HOME` cloud-disabled (there's no cloud code yet, so this
   should already hold — verify explicitly, don't assume).
5. Start FORNX-33 (privacy/redaction) before FORNX-34 is declared complete —
   the epic's own Gate 2 acceptance implies redaction-aware evidence, and the
   secret-leak finding above makes this concretely urgent, not theoretical.
6. FORNX-21: ask the user whether the GitHub org has been created; if so,
   transfer `fornax-core` into it.

## Known accepted risk (do not re-raise)

Historical local Codex rollout files were found to contain plaintext
credentials from a past unrelated session; owner explicitly decided
risk-accept/defer, no rotation, no further forensics. See comment on
FORNX-33 in Jira. Do not re-surface secret values or re-investigate.
