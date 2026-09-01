# Worker evidence contract (provisional)

Jira: FORNX-230. This is the **transient** shape a lane worker hands back to
the coordinator during one `/release-qa-gate` run — just enough for the
coordinator to compute the aggregate verdict in
[`SKILL.md`](SKILL.md) Step 6. It is deliberately minimal.

**This is not the durable QA sign-off artifact.** FORNX-232 ("Add durable
QA sign-off, independent finding verification, runtime recipes and reusable
verifier agents") owns that format and the finding lifecycle, and is not
yet built. When FORNX-232 lands, this contract is superseded by whatever
schema that ticket defines for the durable record; this file's shape stays
in use only as the in-run coordinator/worker handoff, and should be updated
to conform to FORNX-232's schema at that point rather than kept as a second,
diverging shape.

## Shape

```jsonc
{
  "lane": "adapters_providers",          // one of the 8 lane ids from lane-surface-mapping.md
  "verdict": "BLOCK",                    // PASS | BLOCK | INCONCLUSIVE | UNTESTED — FORNX-229 vocabulary, no other spelling
  "risk_class_depth_applied": "TRUST_BOUNDARY",
  "checks": [
    {
      "name": "fornax-hook-codex event-shape correctness",
      "verdict": "PASS",
      "evidence": {
        "kind": "ci_reused",             // ci_reused | ci_rerun | local_repro | golden_journey | static_review
        "ref": "github.com/horonomy/fornax-core/actions/runs/123456", // run URL, commit SHA, file path, or journey ID — never a raw log dump
        "note": "exact-candidate SHA 9e75d95, green, cited not re-run"
      }
    }
  ],
  "blocked_or_untested_surfaces": [],    // FORNX-231 surface ids left unverified in this lane; [] only if genuinely none
  "findings": [                          // present only when verdict is BLOCK; kept to a pointer, not a full report
    {
      "severity": "High",
      "summary": "one-line description",
      "jira_key": "FORNX-9999",          // filed/linked per SKILL.md Step 7; null until filed
      "independently_reproduced": true    // required true for High/Critical at TRUST_BOUNDARY+ per FORNX-229
    }
  ]
}
```

## Rules

- `verdict` uses FORNX-229's four-state vocabulary only. No `WARN`, no
  `PARTIAL`, no bare boolean.
- `evidence.ref` is always a pointer (URL, SHA, path, journey ID) — a worker
  returning inline raw log text or chain-of-thought instead of a pointer is
  a contract violation, not a stylistic choice (this is the AC's "workers
  return compact evidence, not chain-of-thought or giant raw logs").
- `blocked_or_untested_surfaces` is never omitted to make a lane look
  cleaner — an empty array is an explicit claim ("nothing left unverified
  in this lane"), not the absence of the field.
- A `BLOCK` verdict always carries at least one `findings[]` entry; a
  `findings[]` entry always carries a `jira_key` once Step 7 has run (may be
  `null` only in the first coordinator report, before filing).
