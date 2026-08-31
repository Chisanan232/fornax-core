#!/usr/bin/env bash
# Fornax Codex `notify` wrapper (FORNX-17 project-scoped dogfooding).
#
# Codex has no `statusLine`-equivalent host surface (see
# docs/research/adapter-capability-matrix.md — Codex's only live-refreshing
# terminal UI is its own, not extensible by a third-party command the way
# Claude Code's `statusLine` config is). The closest supported,
# non-invasive equivalent is the legacy `notify`/`notify_command`
# mechanism: a single coarse `agent-turn-complete` event, fired once a
# turn ends, with a JSON payload as the invoked command's last argument
# (fields confirmed against the installed codex-cli binary: `type`,
# `thread-id`, `turn-id`, `cwd`, `client`, `input-messages`,
# `last-assistant-message`).
#
# Configure in `~/.codex/config.toml`:
#
#   notify = ["<REPO_ROOT>/scripts/fornax-codex-notify.sh"]
#
# Codex appends the JSON payload as this script's final argument. On
# `agent-turn-complete`, this prints the same compact ambient segment
# `fornax status` gives Claude Code's status line — low-noise by
# construction, since it only fires once per turn, not continuously.
#
# Fails safe: a missing/unbuilt `fornax` binary, an unreachable daemon, or
# a payload this script can't parse all degrade to a quiet no-op/plain
# message rather than breaking the user's Codex turn.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FORNAX_BIN="$REPO_ROOT/target/debug/fornax"

PAYLOAD="${*: -1}"

EVENT_TYPE=""
if command -v jq >/dev/null 2>&1 && [ -n "$PAYLOAD" ]; then
  EVENT_TYPE="$(printf '%s' "$PAYLOAD" | jq -r '.type // empty' 2>/dev/null || true)"
fi

# Only the turn-complete event is a meaningful moment to surface ambient
# state; an unrecognized/unparseable payload is treated the same way (no
# noisy output for every notify invocation Codex might ever add).
if [ -n "$EVENT_TYPE" ] && [ "$EVENT_TYPE" != "agent-turn-complete" ]; then
  exit 0
fi

if [ -x "$FORNAX_BIN" ]; then
  "$FORNAX_BIN" status 2>/dev/null || echo "🛡 fornax: error"
else
  echo "🛡 fornax: not built (run cargo build --workspace)"
fi
