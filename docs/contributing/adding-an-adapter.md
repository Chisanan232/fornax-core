# Adding a new provider adapter

See `docs/adr/0004-adapter-contract.md` for the full rationale. This is the
practical checklist and worked example.

## 1. Create the crate

```
crates/fornax-adapter-<provider>/
  Cargo.toml
  src/
    lib.rs   # AgentAdapter + CapabilityProbe impl — all translation logic
    main.rs  # transport plumbing only: read native input, call normalize(), write to the UDS
```

Add both a `[lib]` and a `[[bin]]` target to `Cargo.toml` (see
`crates/fornax-adapter-claude/Cargo.toml` for the exact shape), and add the
crate path to the workspace `members` list in the repo-root `Cargo.toml`.
`fornax-types` is the only Fornax crate this crate may depend on — see the
ADR's "Allowed core dependencies".

## 2. Implement `CapabilityProbe`

Declare, conservatively, what this adapter's runtime can actually observe —
never infer a class as `Available` you have not confirmed. Every
`SignalClass` your adapter cannot expose should be `Unsupported` (runtime
fundamentally can't) or `Unavailable` (exists in principle, not observed
this session), each with a `detail` string citing your evidence (a real
captured payload, the provider's own docs, a version number).

```rust
impl CapabilityProbe for WidgetAdapter {
    fn probe(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            schema_version: fornax_types::CAPABILITY_SCHEMA_VERSION,
            provider: Provider::Widget, // add a new Provider variant first
            signals: vec![
                CapabilitySignal {
                    class: SignalClass::ToolTrace,
                    state: SignalAvailability::Available,
                    detail: None,
                },
                // ... one CapabilitySignal per SignalClass your transport
                // can say something concrete about.
            ],
            notes: HashMap::new(), // session_id/adapter_version stamped separately, see step 4
        }
    }
}
```

## 3. Implement `AgentAdapter::normalize`

One `match` (or equivalent) over your provider's native discriminator field,
producing a `NormalizationOutcome` per case:

```rust
impl AgentAdapter for WidgetAdapter {
    fn provider(&self) -> Provider { Provider::Widget }
    fn adapter_version(&self) -> &'static str { env!("CARGO_PKG_VERSION") }

    fn normalize(&mut self, session_hint: &str, native: &serde_json::Value) -> NormalizationOutcome {
        let Some(kind) = native.get("event_type").and_then(|v| v.as_str()) else {
            return NormalizationOutcome::Unrecognized {
                discriminator: "<missing event_type>".to_string(),
            };
        };
        match kind {
            "tool_call_finished" => {
                // ... build AgentEvent/Evidence from confirmed fields only ...
                NormalizationOutcome::Messages(vec![/* ... */])
            }
            "heartbeat" => NormalizationOutcome::Ignored {
                reason: "heartbeat: transport keepalive, no canonical mapping",
            },
            other => NormalizationOutcome::Unrecognized {
                discriminator: other.to_string(),
            },
        }
    }
}
```

Rules, from the contract:

- **Never** put a raw, un-vetted provider payload into a canonical message's
  free-form fields as a way of "not losing information" for a shape you
  don't recognize. That is `Unrecognized`'s job, and it carries only a type
  tag — see the ADR's "Unknown-event policy".
- **Never** return an `Err`/panic from `normalize`. Malformed or unexpected
  input is expected input.
- If your transport needs to correlate two native payloads across calls
  (a start/end pair, request/response by id), hold that state as a field on
  your adapter struct — `normalize` takes `&mut self` for exactly this.

## 4. Stamp adapter/version metadata

Attach `adapter_version` and (once known) `session_id` to every capability
declaration via `notes`, the same way both existing adapters do:

```rust
fn stamped_capabilities(adapter: &WidgetAdapter, session_id: &str) -> RuntimeCapabilities {
    let mut caps = adapter.probe();
    caps.notes.insert("session_id".to_string(), session_id.to_string());
    caps.notes.insert("adapter_version".to_string(), adapter.adapter_version().to_string());
    caps
}
```

Include your adapter's identity in any `Evidence::provenance` string you
construct too (e.g. `"widget:<adapter_version>:tool_call_finished"`), so a
finding's rationale can always be traced back to which adapter version
produced the evidence behind it.

## 5. Write the thin `main.rs`

`main.rs` owns reading native input (stdin, a tailed file, a socket — however
your provider actually exposes events) and writing `IngestMessage`s to the
daemon's Unix Domain Socket. It calls `normalize()` and switches on the
`NormalizationOutcome` to decide what to send; it contains no field-name
literals from the provider's native shape. Model it on
`crates/fornax-adapter-claude/src/main.rs` (stateless, one-shot) or
`crates/fornax-adapter-codex/src/main.rs` (long-lived, tailing) depending on
whether your provider's integration point is invocation-based or
transcript-based.

## 6. Wire into the conformance suite

Add your crate as a `[dev-dependencies]` entry in
`crates/fornax-adapter-conformance/Cargo.toml`, then add a fixture function
and two tests to `crates/fornax-adapter-conformance/tests/conformance.rs`
mirroring the existing `claude_*`/`codex_*` tests:

- `<provider>_adapter_satisfies_the_conformance_contract` — runs the generic
  property checks (`probe_provider_matches_declared_provider`,
  `provider_is_stamped_consistently`,
  `every_message_round_trips_through_the_wire_protocol`) against a handful
  of real native fixtures.
- `<provider>_adapter_handles_an_unrecognized_native_event_per_policy` — a
  synthetic, never-seen shape must come back `Unrecognized` with a
  non-empty `discriminator`, never a panic.

If your adapter needs a new `Provider` variant, add it to
`fornax_types::Provider` (`crates/fornax-types/src/lib.rs`) — check first
whether anything downstream (`fornax-cli export-spool`,
`LegacyCapabilitiesWire`) assumes exactly two variants, since that
assumption is documented but not compiler-enforced (see
`fornax-daemon::default_unknown_caps`'s doc comment for one place this
already matters).

## 7. Verify

```
cargo test -p fornax-adapter-<provider>
cargo test -p fornax-adapter-conformance
cargo clippy --workspace --all-targets -- -D warnings
```
