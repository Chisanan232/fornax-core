# FORNX-238 gap closure: live dashboard script-injection check

**Date:** 2026-08-30
**Commit tested:** `1c078ed31c23ffac8f515e8a46c97c1888c76457` (v0.0.1 candidate, `horonomy/fornax-core`)
**Gap closed:** FORNX-238 sign-off explicitly marked "live Playwright
script-injection render check" as NOT RUN. This document is that check,
actually run with Playwright against the real local daemon.

## Scope

Targeted XSS injection test against the daemon's `/dashboard` HTML surface
(`crates/fornax-daemon/src/main.rs`, `dashboard()` handler). Not a general
visual QA pass — only the fields that surface provider-controlled text in
that handler: `claim_text`, `verifier_name`, `rationale`, `verdict`,
`computed_at` (all four rendered via `html_escape`, which replaces `&`,
`<`, `>`).

## Setup

```bash
git worktree add ~/Bryant-Developments/fornax-v001-xss-check 1c078ed31c23ffac8f515e8a46c97c1888c76457
cd ~/Bryant-Developments/fornax-v001-xss-check
CARGO_TARGET_DIR=./target cargo build --workspace
export FORNAX_HOME=/tmp/fornax-xss-home   # short path — long scratch paths overflow SUN_LEN on the UDS socket
./target/debug/fornax-daemon &
```

Events were fed through `./target/debug/fornax-hook-claude` exactly as the
README Quick Start describes: a `PostToolUse` Bash event with `exit_code:
1`, followed by a `Stop` event whose transcript JSONL carries the assistant
claim text. The `TestResultVerifier` only fires on claims matching
`claims_tests_passed()` (must mention "test(s)" + "pass(ed)"/"succeeded"
and not "failed"), so every payload below was embedded inside a claim of
the form `"All tests passed. <payload>"` to guarantee it reached the
`claim_text` field rendered on the dashboard.

## Payloads tried

| # | Payload | Injected into | Observed `window.__FORNAX_XSS_EXECUTED` | Observed DOM |
|---|---|---|---|---|
| 1 | `<script>window.__FORNAX_XSS_EXECUTED = true</script>` | claim text (transcript `"text"` field) | `undefined` | Rendered as literal text `&lt;script&gt;window.__FORNAX_XSS_EXECUTED = true&lt;/script&gt;`; `document.querySelectorAll('script').length === 0` |
| 2 | `<img src=x onerror="window.__FORNAX_XSS_EXECUTED = true">` | claim text | `undefined` | Rendered as literal text `&lt;img src=x onerror="window.__FORNAX_XSS_EXECUTED = true"&gt;`; zero `<img>` elements in DOM |
| 3 | `"><script>window.__FORNAX_XSS_EXECUTED = true</script>` | claim text | `undefined` | Rendered as literal text `"&gt;&lt;script&gt;window.__FORNAX_XSS_EXECUTED = true&lt;/script&gt;`; zero `<script>` elements. Quote itself is not HTML-escaped by `html_escape`, but the field is only ever placed in a text node (`<td>{claim}</td>`), never inside an attribute, so the unescaped `"` has no HTML meaning here |
| 4 | `javascript:window.__FORNAX_XSS_EXECUTED = true` | claim text | `undefined` | Rendered as literal text; `document.querySelectorAll('a[href]')` filtered to `javascript:` scheme returns 0 — no `href` is ever constructed from this field |

All four also went through the same `PostToolUse` → `Stop` pipeline
unmodified (tool_input command text was left as plain `cargo test
--workspace` since `tool_input`/`tool_response` are not rendered anywhere
in the current `dashboard()` handler — confirmed by reading
`crates/fornax-daemon/src/main.rs`, `crates/fornax-store/src/lib.rs` for
what `recent_findings` selects, and `crates/fornax-types/src/lib.rs`).

## Verification method

Real Playwright browser (`mcp__plugin_playwright_playwright__*`), not a
simulated check:

1. `browser_navigate` to `http://localhost:4317/dashboard`.
2. `browser_evaluate`: read `window.__FORNAX_XSS_EXECUTED`, count
   `document.querySelectorAll('script')`, count `<img>` elements, count
   `a[href^="javascript:"]`, and dump `document.body.innerHTML`.
3. `browser_console_messages`: only a `favicon.ico` 404, unrelated.
4. `browser_take_screenshot` (full page) — saved as
   `fornax-xss-dashboard.png` in the worktree's `.playwright-mcp/` output
   dir (not committed; ephemeral QA artifact).

Result: `window.__FORNAX_XSS_EXECUTED` was `undefined` in every check;
`document.querySelectorAll('script').length === 0`; zero `<img>` tags; zero
`javascript:` hrefs. All four payloads appear in the DOM only as inert,
HTML-entity-escaped text inside `<td>` elements.

## Post-injection functional check

After all four adversarial events, one normal event was sent (`exit_code:
0`, claim `"All tests passed."`, no payload). The dashboard correctly
rendered a new row with `class="verified"` and rationale `observed
test-runner exit_code=0 (claude_code:PostToolUse:Bash#tool_response)`,
confirming the adversarial input caused no persistent corruption of
rendering or verification behavior — Playwright re-navigate confirmed
`verified` row present and `window.__FORNAX_XSS_EXECUTED` still
`undefined`.

## Finding

**No XSS.** `dashboard()`'s `html_escape` (escaping `&`, `<`, `>`) is
sufficient for the context it is used in: every escaped field is placed
only inside HTML text-node content (`<td>...</td>`), never inside an HTML
attribute or a `<script>` block, so the unescaped `"`/`'` characters carry
no HTML/JS significance there. No `href`/`src` attribute is ever built
from provider-controlled text on this page. This closes the FORNX-238 gap
item ("live Playwright script-injection render check") as **passed**.

## Caveat / residual risk (informational, not a finding)

`html_escape` does not escape `"` or `'`. This is safe *only* as long as
no future change places one of these fields inside an HTML attribute
value (e.g. a `title="{claim}"` or a data attribute) without a
context-appropriate escaper. If that ever happens, the same payloads used
here (in particular payload 3, `"><script>...`) would become exploitable.
Worth a one-line comment on `html_escape` or a switch to a proper HTML
templating/escaping crate if the dashboard grows attribute-based
rendering.

## Cleanup

```bash
kill %1   # fornax-daemon
git worktree remove ~/Bryant-Developments/fornax-v001-xss-check --force
git worktree prune
```
