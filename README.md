# Fornax

Evidence-first agent-integrity system for coding agents (Claude Code, Codex).

**What should I believe about what this agent is telling me, given the
evidence currently available?**

Fornax watches a coding agent session in real time, captures immutable
evidence (tool calls, exit codes, transcripts), and checks the agent's own
claims against that evidence — surfacing `VERIFIED` / `UNVERIFIED` /
`CONTRADICTED` / `REVIEW` / `UNAVAILABLE`, never a made-up trust score.

Canonical aha scenario: an agent says *"All tests passed"*; Fornax observed
`exit_code=1`; Fornax surfaces `🛡 ✕ CONTRADICTED`, with the exact evidence and
provenance available on demand.

Local-first: the full path (capture → verify → status line → detail command →
localhost dashboard) works with cloud access disabled. See
`docs/adr/0001-architecture-invariants.md`.

## Status

v0.0.1 MVP, local-first vertical slice in progress. Jira: FORNX-20 (epic),
FORNX-25..34 (local runtime path).

## Layout

- `crates/fornax-types` — canonical `AgentEvent`/`Claim`/`Evidence`/`Finding` (FORNX-24)
- `crates/fornax-store` — SQLite/WAL immutable evidence store (FORNX-26)
- `crates/fornax-verify` — deterministic verifiers (FORNX-27)
- `crates/fornax-daemon` — the one local process: UDS intake + localhost HTTP (FORNX-25/30/31/32)
- `crates/fornax-adapter-claude` — Claude Code hook adapter (FORNX-28)
- `crates/fornax-adapter-codex` — Codex rollout-tail adapter (FORNX-29)
- `crates/fornax-cli` — `fornax status` / `fornax detail` (FORNX-31)

## Run locally

```bash
cargo build --workspace
./target/debug/fornax-daemon &          # starts UDS intake + http://127.0.0.1:4317
./target/debug/fornax-hook-codex &      # tails the newest Codex rollout file
./target/debug/fornax status            # compact status-line segment
./target/debug/fornax detail            # full finding detail
open http://127.0.0.1:4317/dashboard    # localhost dashboard
```

Claude Code integration requires wiring `fornax-hook-claude` into
`~/.claude/settings.json` hooks (see comment at the top of
`crates/fornax-adapter-claude/src/main.rs`) — not done automatically, since
that file is your global config.

## License

MIT.
