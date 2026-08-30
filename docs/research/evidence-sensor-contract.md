# Evidence sensor/source contract (FORNX-157)

Status: living doc, describes a shipped contract (`fornax_types::sensor`).
See `crates/fornax-types/src/sensor.rs` for the authoritative, compiled
definitions and doc comments — this file is the map of *why* it looks the
way it does and *what currently implements it*, not a restatement of the
Rust doc comments.

## Why this exists

Before FORNX-157, evidence collection was ad-hoc code inline in each
adapter's `translate()`/`translate_line()` — e.g.
`fornax-adapter-claude`'s Bash exit-code heuristic. That code already did
the right thing (checked capabilities, stamped a free-text `provenance`
string), but there was no shared shape a new collector — a Git sensor, a
CI-webhook sensor, a future reasoning/logprob sensor — could implement
uniformly, and no structured "how much do we trust this" metadata separate
from the free-text provenance breadcrumb.

## The two new types

- **`EvidenceSensor`** (trait): `name()`, `required_capabilities()`
  (`&'static [SignalClass]`, reusing FORNX-155's taxonomy),
  `trust_class()`, and `collect(event: &AgentEvent, caps: &RuntimeCapabilities)
  -> SensorOutcome`. One method, no registry — mirrors `CapabilityProbe`'s
  shape (FORNX-155) deliberately: capabilities are fixed at implementation
  time, not discovered at runtime.
- **`EvidenceSource`** (struct, attached to `Evidence::source: Option<..>`):
  `sensor_name`, `trust_class`, `collected_at`, `provider`. First-class
  metadata, not a string folded into the existing `Evidence::provenance`
  field — the two are complementary: `provenance` stays the granular
  "which branch fired" breadcrumb, `source` is the structured
  identity/trust record.

`SensorOutcome` is a struct (`evidence: Vec<Evidence>`, `state:
SignalAvailability`, `detail: Option<String>`), not an enum, specifically so
a sensor can report *partial* availability (some evidence collected, then a
capability gap or failure) without lying in either direction. A
sensor-internal timeout is reported as `state: SignalAvailability::CollectionFailed`
with a `detail` naming the timeout — there is no separate error type; this
reuses FORNX-155's existing taxonomy rather than inventing a parallel one.

## Trust classes

Named exactly per FORNX-138/FORNX-157:

| Class | Meaning | Current users |
|---|---|---|
| `AgentAdjacent` | The provider's own account of what happened (not independently measured) | `ClaudeBashExitCodeSensor`, `CodexExecCommandEndSensor`, `CodexCustomToolCallOutputSensor` |
| `HostObserved` | Fornax measured it itself (e.g. an exit code Fornax's own process captured, a `git` invocation Fornax ran) | none yet — no sensor collects independently of the provider today |
| `IndependentExternal` | A system outside both agent and host (CI webhook, third-party API) | none yet |
| `HumanReviewed` | Confirmed/entered by a person | none yet |
| `ModelInternal` | The model's own internals (reasoning summary, logprobs) | `ReasoningSummarySensor` (worked example only, see below) |

## Migrated paths (no behavior loss)

Three existing, real evidence-collection code paths were migrated onto
`EvidenceSensor`. In every case the heuristic logic itself is unchanged —
only its shape (a named `EvidenceSensor` impl instead of inline code) and
the evidence it emits (now additionally carrying `source`) changed. Proof:
every pre-existing test's assertions (exact `provenance` strings, exact
payload shapes, exact message counts) were left untouched and still pass —
see the crates' test suites.

| Sensor | Crate | Migrated from |
|---|---|---|
| `ClaudeBashExitCodeSensor` | `fornax-adapter-claude` | The inline Bash `tool_response` exit-code heuristic in `translate()` |
| `CodexExecCommandEndSensor` | `fornax-adapter-codex` | The inline `exec_command_end.exit_code` literal-field extraction in `translate_line()` |
| `CodexCustomToolCallOutputSensor` | `fornax-adapter-codex` | The inline `custom_tool_call_output` "Script completed" heuristic in `translate_line()` |

No Git or CI-signal evidence collection exists in this codebase today — a
`GitSensor`/`CiSensor` was deliberately not built (FORNX-157 non-goal:
don't speculatively design sensors for signals that don't exist yet).

## Worked example: a future `ReasoningSummarySensor`

`crates/fornax-types/src/sensor.rs`'s test module implements a compiled,
tested `ReasoningSummarySensor`: declares `SignalClass::ReasoningSummary`,
reports `TrustClass::ModelInternal`, and — because no provider integration
exposes that signal today — always reports
`SignalAvailability::Unsupported`. Adding it required zero changes to
`EvidenceSensor`, `EvidenceSource`, `Evidence`, or `Verifier`; it is purely
an additional implementor of the already-defined trait, exactly as a real
future sensor (reasoning summaries, logprobs, or other model-internal
telemetry) would attach.

## Persistence

`fornax-store` migration `0004_evidence_source.sql` adds a single nullable
`source` column to the existing `evidence` table (additive, matching
0002/0003's precedent). A `NULL` value round-trips as `Evidence::source ==
None` — an honest "no structured provenance recorded" for legacy rows or
evidence produced by code not yet migrated onto the sensor contract, not a
fabricated value. There is no legacy bool set to reconstruct from (unlike
`RuntimeCapabilities`'s FORNX-155 migration), so the reconstruction rule is
simply "absent column means unknown."

## What `Verifier` consumes

`fornax-verify`'s `Verifier` trait already took `evidence: &[Evidence]` —
the canonical domain type, never raw provider transport — before this
ticket. FORNX-157 did not change `Verifier`'s signature or algorithm; it
only added `source` as additional metadata on the `Evidence` values a
verifier already receives. A verifier may read `Evidence::source.trust_class`
in the future, but none does today — evidence weighting/fusion by trust
class is an explicit non-goal of this ticket.

## Explicit non-goals

- No dynamic plugin loading / sensor registry / discovery mechanism. A
  sensor is a concrete Rust type an adapter constructs and calls directly,
  same as before this ticket.
- No evidence weighting or fusion by trust class.
