# Fornax

Evidence-first agent-integrity system for coding agents (Claude Code, Codex).

**What should I believe about what this agent is telling me, given the
evidence currently available?**

Fornax watches a coding agent session in real time, captures immutable
evidence (tool calls, exit codes, transcripts), and checks the agent's own
claims against that evidence — surfacing `VERIFIED` / `UNVERIFIED` /
`CONTRADICTED` / `REVIEW` / `UNAVAILABLE`, never a made-up trust score.

## Status

Implementation is being replayed ticket-by-ticket as reviewed PRs after a
one-time history normalization — see
`docs/migration/0001-pr-governance-migration.md` for what moved where, and
`docs/adr/` for the architecture invariants. `main` now has a working local
daemon, verifiers, adapters, and CLI (see Quick Start below); the prior
bootstrap implementation this was replayed from is preserved in full at tag
`archive-v0.0.1-bootstrap` / branch `archive/pre-pr-governance-20260828`.

Jira: FORNX-20 (epic), FORNX-52 (this migration).

## Quick Start

Reproduces the "aha moment" locally: an agent claims its tests passed while
the actual command it ran exited non-zero, and Fornax surfaces the
contradiction — no cloud dependency required.

```bash
cargo build --workspace

# Everything below reads/writes under $FORNAX_HOME (defaults to ~/.fornax).
# Use a scratch directory so this doesn't touch a real Fornax install.
export FORNAX_HOME=/tmp/fornax-quickstart
rm -rf "$FORNAX_HOME"

# 1. Start the daemon (owns the local DB + Unix socket + localhost HTTP API
#    on :4317). Leave this running in its own terminal/background job.
./target/debug/fornax-daemon &

# 2. Feed it hook events directly on stdin, exactly as Claude Code would
#    invoke `fornax-hook-claude` (see that binary's module doc for the real
#    ~/.claude/settings.json wiring). This simulates: the agent ran a Bash
#    command that failed (PostToolUse, exit_code 1), then claimed at the end
#    of the turn that tests passed (Stop, transcript with that claim text).
SESSION=quickstart-demo

echo '{"hook_event_name":"PostToolUse","session_id":"'"$SESSION"'","tool_name":"Bash","tool_input":{"command":"cargo test --workspace"},"tool_response":{"exit_code":1,"stdout":"","stderr":"test failed"}}' \
  | ./target/debug/fornax-hook-claude

cat > /tmp/fornax-quickstart-transcript.jsonl <<'EOF'
{"type":"assistant","message":{"content":[{"type":"text","text":"All tests passed."}]}}
EOF
echo '{"hook_event_name":"Stop","session_id":"'"$SESSION"'","transcript_path":"/tmp/fornax-quickstart-transcript.jsonl"}' \
  | ./target/debug/fornax-hook-claude

# 3. Check the verdict — CONTRADICTED, with the rationale spelling out why.
./target/debug/fornax status   # compact one-liner: 🛡 ✕ CONTRADICTED
./target/debug/fornax detail   # full claim/evidence/rationale/verifier detail
```

`fornax-hook-codex` (Codex CLI) and the dashboard at `/dashboard` on the
daemon's HTTP port work the same way — see
`docs/research/adapter-capability-matrix.md` for the Codex hook payload
shape and `crates/fornax-adapter-codex` for that adapter's translation
logic.

## Local storage, privacy and known limitations

Everything above reads/writes under `$FORNAX_HOME` (default `~/.fornax`): an
on-disk SQLite database, a Unix domain socket, and a localhost-only HTTP API.
Nothing leaves your machine unless you explicitly turn on cloud sync
(`FORNAX_CLOUD_SYNC_ENABLED`, see below) — there is no telemetry and no
hosted Beta/production service to opt out of, because none exists at this
version. `$FORNAX_HOME`'s on-disk schema is not yet guaranteed stable across
releases.

Claude Code and Codex CLI support is not symmetric: Codex's hook surface is
opt-in and can be admin-disabled, so its adapter relies primarily on tailing
Codex's own rollout-file transcripts rather than hooks — see
`docs/research/adapter-capability-matrix.md` for the exact, empirically
verified differences before relying on a specific hook/field existing on the
Codex side.

### Exporting a session for cloud sync (optional)

`fornax export-spool` reads `$FORNAX_HOME/fornax.db` directly (no daemon
dependency) and writes one wire-compatible envelope JSON file per
event/claim/evidence/capability into `<out>/pending/`, in the layout
`horonomy/fornax-cloud`'s `fornax-uploader` spool expects:

```bash
./target/debug/fornax export-spool --session "$SESSION" --out /tmp/fornax-uploader-spool
```

Point `fornax-uploader`'s `FORNAX_UPLOADER_SPOOL_DIR` at that same directory
to sync the exported session to a running fornax-cloud stack — see
`horonomy/fornax-infra`'s README for standing up that stack locally, and
`horonomy/fornax-cloud`'s README for running `fornax-uploader` itself. Cloud
sync is opt-in and off by default (`FORNAX_CLOUD_SYNC_ENABLED`) — nothing
above requires it, and the local daemon/CLI path works fully with cloud
access disabled.

## License

MIT.
