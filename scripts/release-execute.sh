#!/usr/bin/env bash
# release-execute.sh — FORNX-235
#
# Deterministic, auditable release execution built ON TOP of FORNX-234's
# release-readiness.sh, which is called as a hard precondition gate and
# never reimplemented here.
#
# Usage:
#   release-execute.sh <manifest.json> [--dry-run|--execute] \
#       --evidence-dir <dir> [--repo-fixture-dir <dir>] [--evidence-out <path>]
#
#   release-execute.sh --yank <version> --reason "<text>" --repo <owner>/<name>
#
# Modes:
#   --dry-run (default)  Prints the plan: which repos would get which tag
#                         pushed to which SHA, what the GitHub Release would
#                         contain, what would be checksummed, and which steps
#                         are IRREVERSIBLE (tag push, GitHub Release create)
#                         vs reversible (local build, checksum compute).
#                         Never touches a remote tag or a GitHub Release.
#   --execute             Does it for real: tag-immutability check, tag
#                         create+push, build+checksum (this repo's binaries
#                         only), canonical GitHub Release creation, and
#                         machine-readable evidence recording every step's
#                         PASS/FAIL so a partial failure is never reported
#                         as a full success.
#   --yank                 Marks an already-published GitHub Release as
#                         deprecated/yanked with a reason. Never deletes or
#                         moves the underlying git tag. Not readiness-gated
#                         — yanking a bad release must work even when the
#                         readiness checker would refuse a new one.
#
# Exit codes:
#   0   success (valid dry-run plan against a READY candidate, successful
#       execute, or successful yank)
#   1   execute/yank failed partway — see the evidence file for exactly
#       which step failed
#   2   usage error (bad arguments, missing manifest, missing jq/gh/git)
#   3   readiness refusal — the candidate is not ready per
#       release-readiness.sh; dry-run and execute both refuse with this
#       code and print exactly which readiness checks failed
#
# --dry-run and --execute both require --evidence-dir; --evidence-dir is the
# same production input contract as release-readiness.sh (see
# docs/release-readiness.md): this script has no Jira/MCP access of its own,
# so the caller pre-fetches evidence tickets (including any
# release_notes_ticket) into that directory in the documented fixture shape.
#
# THIS SCRIPT NEVER CALLS JIRA. All ticket content it uses (readiness
# evidence, release notes text) comes from --evidence-dir fixtures the
# caller prepared.

set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
READINESS_SCRIPT="${SCRIPT_DIR}/release-readiness.sh"

EXIT_FAILED=1
EXIT_USAGE=2
EXIT_NOT_READY=3

usage() {
  cat >&2 <<EOF
Usage:
  ${SCRIPT_NAME} <manifest.json> [--dry-run|--execute] --evidence-dir <dir> \\
      [--repo-fixture-dir <dir>] [--evidence-out <path>]

  ${SCRIPT_NAME} --yank <version> --reason "<text>" --repo <owner>/<name>
EOF
  exit "$EXIT_USAGE"
}

MODE="dry-run"
MANIFEST=""
EVIDENCE_DIR=""
REPO_FIXTURE_DIR=""
EVIDENCE_OUT=""
YANK_VERSION=""
YANK_REASON=""
YANK_REPO=""

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) MODE="dry-run"; shift ;;
    --execute) MODE="execute"; shift ;;
    --yank) MODE="yank"; YANK_VERSION="${2:-}"; shift 2 ;;
    --reason) YANK_REASON="${2:-}"; shift 2 ;;
    --repo) YANK_REPO="${2:-}"; shift 2 ;;
    --evidence-dir) EVIDENCE_DIR="${2:-}"; shift 2 ;;
    --repo-fixture-dir) REPO_FIXTURE_DIR="${2:-}"; shift 2 ;;
    --evidence-out) EVIDENCE_OUT="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    *)
      if [ -z "$MANIFEST" ]; then
        MANIFEST="$1"; shift
      else
        usage
      fi
      ;;
  esac
done

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit "$EXIT_USAGE"; }
command -v gh >/dev/null 2>&1 || { echo "gh is required" >&2; exit "$EXIT_USAGE"; }

# ---------------------------------------------------------------------------
# Evidence recording — flushed after every step so a mid-sequence failure
# (fail-fast, "stop fan-out") still leaves a truthful partial record instead
# of no record at all.
# ---------------------------------------------------------------------------

STEPS_FILE="$(mktemp)"
EVIDENCE_TMP="$(mktemp)"
cleanup() { rm -f "$STEPS_FILE" "$EVIDENCE_TMP"; }
trap cleanup EXIT
: > "$STEPS_FILE"

