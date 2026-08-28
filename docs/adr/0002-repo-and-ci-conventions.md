# ADR 0002: Repository, CI, and environment-isolation conventions

Status: Accepted
Date: 2026-08-28
Jira: FORNX-22/FORNX-23, informed by survey of `~/Bryant-Developments/horonomy/*`

## Context

The epic requires Fornax's Neon/GCP/CI conventions to match "existing Horonom
product conventions." A survey of `octans`/`ophiuchus`/`circinus`/
`official-website` found two divergent real precedents; this ADR picks one
per concern and records why, so it isn't re-derived per ticket.

## Decisions

### CI

Per-build-unit, path-filtered GitHub Actions workflows (not one monolith):
`ci.yml` (fmt/clippy/test, matches `circinus`'s pattern), later a separate
`infra.yml` (terraform fmt/validate only — no apply credentials in CI) and a
`workflow_dispatch`-only `relay-deploy.yml` once Beta infra exists (FORNX-35+).
Third-party actions pinned by commit SHA. `permissions: contents: read`
default. No push-to-deploy for infra — deploy workflows are manual dispatch
only, per `ophiuchus`'s ADR-0010 amendment (this was a deliberate correction
there after an unattended-apply incident risk was identified).

### Docs — distributed authoring, no central aggregator (correcting the epic's phrasing)

The epic says "distributed authoring + centralized publishing," but the actual
Horonom precedent (`circinus` ADR-0007, `ophiuchus`) is **one Docusaurus site
per product** at `<product>.horo.run` with docs at `/docs` on that same site —
never a separate `docs.*` subdomain, and no repo aggregates another repo's
docs content. `official-website` only links out via a hand-maintained
registry; it doesn't pull in docs. Fornax follows this: `fornax.horo.run`
serves both the marketing surface and `/docs`, until a real constraint forces
a split. This is stricter than FORNX-45's ticket title implies and should be
reconciled there rather than re-litigated per PR.

### Terraform / environment isolation

Folder-per-environment (`infra/envs/beta/`, `infra/envs/prod/`), separate GCP
projects, separate GCS state buckets, `workflow_dispatch`-only deploys via
GitHub Environments, images digest-pinned, auth via Workload Identity
Federation (no service-account JSON keys) — matches both `ophiuchus` and
`circinus`. No decision needed; both repos agree here.

### Neon Postgres

The epic is explicit: **one Neon project, environment isolation by branches**
(matches `ophiuchus`'s pattern, not `circinus`'s later two-project reversal).
Followed as specified — `circinus`'s two-project reasoning (a leaked
project-scoped API key can reach every branch) is noted for awareness but not
adopted; if Fornax's Beta credential surface later proves as sensitive as
`circinus`'s billing/webhook surface, revisit via a new ADR, not silently.

### Repo layout

`src`-equivalent is `crates/` (Rust workspace, not `src/` — Rust convention
takes precedence over the Python-repo precedent here). `docs/adr/NNNN-kebab-title.md`
(MADR-lite, matches all four precedent repos). `.claude/CLAUDE.md` +
`AGENTS.md` (pointer only) + `README.md` — same four-file precedence chain
used across Horonom repos.

### Commits / branches

Gitmoji + scoped imperative subject (`✨ (scope): Description`), matching
observed `circinus` git log and the global CLAUDE.md commit policy. Branch
naming `v0.0.1/FORNX-<n>/<type>/<snake_case_slug>`, matching the Horonom
family's `v<version>/<TICKET>/<type>/<slug>` pattern. PR-only to `main`.

## Consequences

- FORNX-45 (Docs) acceptance criteria should be read as "one site with
  `/docs`," not two subdomains — flagged for correction when that ticket is
  picked up.
- FORNX-37 (Neon) uses one project + branches per the epic; the alternative
  two-project pattern is documented here as a known trade-off, not adopted.
