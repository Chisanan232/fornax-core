# Dogfooding Fornax's status line in this project (FORNX-30)

Isolated to this repository only — never touches your global Claude Code
config (`~/.claude/settings.json`, `~/.claude/statusline.py`). Uses Claude
Code's project-local settings scope instead.

## Why project-scoped, not global

A Fornax integration bug must not make Claude Code sessions in unrelated
repositories misbehave, and this integration shouldn't be imposed on other
contributors who clone this repo. Claude Code's `.claude/settings.local.json`
is scoped to exactly one project (this repo/worktree), is never committed
(Claude Code adds it to your global git excludes automatically the first
time it writes there), and takes precedence over both the shared project
`.claude/settings.json` and your user-global `~/.claude/settings.json`.

## Setup

1. Build the binaries: `cargo build --workspace` (from the repo root).
2. Start the daemon once per session: `./target/debug/fornax-daemon &`
3. Create `.claude/settings.local.json` in this repo's root (main checkout,
   not a worktree) with the contents below — replace every `<REPO_ROOT>`
   with this repo's real absolute path on your machine (settings.local.json
   is per-machine and gitignored, so a hardcoded local path here is correct,
   unlike in committed files).

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] },
      { "matcher": "Grep|Glob", "hooks": [{ "type": "command", "command": "~/.claude/hooks/cbm-code-discovery-gate", "timeout": 5 }] }
    ],
    "PostToolUse": [
      { "matcher": "Bash|Monitor|Workflow", "hooks": [{ "type": "command", "command": "python3 ~/.claude/hooks/bg-track.py", "timeout": 3 }] },
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "<REPO_ROOT>/scripts/fornax-hook.sh" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "<REPO_ROOT>/scripts/fornax-hook.sh" }] }
    ],
    "SubagentStart": [{ "hooks": [{ "type": "command", "command": "python3 ~/.claude/hooks/bg-track.py", "timeout": 3 }] }],
    "SubagentStop": [{ "hooks": [{ "type": "command", "command": "python3 ~/.claude/hooks/bg-track.py", "timeout": 3 }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "codegraph prompt-hook" }] }],
    "SessionStart": [
      { "matcher": "startup", "hooks": [{ "type": "command", "command": "~/.claude/hooks/cbm-session-reminder" }] },
      { "matcher": "resume", "hooks": [{ "type": "command", "command": "~/.claude/hooks/cbm-session-reminder" }] },
      { "matcher": "clear", "hooks": [{ "type": "command", "command": "~/.claude/hooks/cbm-session-reminder" }] },
      { "matcher": "compact", "hooks": [{ "type": "command", "command": "~/.claude/hooks/cbm-session-reminder" }] }
    ]
  },
  "statusLine": {
    "type": "command",
    "command": "<REPO_ROOT>/scripts/fornax-statusline.sh",
    "padding": 1,
    "refreshInterval": 3
  }
}
```

**Why the global hook entries are copied in verbatim**: Claude Code's docs
don't guarantee whether a project-level `hooks` object merges with the
global one or replaces it outright (see the FORNX-30 Jira comment for the
research). Treating it as full-replace and copying every existing global
hook entry forward, alongside the new Fornax ones, is what prevents `rtk`,
`codegraph`, and the other global hooks from silently stopping inside this
one project.

**Why the status line is a wrapper script, not a raw Fornax command**: a
project-level `statusLine` fully replaces the global one (it's a single
command, not a mergeable list) — `scripts/fornax-statusline.sh` calls your
existing `~/.claude/statusline.py` with the same stdin JSON first, so its
output is preserved, then appends the Fornax segment as an extra line.

## Verifying isolation

From inside this repo: open a Claude Code session, run a Bash tool call,
confirm the Fornax segment appears in the status line and `fornax detail`
shows real findings after a session ends.

From an unrelated repo/path: open a separate Claude Code session, confirm
the status line looks exactly as it did before this setup existed, and no
`fornax-hook.sh`/`fornax-statusline.sh` process ever runs (project-local
settings never apply outside this repo — this is a Claude Code platform
guarantee, not something Fornax enforces itself).

## Failure containment

Both wrapper scripts fail safe: if `target/debug/fornax*` isn't built, or
the daemon isn't running, they degrade to a plain message rather than
erroring — a Fornax problem never breaks your ability to use Claude Code in
this project, let alone any other.