flush_evidence() {
  local overall="PASS"
  if [ -s "$STEPS_FILE" ] && grep -q '"status":"fail"' "$STEPS_FILE" 2>/dev/null; then
    overall="FAIL"
  fi
  jq -n \
    --arg mode "$MODE" \
    --arg overall "$overall" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --slurpfile steps <(cat "$STEPS_FILE" 2>/dev/null || echo '[]') \
    '{mode:$mode, overall:$overall, timestamp:$timestamp, steps:$steps}' > "$EVIDENCE_TMP"
  if [ -n "$EVIDENCE_OUT" ]; then
    cp "$EVIDENCE_TMP" "$EVIDENCE_OUT"
  fi
}

add_step() {
  # add_step <name> <pass|fail> <detail> [extra_json]
  local name="$1" status="$2" detail="$3" extra="${4:-null}"
  jq -nc --arg name "$name" --arg status "$status" --arg detail "$detail" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson extra "$extra" \
    '{name:$name,status:$status,detail:$detail,timestamp:$timestamp,extra:$extra}' >> "$STEPS_FILE"
  flush_evidence
}

step_failed() {
  echo "FAILED: $1" >&2
  flush_evidence
  cat "$EVIDENCE_TMP"
  exit "$EXIT_FAILED"
}

# ---------------------------------------------------------------------------
# Readiness gate — hard precondition, never reimplemented
# ---------------------------------------------------------------------------

run_readiness() {
  local readiness_args=("$MANIFEST" --evidence-dir "$EVIDENCE_DIR")
  if [ -n "$REPO_FIXTURE_DIR" ]; then
    readiness_args+=(--repo-fixture-dir "$REPO_FIXTURE_DIR")
  fi
  READINESS_OUTPUT="$(bash "$READINESS_SCRIPT" "${readiness_args[@]}" 2>&1)" && READINESS_EXIT=0 || READINESS_EXIT=$?
}

require_ready() {
  run_readiness
  if ! echo "$READINESS_OUTPUT" | jq empty >/dev/null 2>&1; then
    echo "readiness check did not return valid JSON:" >&2
    echo "$READINESS_OUTPUT" >&2
    exit "$EXIT_NOT_READY"
  fi
  READY="$(echo "$READINESS_OUTPUT" | jq -r '.ready')"
  if [ "$READY" != "true" ]; then
    echo "REFUSED: candidate is not ready per release-readiness.sh (FORNX-234)." >&2
    echo "Failing checks:" >&2
    echo "$READINESS_OUTPUT" | jq -r '.checks[] | select(.status=="fail") | "  - \(.name): \(.detail)"' >&2
    jq -n --argjson readiness "$READINESS_OUTPUT" \
      '{mode:"refused", ready:false, reason:"release-readiness.sh returned ready:false — see readiness.checks", readiness:$readiness}'
    exit "$EXIT_NOT_READY"
  fi
}

# ---------------------------------------------------------------------------
# Manifest helpers
# ---------------------------------------------------------------------------

[ -n "$MANIFEST" ] || { [ "$MODE" = "yank" ] || usage; }

if [ "$MODE" != "yank" ]; then
  [ -f "$MANIFEST" ] || { echo "manifest not found: $MANIFEST" >&2; exit "$EXIT_USAGE"; }
  [ -n "$EVIDENCE_DIR" ] || usage
  jq empty "$MANIFEST" 2>/dev/null || { echo "manifest is not valid JSON" >&2; exit "$EXIT_USAGE"; }
fi

VERSION="$(jq -r '.version // empty' "$MANIFEST" 2>/dev/null || true)"
RELEASE_NOTES_TICKET="$(jq -r '.release_notes_ticket // empty' "$MANIFEST" 2>/dev/null || true)"
RELEASE_NOTES_PATH="$(jq -r '.release_notes_path // empty' "$MANIFEST" 2>/dev/null || true)"

# The repo this checkout IS — the only repo whose source we can actually
# build. Cross-repo builds are out of scope: other manifest repos get their
# tag/release actions, never a local `cargo build` from this checkout.
THIS_REPO_NAME="fornax-core"
THIS_REPO_OWNER="horonomy"
CANONICAL_RELEASE_REPO_OWNER="horonomy"
CANONICAL_RELEASE_REPO_NAME="fornax-core"

