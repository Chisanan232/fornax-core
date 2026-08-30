# Third-provider architecture-fitness report (FORNX-161)

Jira: FORNX-161, parent FORNX-138. This ticket exists to prove or falsify
the Stage-3 platform built in FORNX-155–160 by integrating a third,
architecturally distinct coding-agent runtime — opencode — and reporting
honestly on what stayed inside the intended extension seams versus what
required touching core logic.

**Bottom line: zero unexpected core coupling.** Every change needed to add
opencode lives in a new adapter crate, one new fixture directory, and one
new `Provider` enum variant inside the crate that already owns that
taxonomy. No file in `fornax-daemon`, `fornax-store`, or `fornax-verify` was
touched. The one real friction found was upstream of Fornax entirely (a
local-Ollama tool-calling limitation, not an architecture gap) — see
below.

## What was actually run, not simulated

- opencode CLI v1.18.25 installed locally via `npm install -g opencode-ai`
  (`opencode --version` → `1.18.25`).
- Ollama v0.32.11 was already running locally on this machine with several
  models already pulled (`qwen2.5-coder:7b`/`:14b`, `mistral-nemo:latest`,
  `llama3:8b`, `phi3:mini`, `starcoder2:7b`, `deepseek-coder:latest`) —
  **no new model was pulled**, so no additional disk was used for this
  ticket (521 GiB free on `/System/Volumes/Data` at the time, confirmed
  before starting per `~/CLAUDE.md`'s disk-pressure incident log).
- A capture plugin (`.opencode/plugin/capture.js` during investigation,
  formalized as the shipped `crates/fornax-adapter-opencode/plugin/fornax-capture.js`)
  was installed into a real opencode project and logged every hook
  invocation from `opencode run` sessions verbatim to a local JSONL file.
- Real sessions were run against local Ollama models over its
  OpenAI-compatible endpoint (`http://localhost:11434/v1`), producing real
  `session.created`/`session.idle`/`chat.message` events driven by genuine
  local LLM inference (`mistral-nemo:latest`).
- A real `tool.execute.before`/`tool.execute.after` pair, including a real
  `ls -la .` process opencode spawned and its real stdout/exit code, was
  captured — see "The tool-calling limitation" below for exactly how the
  driving LLM turn was produced and why that doesn't make the captured
  event any less genuine.

## The tool-calling limitation (disclosed, not papered over)

The ticket's install step directed toward local Ollama as opencode's
zero-cost path. That path is real for opencode's *integration mechanism*
(the in-process plugin API genuinely works, confirmed below) but hit a
real, reproducible limitation one layer down:

Every locally available Ollama tool-calling model
(`qwen2.5-coder:7b`, `qwen2.5-coder:14b`, `mistral-nemo:latest`) reliably
degraded a real `tool_calls` response into plain-text JSON once the request
carried opencode's actual production system prompt (~20k tokens, ~10 tool
definitions). This was reproduced directly against Ollama's raw HTTP API
(`/v1/chat/completions` and `/api/chat`, both streaming and non-streaming),
independent of opencode entirely — the same models reliably emit
well-formed `tool_calls` against a minimal (~160-token, 1-tool) prompt, so
this is a genuine model/Ollama-version behavior under opencode's real
prompt size, not an opencode integration bug.

To still capture a **genuine** opencode-produced `tool.execute.before`/
`tool.execute.after` event pair — rather than fabricate one from the type
definitions — a deterministic HTTP stub stood in for only the LLM's one
turn (it always returns a fixed `bash` tool call). Everything downstream of
that stub — opencode's own tool-invocation plumbing, the real `ls -la .`
process it spawned, that process's real stdout and real exit code, and
every field in the captured hook payloads — is opencode's own genuine code
running for real, not hand-written JSON. This is disclosed explicitly in
`fixtures/opencode/tool_execute_before_after_pair.json`'s description field
and in `docs/research/adapter-capability-matrix.md`. Session-lifecycle and
chat-message fixtures were captured from fully organic local-Ollama
inference (`mistral-nemo:latest`), with no stub involved.

## Architecture-fitness findings

### 1. The `Provider` enum experiment (run before any adapter code was written)

