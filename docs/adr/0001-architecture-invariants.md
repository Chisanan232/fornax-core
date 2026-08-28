# ADR 0001: Fornax v0.0.1 architecture invariants and OSS boundary

Status: Accepted
Date: 2026-08-28
Jira: FORNX-22 (child of FORNX-20)

## Context

Fornax is an evidence-first agent-integrity system: given a coding agent's claim
about what it did, decide `VERIFIED` / `UNVERIFIED` / `CONTRADICTED` / `REVIEW` /
`UNAVAILABLE` from observed evidence, not from the agent's own narration. The MVP
epic (FORNX-20, product thesis HVDL-15) requires a local-first vertical slice
before any cloud/SaaS work is considered product-ready (Gate 2, FORNX-34).

This ADR records the invariants so they survive session boundaries and are not
re-derived or silently drifted on each ticket.

## Decisions

### D1 — Modular monolith, not microservices

Exactly four deployables in v0.0.1:

1. `fornax-daemon` — one local Rust process (adapters, storage, verifiers,
   status/detail/dashboard APIs all in-process).
2. Rust cloud ingest (stateless Cloud Run service, FORNX-39).
3. Python FastAPI modular monolith (FORNX-40).
4. React SaaS frontend (FORNX-42).

A module boundary (adapter vs. verifier vs. store) is a Rust module/crate
boundary, never a process or network boundary, inside the daemon. No internal
HTTP service for Claim/Evidence/Finding.

### D2 — Local critical path has no cloud dependency

Evidence capture → normalization → verification → status line / detail command /
localhost dashboard must work with all cloud network access disabled. Cloud sync
is strictly async and best-effort, after local policy approves it (FORNX-33,
FORNX-41).

### D3 — Immutable observation before interpretation

Raw adapter events are persisted (SQLite/WAL, append-only) before any claim
extraction or verification runs against them. Verifier logic is
`Claim + Evidence[] + RuntimeCapabilities -> Finding`, replayable without live
Claude Code/Codex/network access (needed for FORNX-49 ablation benchmark).

### D4 — Five-state finding vocabulary, never collapsed

`VERIFIED`, `UNVERIFIED`, `CONTRADICTED`, `REVIEW`, `UNAVAILABLE`. Missing
provider capability is `UNAVAILABLE`, never inferred or silently treated as
`VERIFIED`/pass. See FORNX-24 for the canonical type definitions and the
capability-matrix rationale (Claude Code vs. Codex hook/event surfaces differ
materially — recorded separately once both adapters' real payload shapes are
confirmed).

### D5 — Adapters are thin

`fornax-adapter-claude` and `fornax-adapter-codex` translate provider-native
events into canonical `AgentEvent`s and nothing else. No verification logic, no
duplicated domain model, per adapter.

### D6 — No infrastructure without measured need

No Kafka/Redpanda, ClickHouse, Kubernetes/GKE, service mesh, multi-region HA,
Redis, SSO/SCIM, or generic policy platform in v0.0.1. IPC to the daemon is a
Unix Domain Socket (macOS/Linux) — no HTTP hop for adapter → daemon events.

### D7 — Privacy invariant

Raw prompt content, source code, file contents, sensitive paths, shell/tool
arguments, secrets, and private API responses default to local-only. Cloud sync
carries only a policy-approved redacted envelope. Local integrity evaluation
never requires cloud availability. Tested explicitly with cloud config disabled
(FORNX-34 acceptance).

### D8 — Open by default, private by necessity

Public/OSS: local Rust runtime, adapters, event/evidence protocol, claim/
evidence/finding models, basic verifiers, local privacy/redaction behavior,
CLI/local UX, docs/site code with no sensitive operational material.

Private: SaaS/cloud implementation, production infra/topology, credentials,
customer data, billing/ops, internal runbooks, proprietary calibration data.

### D9 — Repository/org bootstrap sequencing

Dedicated Fornax GitHub Organization creation requires a human browser session
(no API for org creation under a personal account) — tracked as an open manual
action on FORNX-21, not a blocker for engineering work. Repos are created under
the personal account (`Chisanan232/fornax-core`, ...) now and transferred to the
org later (GitHub repo transfer preserves history/issues/PRs).

## Consequences

- Verifier and store code must not assume network/adapter liveness — enables
  replay testing and the ablation benchmark harness.
- Every new cross-cutting capability proposal is checked against D6 before
  adoption; default answer is no.
- FORNX-24's canonical types are derived empirically from actual Claude Code
  hook payloads and actual Codex CLI capability surface, not assumed symmetric
  — see `docs/research/adapter-capability-matrix.md`.
