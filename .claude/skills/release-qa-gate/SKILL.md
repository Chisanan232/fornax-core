---
name: release-qa-gate
description: Orchestrate risk-scaled QA verification of a frozen Fornax release candidate — bounded, dynamically-sized worker lanes, reused CI evidence, and a truthful PASS/BLOCK/INCONCLUSIVE/UNTESTED gate verdict. Triggers on `/release-qa-gate <version>`, "run the QA gate", "verify this release candidate", "is this candidate ready to ship", "QA sign-off for v<version>".
---

# release-qa-gate

Jira: FORNX-230. Coordinator-orchestrated, risk-scaled QA verification of one
**frozen** release candidate. Produces a durable, evidence-backed verdict
instead of an ad-hoc chat summary.

## What this skill does not own (read this first)

This skill is a **consumer**, not a definer, of the vocabulary and artifacts
below. It never redefines them, and a divergent local definition anywhere in
this skill's assets is a bug in this skill, not a valid override:

| Concern | Owner |
|---|---|
| Risk classes (`PATCH_LOW_RISK`/`FEATURE`/`TRUST_BOUNDARY`/`MAJOR_OR_GA`), assurance-depth-by-risk table, verdict semantics (`PASS`/`BLOCK`/`INCONCLUSIVE`/`UNTESTED`), blocker taxonomy, waiver policy, `CI_DEGRADED_EXTERNAL` | `docs/release-assurance-policy.md` (FORNX-229) |
| Candidate manifest schema, `risk_class` field, feature-delta schema, golden-journey catalog, QA coverage reconciliation schema, shared surface vocabulary | `docs/release-candidate-evidence.md` (FORNX-231) |
| Durable QA sign-off artifact format, finding lifecycle | FORNX-232 (not yet built — see "Worker evidence contract" below) |
| Security gate skill, threat model, trust-boundary delta record | FORNX-233 (not yet built) |
| Mechanical gate enforcement (`scripts/release-readiness.sh`) | FORNX-234 |
| Tag/build/publish/promotion mechanics | FORNX-235 |

This skill owns exactly: turning a candidate's risk class + feature-delta +
coverage-reconciliation into a **selected lane set**, a **bounded worker
count**, dispatch of those workers, and an honest aggregate verdict.

## When to use

- Invoked as `/release-qa-gate <version>` against a candidate manifest that
  is already frozen (`release/<version>-candidate-manifest.json` exists).
- Feature-delta discovery and QA coverage reconciliation for that version
  already exist or can be produced (FORNX-231 outputs).

## When not to use

- The candidate is not yet frozen (no manifest) — run Feature Delta/coverage
  reconciliation first.
- You need the *durable* QA sign-off record — that artifact format is
  FORNX-232's; this skill's output feeds it, but is not itself that record.
- You need to run the security gate — that is a separate skill (FORNX-233).
- A single narrow bug needs reproduction — use the normal dev-impl-loop, not
  this coordinator.

## Step 1 — Coordinator discovery (performed once, shared with every worker)

Read, once, and hand the same digest to every worker rather than letting each
worker re-derive it:

1. `release/<version>-candidate-manifest.json` — repos/SHAs and `risk_class`.
2. The feature-delta list for this version (FORNX-231 schema,
   `FD-<version>-NNNN` items with `surfaces[]`).
3. The QA coverage reconciliation result for this version (FORNX-231 schema).
4. Existing hosted-CI status for the exact candidate SHA(s) (or a local
   CI-equivalent verification manifest per `CI_DEGRADED_EXTERNAL`, FORNX-229).
5. The golden-journey catalog (`release/golden-journeys.json`), filtered to
   entries whose `surfaces` intersect the feature-delta's `surfaces`.

**Fail closed on `risk_class`.** If the manifest's `risk_class` is absent,
this is not an implicit `PATCH_LOW_RISK` — per FORNX-231, classification has
not happened yet. Do not guess a class or invent a lane set. Report
`UNTESTED` for the whole gate with the reason `risk_class not set`, and stop
before any lane selection or worker dispatch. This is a real, first-class
outcome of this skill, not a failure to run it correctly.

## Step 2 — Lane selection (risk depth + lanes visible before execution)

Map the feature-delta items' `surfaces` to verification lanes using
[`lane-surface-mapping.md`](lane-surface-mapping.md)'s table, then scale
lane depth by `risk_class` per FORNX-229's assurance-depth-by-risk table
(touched-P0-only at `PATCH_LOW_RISK`, up to full P0/P1/P2 sweep across all
surfaces at `MAJOR_OR_GA`). **Print the selected lane set and the risk class
before dispatching any worker** — this is the AC's "risk depth and selected
lanes are visible before execution," and it is also the point at which a
human reviewing the run can sanity-check the scope before cost is spent.