Added `Provider::OpenCode` to `fornax_types::Provider` in isolation and ran
`cargo check --workspace` and `cargo clippy --workspace --all-targets -- -D
warnings` before writing a line of adapter logic. **Zero errors, zero
warnings.** Grepping every `Provider::` reference across `fornax-daemon`,
`fornax-store`, `fornax-verify`, `fornax-cli`, and `fornax-types` itself
confirms why: every reference is a literal constructor
(`Provider::ClaudeCode`/`Provider::Codex`) in production or test code, never
an exhaustive `match` over the enum. Core logic genuinely does not branch on
provider identity anywhere. This directly falsifies the concern
`docs/contributing/adding-an-adapter.md` previously carried ("check first
whether anything downstream assumes exactly two variants") — nothing does,
inside this repo. That doc has been corrected to say so explicitly, with
one caveat below.

### 2. `CollectionMethod`: reused `HookCallback`, did not add a new variant

The ticket anticipated opencode's collection method would "likely" need a
new `CollectionMethod` variant, since it's neither a hook-script nor
file-tailing. Reading `TamperBoundary::for_trust_class`'s actual match arms
confirmed `Unrecognized(_)` degrades every tamper-boundary description to a
generic "collection method not recognized" sentence, discarding the
specific one a named variant produces — so `Unrecognized("in_process_plugin")`
was correctly ruled out as dishonest (it would throw away a boundary this
adapter genuinely knows). But the existing `HookCallback` variant's own doc
comment — "an in-process callback invoked synchronously by the provider
around an action" — turned out to be a **more literal fit for opencode's
real mechanism than for the Claude Code hook-script mechanism the variant
was originally named after** (Claude Code's hook is an external process
spawned per event, not literally in-process). `fornax-adapter-opencode`
reuses `CollectionMethod::HookCallback` as-is. This is the strongest single
fitness result in this report: the taxonomy already generalized to a third,
architecturally distinct provider with *zero* changes to `fornax-types`'
`sensor.rs`, contradicting the ticket's own working assumption that a new
variant would likely be needed.

### 3. `ExtensionEnvelope`: first real adapter usage

Neither `fornax-adapter-claude` nor `fornax-adapter-codex` populates
`Evidence::extension` — both construct it as `None` unconditionally.
`fornax-adapter-opencode`'s `OpenCodeExitCodeSensor` is the first real
producer: opencode's `tool.execute.after` payload carries a `title` and
precise `time.start`/`time.end` timestamps with no home in
`ExitCodePayload`'s canonical shape, so they're carried forward via
`ContentClass::ToolTelemetry` rather than dropped. This exercised the
extension-envelope contract (FORNX-158) against real data for the first
time, in an existing, unmodified extension point — no changes to
`extension.rs` were needed.

### 4. `SignalAvailability`'s three-state design earns its keep

`fornax-adapter-opencode::probe()` declares `ProcessResult: Available`
(genuine literal exit code — the first provider of the three that has one),
`SubagentLifecycle: Unsupported` (no such hook exists in the Hooks
interface at all — structural), and `FinalResponse: Unavailable` (the
signal genuinely exists in opencode's real event stream —
`message.updated`/text parts were observed live — but this adapter version
doesn't translate them, per FORNX-161's single-event-path scope). Getting
the last one right required resisting the temptation to mark it
`Unsupported` (which would be false — the mechanism exists) just because
this adapter doesn't consume it yet.

### 5. No local schema change needed

`crates/fornax-store`'s `provider` column is a plain TEXT field in every
migration (`0001_init.sql`, `0002_runtime_capabilities.sql`,
`0003_capability_signals.sql`, `0005_evidence_extension.sql`) — no `CHECK`
constraint enumerating providers anywhere. `LegacyCapabilitiesWire` is
generic over `provider: Provider`. A third provider's rows insert with zero
migration changes.

### 6. The one real, disclosed non-local constraint: fornax-cloud's closed enum

`fornax-daemon::default_unknown_caps`'s existing doc comment already
documented that `horonomy/fornax-cloud` (a separate, out-of-scope repo) has
a closed 2-variant `Provider` enum on its ingest boundary. That comment
literally said "2-variant" — now stale, since `Provider` has three variants
as of this ticket; the comment's premise about risk if `Provider::Unknown`
were ever exported is unaffected, but the "2-variant" framing is now wrong
and has been flagged in `docs/contributing/adding-an-adapter.md`. Concretely:
nothing in *this* repo needs fornax-cloud's enum touched — the local
daemon/store/CLI path is fully functional for opencode monitoring with zero
cloud involvement — but `fornax export-spool`'d opencode session data would
get a real 422 from fornax-cloud's ingest API today, until that repo (out of
scope here) adds a third variant. This is reported, not fixed — touching
`fornax-cloud` was explicitly out of scope for FORNX-161.

