# ADR 0003: Dependency version policy

Status: accepted (owner directive, 2026-08-29)

## Invariant

**Latest stable by default; pinned for reproducibility; downgrade only with
demonstrated compatibility evidence.**

This applies to every Fornax repository (`fornax-core`, `fornax-cloud`,
`fornax-infra`, `fornax-docs`, `fornax-website`) and every ecosystem in use
(Rust/Cargo, Python/uv, Node/npm-or-pnpm, Terraform, Docker images).

## Rules

1. When selecting or upgrading a dependency, use the current latest **stable**
   release at the time of the decision — not a version remembered from
   training data, an old tutorial, or copied from an unrelated Horonomy
   product. Verify against the authoritative source (crates.io/docs.rs,
   PyPI, npm registry, Terraform Registry, official image registries).
2. No alpha/beta/rc/nightly/pre-release dependencies by default. Only use one
   when Fornax needs a feature unavailable in stable, the risk is understood,
   tests cover it, and the choice is documented at the point of use.
3. After selecting a version, pin/lock it for reproducibility: commit
   `Cargo.lock`, `uv.lock`, the Node lockfile, `.terraform.lock.hcl`, and pin
   Docker image tags (never floating `latest` in reproducible dev/CI/release
   paths).
4. Downgrade from latest stable only with concrete evidence (build failure,
   runtime regression, MSRV/interpreter/runtime incompatibility, known
   upstream bug, security regression, broken API contract) — never merely
   because "older seems safer." When a downgrade is necessary, step down to
   the newest known-compatible version, not an arbitrarily old one, and
   record: the dependency, the attempted latest version, the failure
   evidence, the selected fallback, why it's the newest acceptable choice,
   and the condition for removing the pin.
5. Security posture still wins over recency: do not adopt a version with a
   known vulnerability or from a suspicious/abandoned release merely because
   its version number is higher.
6. Dependency minimalism is unaffected by this policy — "use latest" is not
   license to add dependencies the standard library or existing stack
   already covers.
7. A dependency version change is its own atomic commit (`⬆️ (deps): ...`);
   the lockfile diff it produces belongs in the same commit, not a separate
   "update lockfile" commit. Unrelated upgrades are never bundled together.
8. Where reasonable, configure automated dependency-update detection
   (e.g. Dependabot) per repository — review-gated, never auto-merged for
   arbitrary major bumps, and still subject to normal CI.

## Why

Prevents silent version drift toward stale, remembered, or copy-pasted
dependency choices, while keeping builds reproducible and avoiding
unreviewed floating versions. Downgrades stay possible but must be earned
with evidence, not assumed as the safe default.
