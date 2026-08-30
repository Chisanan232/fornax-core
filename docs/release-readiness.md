# Release readiness checker

Jira: FORNX-234. Turns release assurance into a mechanical chokepoint: a
missing or stale QA/Security/Docs/Stage sign-off cannot be waved through by a
narrative summary, because a script — not a person's paraphrase — decides.

This is scoped to the checker itself. Wiring it into tag/publish/promotion
automation as an enforced gate is FORNX-235 and is not done here; this doc
covers what exists today.

## The candidate manifest

A release candidate — potentially spanning several repos — is described by
one JSON file validated against
[`release/candidate-manifest.schema.json`](../release/candidate-manifest.schema.json).
See [`release/example-candidate-manifest.json`](../release/example-candidate-manifest.json)
for an annotated example and
[`release/v0.0.1-candidate-manifest.json`](../release/v0.0.1-candidate-manifest.json)
for the real Fornax v0.0.1 candidate as of this writing.

```jsonc
{
  "version": "v0.0.1",
  "repos": [
    { "name": "fornax-core", "owner": "horonomy", "sha": "<40-char sha>", "publishes_artifact": true }
    // one entry per repo the candidate spans — never assume all repo HEADs
    // are implicitly synchronized
  ],
  "evidence": [
    {
      "gate": "qa",              // one of: qa, security, docs, stage
      "jira_key": "FORNX-238",   // the Jira ticket carrying the sign-off
      "jira_url": "https://.../browse/FORNX-238",
      "applies_to_repos": ["fornax-core"]  // [] = candidate-wide, checked
                                            // against `version` instead of a SHA
    }
  ]
}
```

**Jira is the evidence source of truth.** The manifest never duplicates
sign-off content — it only points at the ticket that carries it. All four
gates (`qa`, `security`, `docs`, `stage`) are required; a manifest missing
any one of them fails closed.

## Running it

```bash
scripts/release-readiness.sh <manifest.json> --evidence-dir <dir> [--repo-fixture-dir <dir>]
```

### `--evidence-dir` (required) — the Jira evidence input contract

A shell script has no MCP/Jira API access of its own, so it never calls Jira
directly. Instead, the caller (an agent with `getJiraIssue` access, or any
script hitting the Jira REST API) pre-fetches each evidence ticket referenced
by the manifest into a normalized fixture, one file per ticket:

```
<evidence-dir>/<JIRA-KEY>.json
```

```jsonc
{
  "key": "FORNX-238",
  "exists": true,
  "status_category": "done",  // "new" | "indeterminate" | "done" — Jira's
                               // statusCategory.key, never the display name
  "status_name": "Done",
  "text": "<issue description + every comment body, concatenated>"
}
```

A ticket that doesn't exist is represented as `{"exists": false}` (or the
file simply absent) — either way the checker fails closed rather than
skipping the gate.

### `--repo-fixture-dir` (test-only override)

In production, repo/SHA checks call `gh api` directly — no override needed.
Tests pass `--repo-fixture-dir <dir>` to substitute fixtures instead of
hitting GitHub, one file per repo:

```
<repo-fixture-dir>/<owner>__<repo>.json
```

```jsonc
{ "commit_exists": true, "compare_status": "identical" }
```

## What it checks (and fails closed on)

| Check | Pass condition | Fails on |
|---|---|---|
| `manifest.gates.presence` | all of `qa`, `security`, `docs`, `stage` appear at least once | any gate missing entirely |
| `repo.<owner>/<name>.sha_exists` | `gh api repos/<owner>/<repo>/commits/<sha>` returns 200 | SHA not found (typo, wrong repo, unpushed commit) |
| `repo.<owner>/<name>.sha_on_main` | `gh api repos/<owner>/<repo>/compare/main...<sha>` status is `identical` or `behind` | status is `ahead`/`diverged` (SHA is real but not an ancestor of `main`) or the compare call errors |
| `evidence.<gate>.<key>.exists` | evidence fixture present with `exists: true` | ticket doesn't exist, or evidence wasn't fetched at all — never treated as a silent pass |
| `evidence.<gate>.<key>.done` | `status_category == "done"` | ticket open/in-progress; keyed off Jira's `statusCategory`, not a display-string guess |
| `evidence.<gate>.<key>.not_blocked` | ticket text contains no `BLOCK` verdict | a reviewer recorded an explicit BLOCK — Done status alone is not enough |
| `evidence.<gate>.<key>.candidate_reference` | ticket text references the exact SHA (7+ char prefix) of every repo in `applies_to_repos`, or the manifest `version` when `applies_to_repos` is empty | **candidate mutation**: the sign-off doesn't mention *this* candidate at all, or mentions a different one — a ticket with zero SHA/version reference is a fail, not a pass, since an unreferenced sign-off can't be tied to this candidate |

Any single failing check makes the whole run `ready: false` and the process
exits non-zero. There is no partial-credit path.

## Output shape

```json
{
  "ready": false,
  "version": "v0.0.1",
  "checks": [
    {"name": "evidence.qa.FORNX-238.done", "status": "fail", "detail": "ticket FORNX-238 is not Done (status: To Do)"}
  ],
  "candidate": { "repos": [...], "evidence": [...] }
}
```

Compact, one object, every check individually named and detailed — auditable
without re-deriving anything from raw logs.

## Testing

`tests/release-readiness/release_readiness.bats` (bats-core; installed via
Homebrew on this machine, no new project dependency) runs the full negative
and happy-path matrix entirely against local fixtures under
`tests/release-readiness/fixtures/` — no live `gh`/Jira calls, so it's fast
and deterministic:

```bash
bats tests/release-readiness/release_readiness.bats
```

Covers: missing sign-off ticket key, ticket not Done, SHA not found, SHA
real-but-not-on-main, stale/mutated SHA reference, explicit BLOCK verdict,
a manifest missing a required gate, a referenced ticket that doesn't exist,
malformed JSON, and the happy path.

## Known limitations (out of scope for FORNX-234)

- Not wired into any CI/tag/publish workflow yet — FORNX-235.
- Doesn't validate version/schema/migration compatibility metadata or
  changelog consistency, and doesn't check a release-blocker list —
  these need FORNX-229/230/231/232/233/216 first, which don't exist yet.
- `candidate_reference` matching is a case-insensitive substring/prefix scan
  of ticket text, not a structured field. It's deliberately strict (no
  reference at all is a fail) but can't distinguish "this comment quotes an
  old SHA for historical context" from a genuine stale sign-off — reviewers
  writing sign-off comments should state the candidate SHA/version plainly.