resolve_release_notes_text() {
  # Prints the release notes text to stdout, or nothing + returns 1 if
  # neither source is available.
  if [ -n "$RELEASE_NOTES_TICKET" ] && [ -n "$EVIDENCE_DIR" ] && [ -f "${EVIDENCE_DIR}/${RELEASE_NOTES_TICKET}.json" ]; then
    jq -r '.text // empty' "${EVIDENCE_DIR}/${RELEASE_NOTES_TICKET}.json"
    return 0
  fi
  if [ -n "$RELEASE_NOTES_PATH" ]; then
    local repo_root
    repo_root="$(cd "${SCRIPT_DIR}/.." && pwd)"
    if [ -f "${repo_root}/${RELEASE_NOTES_PATH}" ]; then
      cat "${repo_root}/${RELEASE_NOTES_PATH}"
      return 0
    fi
  fi
  return 1
}

# ---------------------------------------------------------------------------
# External-system operations — every git/gh call lives in one of these
# functions so tests can substitute fake `git`/`gh` binaries earlier in
# PATH and exercise the real production code path, not a parallel test-only
# branch.
# ---------------------------------------------------------------------------

remote_tag_commit_sha() {
  # remote_tag_commit_sha <owner> <repo> <tag>  -> prints commit sha, or
  # returns 1 if the tag doesn't exist.
  local owner="$1" repo="$2" tag="$3" ref_json obj_type obj_sha
  ref_json="$(gh api "repos/${owner}/${repo}/git/ref/tags/${tag}" 2>/dev/null)" || return 1
  obj_type="$(jq -r '.object.type' <<<"$ref_json")"
  obj_sha="$(jq -r '.object.sha' <<<"$ref_json")"
  if [ "$obj_type" = "tag" ]; then
    gh api "repos/${owner}/${repo}/git/tags/${obj_sha}" --jq '.object.sha'
  else
    echo "$obj_sha"
  fi
}

create_and_push_tag() {
  # create_and_push_tag <owner> <repo> <tag> <sha> <message>
  local owner="$1" repo="$2" tag="$3" sha="$4" message="$5" tag_obj_sha
  tag_obj_sha="$(gh api "repos/${owner}/${repo}/git/tags" \
    -f "tag=${tag}" -f "message=${message}" -f "object=${sha}" -f "type=commit" \
    -f "tagger[name]=fornax-release-bot" -f "tagger[email]=release-bot@horonomy.invalid" \
    --jq '.sha')"
  gh api "repos/${owner}/${repo}/git/refs" -f "ref=refs/tags/${tag}" -f "sha=${tag_obj_sha}" >/dev/null
}

github_release_exists() {
  gh release view "$1" --repo "$2" >/dev/null 2>&1
}

create_github_release() {
  # create_github_release <owner/repo> <tag> <title> <notes_file> [asset...]
  local repo="$1" tag="$2" title="$3" notes_file="$4"; shift 4
  gh release create "$tag" --repo "$repo" --title "$title" --notes-file "$notes_file" "$@"
}

build_release_artifacts() {
  local repo_root="$1"
  ( cd "$repo_root" && CARGO_TARGET_DIR=./target cargo build --release --workspace )
}

compute_checksums() {
  # compute_checksums <dir> <bin1> [bin2...]  -> writes sha256 lines to stdout
  local dir="$1"; shift
  for bin in "$@"; do
    local f="${dir}/${bin}"
    if [ ! -f "$f" ]; then
      echo "MISSING:${bin}"
      return 1
    fi
    shasum -a 256 "$f" 2>/dev/null || sha256sum "$f"
  done
}

# ---------------------------------------------------------------------------
# Yank mode — never readiness-gated, never touches the underlying tag
# ---------------------------------------------------------------------------

if [ "$MODE" = "yank" ]; then
  [ -n "$YANK_VERSION" ] || usage
  [ -n "$YANK_REASON" ] || { echo "--reason is required for --yank" >&2; exit "$EXIT_USAGE"; }
  REPO="${YANK_REPO:-${CANONICAL_RELEASE_REPO_OWNER}/${CANONICAL_RELEASE_REPO_NAME}}"

  if ! github_release_exists "$YANK_VERSION" "$REPO"; then
    add_step "yank.release_exists" "fail" "GitHub Release ${YANK_VERSION} not found on ${REPO} — nothing to yank"
    step_failed "release not found"
  fi
  add_step "yank.release_exists" "pass" "found release ${YANK_VERSION} on ${REPO}"

  EXISTING_BODY="$(gh release view "$YANK_VERSION" --repo "$REPO" --json body --jq '.body' 2>/dev/null || echo "")"
  BANNER="> **DEPRECATED / YANKED** — ${YANK_REASON} (recorded $(date -u +%Y-%m-%dT%H:%M:%SZ))