## Files/modules touched — expected seams vs. unexpected coupling

### Expected extension seams (all of it)

| Path | What |
|---|---|
| `crates/fornax-adapter-opencode/` (new crate) | `AgentAdapter`/`CapabilityProbe` impl, `OpenCodeExitCodeSensor` (`EvidenceSensor`), thin `main.rs` transport binary |
| `crates/fornax-adapter-opencode/plugin/fornax-capture.js` (new) | The in-process opencode plugin — the real, distinct integration mechanism this ticket tests |
| `crates/fornax-types/src/lib.rs` | One line: `Provider::OpenCode` enum variant |
| `crates/fornax-adapter-conformance/Cargo.toml`, `src/fixtures.rs`, `tests/conformance.rs`, `tests/golden_fixtures.rs`, `tests/contract.rs` | New `[dev-dependencies]` entry + opencode-mirroring test functions, following the exact pattern Claude/Codex already established |
| `crates/fornax-adapter-conformance/fixtures/opencode/*.json` (new) | Five real (four) / synthetic (one) sanitized golden fixtures |
| `Cargo.toml` (workspace root) | One line: new member path |
| `README.md`, `docs/contributing/adding-an-adapter.md`, `docs/research/adapter-capability-matrix.md` | Documentation — third-provider wiring instructions, doc-drift corrections found while actually following the existing doc |

### Unexpected core coupling

**None.** No file under `crates/fornax-daemon/src`, `crates/fornax-store/src`,
`crates/fornax-verify/src`, or `crates/fornax-cli/src` was modified. The
one core file referenced above (`fornax-daemon/src/main.rs`'s
`default_unknown_caps` doc comment) was not edited — it is *cited*, not
changed, and its own out-of-date "2-variant" claim is flagged in the
contributing doc rather than corrected in place (fixing daemon comments
unrelated to the adapter itself was judged out of this ticket's minimal-diff
scope).

## Doc drift found by actually following `adding-an-adapter.md`

1. **Step 2** ("add a new `Provider` variant first — check first whether
   anything downstream assumes exactly two variants") was correct to flag
   the risk but had never been empirically checked. Now it has been, with
   a clean result — see finding #1 above. Doc updated.
2. **Step 5**'s two `main.rs` templates (stateless-stdin-hook vs.
   long-lived-file-tail) do not cover a plugin-hosted, in-process-callback
   transport. A third pattern was needed and is now documented: a small
   companion script in the provider's plugin language spawns the adapter
   binary once as a long-lived child process and pipes NDJSON to its
   stdin for the life of the session. Doc updated with a worked-example
   pointer to this ticket's crate.

## Test results

- `cargo test -p fornax-adapter-opencode`: 12 passed.
- `cargo test -p fornax-adapter-conformance`: 39 passed (7 `tests/conformance.rs`
  + 20 `tests/contract.rs` + 8 `tests/golden_fixtures.rs` + 4
  `src/fixtures.rs` unit tests), including opencode-specific declaration-
  vs-reality checks (`opencode_declares_process_result_available_and_never_emits_a_heuristic_exit_code`,
  `opencode_declares_subagent_lifecycle_unsupported_and_never_emits_subagent_events`)
  and the first-ever real `ExtensionEnvelope` usage test
  (`opencode_tool_execute_evidence_carries_a_real_extension_envelope`).
- `cargo test --workspace`: all suites pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.

## Non-goals honored

No plugin marketplace/dynamic loading infrastructure was built. No attempt
was made at broad feature parity with Claude Code/Codex — `FinalResponse`,
`ReasoningSummary`, permission-hook translation, and subagent handling are
explicitly left untranslated/`Unsupported`/`Unavailable`, honestly declared
rather than silently stubbed as `Available`. `fornax-cloud`,
`fornax-infra`, `fornax-docs`, and `fornax-website` were not touched.
