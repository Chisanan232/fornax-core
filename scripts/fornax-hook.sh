#!/usr/bin/env bash
# Fornax Claude Code hook wrapper (FORNX-30 project-scoped dogfooding).
#
# Resolves fornax-hook-claude relative to this script's own location, never
# a hardcoded machine-specific path, so this file is safe to commit and
# reuse on any machine that has this repo checked out and built.
#
# Fails safe: if the binary isn't built yet, drain stdin and exit 0 quietly
# rather than breaking the hook chain for unrelated tools (rtk, codegraph,
# etc. also wired to this event).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$REPO_ROOT/target/debug/fornax-hook-claude"

if [ -x "$BIN" ]; then
  exec "$BIN"
else
  cat >/dev/null
  exit 0
fi