"
  NEW_BODY="${BANNER}${EXISTING_BODY}"
  if gh release edit "$YANK_VERSION" --repo "$REPO" --notes "$NEW_BODY" >/dev/null 2>&1; then
    add_step "yank.annotate" "pass" "release ${YANK_VERSION} on ${REPO} marked deprecated; underlying tag left untouched"
  else
    add_step "yank.annotate" "fail" "gh release edit failed for ${YANK_VERSION} on ${REPO}"
    step_failed "yank annotate failed"
  fi
  flush_evidence
  cat "$EVIDENCE_TMP"
  exit 0
fi

# ---------------------------------------------------------------------------
# Dry-run mode
# ---------------------------------------------------------------------------

if [ "$MODE" = "dry-run" ]; then
  require_ready

  NOTES_SOURCE="null"
  NOTES_STATUS="fail"
  NOTES_DETAIL="neither release_notes_ticket nor release_notes_path is set on the manifest — a real release cannot generate notes until one is added"
  if [ -n "$RELEASE_NOTES_TICKET" ]; then
    NOTES_SOURCE="$(jq -n --arg t "$RELEASE_NOTES_TICKET" '{type:"jira_ticket",ref:$t}')"
    NOTES_STATUS="pass"
    NOTES_DETAIL="notes will be composed from Jira ticket ${RELEASE_NOTES_TICKET} at execute time via its pre-fetched --evidence-dir fixture (this script never calls Jira directly)"
  elif [ -n "$RELEASE_NOTES_PATH" ]; then
    NOTES_SOURCE="$(jq -n --arg p "$RELEASE_NOTES_PATH" '{type:"file",path:$p}')"
    NOTES_STATUS="pass"
    NOTES_DETAIL="notes will be read from committed file ${RELEASE_NOTES_PATH} at execute time"
  fi

  TAG_ACTIONS="$(jq -c '[.repos[]? | select(.publishes_artifact==true) | {
      repo: (.owner + "/" + .name),
      tag: $version,
      sha: .sha,
      action: "create_and_push_annotated_tag",
      irreversible: true
    }]' --arg version "$VERSION" "$MANIFEST")"

  BUILD_ACTIONS='[{"action":"cargo_build_release","repo":"horonomy/fornax-core (this checkout only)","irreversible":false},{"action":"compute_sha256_checksums","binaries":["fornax","fornax-daemon","fornax-hook-claude","fornax-hook-codex"],"irreversible":false}]'

  RELEASE_ACTIONS="$(jq -n --arg repo "${CANONICAL_RELEASE_REPO_OWNER}/${CANONICAL_RELEASE_REPO_NAME}" --arg version "$VERSION" \
    '[{repo:$repo, tag:$version, action:"create_or_verify_idempotent_github_release", title:("Fornax " + $version), irreversible:true}]')"

  jq -n \
    --arg mode "dry-run" \
    --arg version "$VERSION" \
    --argjson readiness "$READINESS_OUTPUT" \
    --argjson notes_source "$NOTES_SOURCE" \
    --arg notes_status "$NOTES_STATUS" \
    --arg notes_detail "$NOTES_DETAIL" \
    --argjson tag_actions "$TAG_ACTIONS" \
    --argjson build_actions "$BUILD_ACTIONS" \
    --argjson release_actions "$RELEASE_ACTIONS" \
    '{
      mode: $mode,
      version: $version,
      readiness: {ready: $readiness.ready, checks: $readiness.checks},
      release_notes: {status: $notes_status, source: $notes_source, detail: $notes_detail},
      plan: {
        reversible_steps: $build_actions,
        irreversible_steps: ($tag_actions + $release_actions)
      }
    }'
  exit 0
fi

# ---------------------------------------------------------------------------
# Execute mode
# ---------------------------------------------------------------------------

require_ready
add_step "readiness.gate" "pass" "release-readiness.sh returned ready:true for version ${VERSION}"

REPO_ROOT="${RELEASE_EXECUTE_REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

mapfile -t PUBLISHING_REPOS < <(jq -c '.repos[]? | select(.publishes_artifact==true)' "$MANIFEST")

