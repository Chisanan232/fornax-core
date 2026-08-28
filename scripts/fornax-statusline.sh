#!/usr/bin/env bash
# Fornax status-line wrapper (FORNX-30 project-scoped dogfooding).
#
# A project-level `statusLine` command fully REPLACES the user's global one
# for sessions rooted in this project (Claude Code does not merge non-list
# settings across scopes). This script preserves that behavior instead of
# silently dropping it: it invokes the user's existing global
# ~/.claude/statusline.py with the exact same stdin JSON Claude Code gave
# this script, then appends the Fornax segment as an additional line.
#
# Fails safe: a missing global statusline.py, or an unreachable Fornax
# daemon, degrades gracefully rather than breaking the status line.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GLOBAL_STATUSLINE="${HOME}/.claude/statusline.py"

INPUT="$(cat)"

ORIGINAL=""
if [ -f "$GLOBAL_STATUSLINE" ]; then
  ORIGINAL="$(printf '%s' "$INPUT" | python3 "$GLOBAL_STATUSLINE" 2>/dev/null || true)"
fi

FORNAX_BIN="$REPO_ROOT/target/debug/fornax"
if [ -x "$FORNAX_BIN" ]; then
  FORNAX_SEG="$("$FORNAX_BIN" status 2>/dev/null || echo '🛡 fornax: error')"
else
  FORNAX_SEG="🛡 fornax: not built (run cargo build --workspace)"
fi

if [ -n "$ORIGINAL" ]; then
  printf '%s\n%s\n' "$ORIGINAL" "$FORNAX_SEG"
else
  printf '%s\n' "$FORNAX_SEG"
fi
