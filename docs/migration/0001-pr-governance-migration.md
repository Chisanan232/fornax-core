# Migration 0001: PR-governance history normalization

Status: in progress
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
| `4af1863` | 🔧 repo: Scaffold Fornax Rust workspace, CI, and repo conventions | FORNX-23 | TBD | pending replay |
| `d43a52a` | 🔧 repo: Untrack harness scheduled_tasks.lock file | (housekeeping, no ticket) | n/a — folded into FORNX-23 replay | pending |
| `15fd6b6` | ✨ types: Add canonical AgentEvent/Claim/Evidence/Finding contracts | FORNX-24 | TBD | pending replay |
| `3b1869b` | ✨ store: Add immutable SQLite/WAL evidence store | FORNX-26 | TBD | pending replay |
| `275ea7b` | ✨ verify: Add deterministic TestResultVerifier | FORNX-27 | TBD | pending replay |
| `70509c1` | ✨ daemon: Add local daemon — UDS intake, verifier pipeline, localhost API | FORNX-25 (+FORNX-31/32 endpoints) | TBD | pending replay — daemon split from status-line/dashboard Stories per Phase 2 reclassification |
| `2a78a58` | ✨ adapter(claude): Add Claude Code hook adapter | FORNX-28 | TBD | pending replay |
| `b6971f1` | ✨ adapter(codex): Add Codex rollout-tail adapter | FORNX-29 | TBD | pending replay |
| `89a6913` | ✨ cli: Add fornax status/detail commands | FORNX-31 (detail command Story); status-line half → FORNX-30 | TBD (may split into two PRs) | pending replay |
| `f9fec8a` | 📝 docs: Add architecture ADRs, capability matrix research, delivery state | FORNX-22 | TBD | pending replay |
| `ab0c73e` | ✨ privacy: Add secret-pattern redaction at the ingest boundary | FORNX-33 (partial) | TBD — must be completed, not replayed as-is (see Jira note: partial classifier alone does not close FORNX-33) | pending, scope extension required |
| `a3d9977` | 📝 docs: Update delivery state | (doc-only, no ticket) | folded into whichever PR lands last | pending |

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

- [ ] Every row above has a real PR number, not TBD.
- [ ] `git diff archive-v0.0.1-bootstrap main -- crates/ docs/` reviewed —
      every difference is either a governance addition, a deliberate
      completion of a partial ticket (FORNX-30, FORNX-33), or deliberately
      excluded pending functionality on an unmerged branch. Nothing missing
      by accident.
- [ ] Full `cargo build/test/fmt/clippy` green on new main.
- [ ] Jira statuses/comments reflect the new PR links, not the old commit SHAs.