TAG_SHAS_JSON="[]"
for repo_json in "${PUBLISHING_REPOS[@]}"; do
  name="$(jq -r '.name' <<<"$repo_json")"
  owner="$(jq -r '.owner' <<<"$repo_json")"
  sha="$(jq -r '.sha' <<<"$repo_json")"
  step="tag.${owner}/${name}.immutability"

  existing_sha=""
  if existing_sha="$(remote_tag_commit_sha "$owner" "$name" "$VERSION")"; then
    if [ "$existing_sha" = "$sha" ]; then
      add_step "$step" "pass" "tag ${VERSION} already exists on ${owner}/${name} at the same sha (${sha}) — idempotent, no push needed"
      TAG_SHAS_JSON="$(jq -c --arg r "${owner}/${name}" --arg s "$sha" --arg action "idempotent_noop" '. + [{repo:$r,sha:$s,action:$action}]' <<<"$TAG_SHAS_JSON")"
      continue
    else
      add_step "$step" "fail" "tag ${VERSION} already exists on ${owner}/${name} at a DIFFERENT commit (${existing_sha}, expected ${sha}) — refusing to move a published tag"
      step_failed "tag immutability violation on ${owner}/${name}"
    fi
  else
    add_step "$step" "pass" "tag ${VERSION} does not yet exist on ${owner}/${name} — safe to create"
  fi

  step="tag.${owner}/${name}.create_push"
  if create_and_push_tag "$owner" "$name" "$VERSION" "$sha" "Fornax ${VERSION}"; then
    add_step "$step" "pass" "created and pushed annotated tag ${VERSION} on ${owner}/${name} at ${sha}"
    TAG_SHAS_JSON="$(jq -c --arg r "${owner}/${name}" --arg s "$sha" --arg action "created" '. + [{repo:$r,sha:$s,action:$action}]' <<<"$TAG_SHAS_JSON")"
  else
    add_step "$step" "fail" "failed to create/push tag ${VERSION} on ${owner}/${name}"
    step_failed "tag create/push failed on ${owner}/${name}"
  fi
done

# Build + checksum — this checkout's binaries only (cross-repo builds are
# out of scope; other repos only get tag/release actions above).
BINARIES=(fornax fornax-daemon fornax-hook-claude fornax-hook-codex)
if build_release_artifacts "$REPO_ROOT"; then
  add_step "build.fornax-core" "pass" "cargo build --release --workspace succeeded"
else
  add_step "build.fornax-core" "fail" "cargo build --release --workspace failed"
  step_failed "build failed"
fi

CHECKSUM_OUTPUT="$(compute_checksums "${REPO_ROOT}/target/release" "${BINARIES[@]}")" && CHECKSUM_STATUS=0 || CHECKSUM_STATUS=$?
if [ "$CHECKSUM_STATUS" -eq 0 ]; then
  add_step "checksum.compute" "pass" "sha256 computed for: ${BINARIES[*]}" "$(jq -n --arg c "$CHECKSUM_OUTPUT" '{checksums:$c}')"
else
  add_step "checksum.compute" "fail" "checksum computation failed: ${CHECKSUM_OUTPUT}"
  step_failed "checksum computation failed"
fi

CHECKSUM_FILE="$(mktemp)"
echo "$CHECKSUM_OUTPUT" > "$CHECKSUM_FILE"

NOTES_TEXT=""
if NOTES_TEXT="$(resolve_release_notes_text)"; then
  add_step "release_notes.assemble" "pass" "release notes resolved (${#NOTES_TEXT} chars) from $( [ -n "$RELEASE_NOTES_TICKET" ] && echo "ticket ${RELEASE_NOTES_TICKET}" || echo "file ${RELEASE_NOTES_PATH}")"
else
  add_step "release_notes.assemble" "fail" "no release notes source resolvable — set release_notes_ticket (with a matching --evidence-dir fixture) or release_notes_path on the manifest"
  step_failed "release notes unresolvable"
fi

NOTES_FILE="$(mktemp)"
printf '%s\n' "$NOTES_TEXT" > "$NOTES_FILE"

CANONICAL_REPO="${CANONICAL_RELEASE_REPO_OWNER}/${CANONICAL_RELEASE_REPO_NAME}"
if github_release_exists "$VERSION" "$CANONICAL_REPO"; then
  add_step "release.create" "pass" "GitHub Release ${VERSION} already exists on ${CANONICAL_REPO} — idempotent, no create needed"
else
  if create_github_release "$CANONICAL_REPO" "$VERSION" "Fornax ${VERSION}" "$NOTES_FILE" "$CHECKSUM_FILE"; then
    add_step "release.create" "pass" "created GitHub Release ${VERSION} on ${CANONICAL_REPO} with checksum attachment"
  else
    add_step "release.create" "fail" "gh release create failed for ${VERSION} on ${CANONICAL_REPO}"
    step_failed "release create failed"
  fi
fi

rm -f "$CHECKSUM_FILE" "$NOTES_FILE"

flush_evidence
cat "$EVIDENCE_TMP"
exit 0
