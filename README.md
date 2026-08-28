# Fornax

Evidence-first agent-integrity system for coding agents (Claude Code, Codex).

**What should I believe about what this agent is telling me, given the
evidence currently available?**

Fornax watches a coding agent session in real time, captures immutable
evidence (tool calls, exit codes, transcripts), and checks the agent's own
claims against that evidence — surfacing `VERIFIED` / `UNVERIFIED` /
`CONTRADICTED` / `REVIEW` / `UNAVAILABLE`, never a made-up trust score.

## Status

Repository governance baseline. Implementation is being replayed
ticket-by-ticket as reviewed PRs after a one-time history normalization —
see `docs/migration/0001-pr-governance-migration.md` for what moved where,
and `docs/adr/` for the architecture invariants. The prior bootstrap
implementation (Cargo workspace with a working local daemon, verifiers,
adapters, CLI) is preserved in full at tag `archive-v0.0.1-bootstrap` /
branch `archive/pre-pr-governance-20260828`.

Jira: FORNX-20 (epic), FORNX-52 (this migration).

## License

MIT.
