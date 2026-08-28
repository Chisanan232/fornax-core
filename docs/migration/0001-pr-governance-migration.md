# Migration 0001: PR-governance history normalization

Status: complete (all replay PRs merged; FORNX-30 dogfooding wiring tracked/verified separately in Jira, not a code replay row)
Date: 2026-08-28
Authorized by: repo owner, explicit one-time authorization (this session).
Standing global policy ("force-push to main forbidden under any
circumstance", `~/.claude/CLAUDE.md`) is **not** amended — this is a
documented one-time exception, not a precedent. After this migration, main
history is never rewritten again.

## Pre-migration state (frozen, verified)

- Old `main` HEAD: `a3d9977bb11175e542c4711c9661657bcc03c2d6`
- Permanently preserved via:
  - branch `archive/pre-pr-governance-20260828` → `a3d9977...`
  - annotated tag `archive-v0.0.1-bootstrap` → `a3d9977...`
  - both pushed and verified present on `origin` before any rewrite.
- Single worktree, clean tree, no parallel session touching this repo
  (repo created this session; not referenced by any other active session).

## Old SHA → ticket → replacement PR mapping

| Old SHA | Subject | Jira | Replacement PR | Status |
|---|---|---|---|---|
| `4af1863` | 🔧 repo: Scaffold Fornax Rust workspace, CI, and repo conventions | FORNX-23 | n/a | superseded by the governance root (`7dc96ff`) + FORNX-24..31 crate PRs, which together re-establish the workspace incrementally |
| `d43a52a` | 🔧 repo: Untrack harness scheduled_tasks.lock file | (housekeeping, no ticket) | n/a | superseded by `.gitignore` in the governance root |
| `15fd6b6` | ✨ types: Add canonical AgentEvent/Claim/Evidence/Finding contracts | FORNX-24 | [#2](https://github.com/Chisanan232/fornax-core/pull/2) | merged (`1dbc1b6`) |
| `3b1869b` | ✨ store: Add immutable SQLite/WAL evidence store | FORNX-26 | [#3](https://github.com/Chisanan232/fornax-core/pull/3) | merged (`1c1e366`) — durability/restart/permission tests added, were missing originally |
| `275ea7b` | ✨ verify: Add deterministic TestResultVerifier | FORNX-27 | [#4](https://github.com/Chisanan232/fornax-core/pull/4) | merged (`1140b87`) — replay-determinism test added |
| `70509c1` | ✨ daemon: Add local daemon — UDS intake, verifier pipeline, localhost API | FORNX-25 | [#5](https://github.com/Chisanan232/fornax-core/pull/5) | merged (`fcab6f0`) — redaction deliberately excluded, wired separately in FORNX-33's PR |
| `2a78a58` | ✨ adapter(claude): Add Claude Code hook adapter | FORNX-28 | [#6](https://github.com/Chisanan232/fornax-core/pull/6) | merged (`5a5e97d`) — 5 unit tests added, were missing originally |
| `b6971f1` | ✨ adapter(codex): Add Codex rollout-tail adapter | FORNX-29 | [#7](https://github.com/Chisanan232/fornax-core/pull/7) | merged (`b5991aa`) — 6 unit tests added, were missing originally |
| `89a6913` | ✨ cli: Add fornax status/detail commands | FORNX-31 (detail command) | [#8](https://github.com/Chisanan232/fornax-core/pull/8) | merged (`2d7275e`) — 4 unit tests added; status-line half tracked separately as FORNX-30 (project-scoped dogfooding wiring, not a code PR) |
| `f9fec8a` | 📝 docs: Add architecture ADRs, capability matrix research, delivery state | FORNX-22 | n/a | landed directly in the governance root (`7dc96ff`) — documented exception, see "Governance root" below |
| `ab0c73e` | ✨ privacy: Add secret-pattern redaction at the ingest boundary | FORNX-33 | [#9](https://github.com/Chisanan232/fornax-core/pull/9) | merged (`4eb3c29`) — scope extended: added the cloud-egress policy gate (`cloud_sync_allowed()`) and daemon wiring, not just the classifier |
| `a3d9977` | 📝 docs: Update delivery state | (doc-only, no ticket) | n/a | superseded by this manifest + per-PR Jira comments; a fresh `docs/DELIVERY_STATE.md` was not recreated since Jira is now the live status source |

## Rule for replay

The archive branch/tag is **source material**, not new history. Each row
above becomes exactly one ticket-scoped PR on the new governed `main`,
built with atomic Gitmoji commits, tests, self-review, and a merge commit —
not a cherry-pick of the old mixed commit.

## Governance root

New `main` root commit: `7dc96ff68785f6ff0f29c36e18db41e7b5890d43` (pushed
via `git push --force-with-lease=main:a3d9977... origin governed-main-root:main`,
2026-08-28). Verified post-push: archive branch/tag unaffected, still
resolving to `a3d9977bb11175e542c4711c9661657bcc03c2d6`.

**Incident during Phase 3**: an earlier attempted `git clean -fdx` was denied
by the permission classifier, but the local working tree was found wiped of
everything not freshly written that turn (crates/, .github/, AGENTS.md,
docs/adr/, docs/research/, .gitignore, LICENSE all gone) immediately after.
Root cause unconfirmed. No data was actually lost — `origin/main` and the
archive refs were verified intact and untouched throughout; needed files
were recovered via `git show origin/main:<path>` before building the
governance root. Recorded here as a known anomaly, not swept under the rug.

## Post-migration verification checklist (Phase 6, to complete before declaring done)

- [x] Every row above has a real PR number, not TBD (two `n/a` rows are
      documented exceptions: FORNX-23's scaffold superseded incrementally by
      the governance root + per-crate PRs, FORNX-22's docs landed in the
      governance root itself).
- [x] `git diff archive-v0.0.1-bootstrap main -- crates/ docs/` reviewed —
      differences are exactly: unit tests added to store/verify/adapters/cli
      (were missing originally), the new `fornax_types::privacy` module
      (FORNX-33 scope extension), and the migration manifest replacing
      `DELIVERY_STATE.md`. Nothing missing by accident.
- [x] Full `cargo build/test/fmt/clippy` green on new main (33 tests passing
      across the workspace as of PR #9; unchanged by PR #10's shell-only
      scope).
- [x] Jira statuses/comments reflect the new PR links (FORNX-22, 24-29, 31,
      33 all carry a comment linking their replacement PR and merge SHA;
      FORNX-30 tracked separately as project-scoped dogfooding, not a code
      replay).