A lane with zero mapped feature-delta surfaces at this risk depth is not
selected — this skill runs the lanes the actual diff touches, not every lane
that theoretically exists (see "Anti-pattern: the ceiling as a target"
below).

## Step 3 — Worker sizing (bounded by actual independent work)

One worker per **selected** lane, never more workers than selected lanes,
never fewer than the lanes actually need to stay non-overlapping:

| Selected lane count | Worker band |
|---|---|
| 1–2 lanes (narrow change) | 2–4 workers |
| 3–4 lanes (several independent surfaces) | 4–6 workers |
| 5+ lanes, or any lane at `TRUST_BOUNDARY`+ depth (broad/high-risk) | 6–8 workers |

**Anti-pattern: the ceiling as a target.** The upper bound of a band is not
a goal to reach — a genuinely 2-lane `FEATURE` change gets 2–4 workers, full
stop, even though the table's top band goes to 8. Do not pad worker count to
look thorough.

No nested sub-agent explosion: a worker verifies its own lane directly. A
worker that needs to fan out further is a sign the lane was drawn too
broadly — split the lane at Step 2, not by spawning sub-workers under it.

## Step 4 — Reuse existing CI evidence before re-running anything

Before a worker re-runs a suite, check whether hosted CI already ran that
exact suite against the exact candidate SHA and is currently green
(`scripts/release-readiness.sh`'s `sha_on_main`/evidence checks, or the
repo's own CI status API for that SHA). If so, the worker **cites** that
run (SHA, workflow run URL/ID, result) instead of re-executing it.
Independent reproduction is still required, per FORNX-229's depth table,
for any suspected High/Critical finding at `TRUST_BOUNDARY`+ risk, and for
any lane whose existing CI evidence is itself `CI_DEGRADED_EXTERNAL` — in
that case follow FORNX-229's local-CI-equivalent-manifest policy rather than
treating the outage itself as a pass or a block.

## Step 5 — Dispatch workers with a compact evidence contract

See [`worker-evidence-contract.md`](worker-evidence-contract.md) for the
exact shape. Workers return **compact evidence**: verdict, evidence
pointers (paths/SHAs/run IDs/log excerpts), and blocked/untested surfaces —
never chain-of-thought, and never a raw log dump. A worker that cannot
produce a confident `PASS`/`BLOCK` reports `INCONCLUSIVE`, not a rounded-up
guess.

## Step 6 — Aggregate verdict

The gate's overall verdict is the **worst** of its constituent lane
verdicts, per FORNX-229's verdict semantics: one `BLOCK` or undispositioned
`UNTESTED` lane makes the whole gate non-`PASS`, regardless of how many
other lanes passed. Never silently convert a blocked or untested surface
into `PASS`. The final report lists, explicitly:

- Overall verdict (`PASS`/`BLOCK`/`INCONCLUSIVE`/`UNTESTED`).
- Per-lane verdict and evidence pointer.
- The full untested/blocked surface list — never omitted even when the
  overall verdict is otherwise `PASS`-leaning, because a `PASS` overall
  verdict is only reachable when this list is empty or every entry on it
  is individually dispositioned per FORNX-229's waiver policy.

## Step 7 — Autonomous defect loop

When a worker reports `BLOCK` for a reproducible defect, this skill does
**not** reimplement a second test framework or a bespoke fix workflow. It
hands off to the repo's already-established path:

1. Independently reproduce the finding (not solely coordinator-asserted) —
   required by FORNX-229's depth table at `TRUST_BOUNDARY`+ for High/Critical.
2. File or link a Jira Bug carrying the reproduction.
3. Fix through the normal ticket/PR workflow (`dev-impl-loop` — global
   `CLAUDE.md` Skill Invocation Guide).
4. Independent review, CI, merge — normal PR policy.
5. Resync the candidate manifest to the new SHA (this is a manifest edit,
   not a new manifest — see `docs/release/v0.0.1-qa-security-signoff.md`'s
   own "Candidate SHA history" precedent for how a manifest SHA moves
   during a live QA pass).
6. Rerun **only the affected lane(s)**, not the whole gate — Step 4's CI-
   reuse rule still applies to every unaffected lane.

## Anti-patterns

- Treating the worker-band ceiling as a target (Step 3).
- Re-running a lane's suite when exact-candidate CI is already green for it
  (Step 4) — except where independent reproduction is itself required.
- Converting `UNTESTED` or `BLOCK` into `PASS` because "most things passed."
- Spawning sub-agents under a worker instead of splitting the lane.
- Guessing `risk_class` when the manifest does not carry it.

## Recorded dogfood run

A real discovery-only run of this skill against the `v0.0.1` candidate is
recorded at
[`docs/release/v0.0.1-qa-gate-dogfood-run.md`](../../../docs/release/v0.0.1-qa-gate-dogfood-run.md).
