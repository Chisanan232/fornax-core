#!/usr/bin/env bats
# Tests for scripts/release-execute.sh (FORNX-235).
#
# Every git/gh/cargo call in the script lives behind a function; these tests
# put fake `gh` and `cargo` binaries (tests/release-execute/fakebin/) ahead
# of the real ones on PATH and exercise the actual production code path, not
# a parallel test-only branch. No live gh/git/Jira/cargo call happens here.
# `RELEASE_EXECUTE_REPO_ROOT` redirects the (faked) build into a scratch dir
# instead of this checkout.

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/../../scripts/release-execute.sh"
  FIXTURES="${BATS_TEST_DIRNAME}/fixtures"
  FAKEBIN="${BATS_TEST_DIRNAME}/fakebin"
  export PATH="${FAKEBIN}:${PATH}"
  export RELEASE_EXECUTE_REPO_ROOT="$(mktemp -d)"
  export FAKE_GH_LOG="$(mktemp)"
  export FAKE_CARGO_LOG="$(mktemp)"
  unset FAKE_TAG_EXISTS FAKE_TAG_SHA FAKE_RELEASE_EXISTS FAKE_RELEASE_CREATE_FAIL FAKE_CARGO_FAIL || true
}

teardown() {
  rm -rf "$RELEASE_EXECUTE_REPO_ROOT"
  rm -f "$FAKE_GH_LOG" "$FAKE_CARGO_LOG"
}

# --- dry-run -----------------------------------------------------------

@test "dry-run refuses when readiness fails, exit 3, explains why" {
  F="${FIXTURES}/../../release-readiness/fixtures/ticket_not_done"
  run bash "$SCRIPT" "${F}/manifest.json" --dry-run \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos"
  [ "$status" -eq 3 ]
  [[ "$output" == *"REFUSED"* ]]
  json="$(echo "$output" | sed -n '/^{/,$p')"
  echo "$json" | jq -e '.ready == false'
  echo "$json" | jq -e '.readiness.checks[] | select(.name=="evidence.qa.PROJ-1.done" and .status=="fail")'
}

@test "dry-run on a ready candidate prints the full plan, exit 0" {
  F="${FIXTURES}/happy"
  run bash "$SCRIPT" "${F}/manifest.json" --dry-run \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos"
  [ "$status" -eq 0 ]
  echo "$output" | jq -e '.readiness.ready == true'
  echo "$output" | jq -e '.plan.irreversible_steps | length == 2'
  echo "$output" | jq -e '.plan.irreversible_steps[] | select(.action=="create_and_push_annotated_tag")'
  echo "$output" | jq -e '.plan.reversible_steps[] | select(.action=="cargo_build_release")'
  echo "$output" | jq -e '.release_notes.status == "pass"'
}

@test "dry-run flags a missing release-notes source as a plan-time fail, still exit 0" {
  F="${FIXTURES}/happy"
  tmp="$(mktemp -d)"
  jq 'del(.release_notes_ticket)' "${F}/manifest.json" > "${tmp}/manifest.json"
  run bash "$SCRIPT" "${tmp}/manifest.json" --dry-run \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos"
  [ "$status" -eq 0 ]
  echo "$output" | jq -e '.release_notes.status == "fail"'
  rm -rf "$tmp"
}

# --- execute: tag immutability -----------------------------------------

@test "execute: tag does not exist yet -> created, idempotent-clean happy path, exit 0" {
  F="${FIXTURES}/happy"
  export FAKE_TAG_EXISTS=false
  export FAKE_RELEASE_EXISTS=false
  out="$(mktemp)"
  run bash "$SCRIPT" "${F}/manifest.json" --execute \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos" --evidence-out "$out"
  [ "$status" -eq 0 ]
  jq -e '.overall == "PASS"' "$out"
  jq -e '.steps[] | select(.name=="tag.acme/widget.create_push" and .status=="pass")' "$out"
  jq -e '.steps[] | select(.name=="release.create" and .status=="pass")' "$out"
  grep -q "git/refs -f ref=refs/tags/v1.2.3" "$FAKE_GH_LOG"
  rm -f "$out"
}

@test "execute: existing tag at the SAME sha is treated as idempotent success" {
  F="${FIXTURES}/happy"
  export FAKE_TAG_EXISTS=true
  export FAKE_TAG_SHA="abcdef1234567890abcdef1234567890abcdef12"
  export FAKE_RELEASE_EXISTS=true
  out="$(mktemp)"
  run bash "$SCRIPT" "${F}/manifest.json" --execute \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos" --evidence-out "$out"
  [ "$status" -eq 0 ]
  jq -e '.overall == "PASS"' "$out"
  jq -e '.steps[] | select(.name=="tag.acme/widget.immutability" and (.detail | contains("idempotent")))' "$out"
  # no create_push step should have run — nothing to push
  run jq -e '.steps[] | select(.name=="tag.acme/widget.create_push")' "$out"
  [ "$status" -ne 0 ]
  rm -f "$out"
}

@test "execute: existing tag at a DIFFERENT sha is a hard fail, never moves the tag" {
  F="${FIXTURES}/happy"
  export FAKE_TAG_EXISTS=true
  export FAKE_TAG_SHA="1111111111111111111111111111111111111111"
  out="$(mktemp)"
  run bash "$SCRIPT" "${F}/manifest.json" --execute \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos" --evidence-out "$out"
  [ "$status" -eq 1 ]
  jq -e '.overall == "FAIL"' "$out"
  jq -e '.steps[] | select(.name=="tag.acme/widget.immutability" and .status=="fail")' "$out"
  # must never attempt to create/push a tag once immutability fails
  ! grep -q "git/refs -f ref=refs/tags" "$FAKE_GH_LOG"
  rm -f "$out"
}

# --- execute: partial-failure evidence ----------------------------------

@test "execute: a mid-sequence build failure records a truthful partial evidence file" {
  F="${FIXTURES}/happy"
  export FAKE_TAG_EXISTS=false
  export FAKE_CARGO_FAIL=true
  out="$(mktemp)"
  run bash "$SCRIPT" "${F}/manifest.json" --execute \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos" --evidence-out "$out"
  [ "$status" -eq 1 ]
  jq -e '.overall == "FAIL"' "$out"
  jq -e '.steps[] | select(.name=="tag.acme/widget.create_push" and .status=="pass")' "$out"
  jq -e '.steps[] | select(.name=="build.fornax-core" and .status=="fail")' "$out"
  # steps after the failure must never appear — the evidence is truthful
  # about exactly where the sequence stopped, not a false overall failure
  # that hides which earlier steps actually succeeded
  run jq -e '.steps[] | select(.name=="release.create")' "$out"
  [ "$status" -ne 0 ]
  rm -f "$out"
}

@test "execute refuses when readiness fails, exit 3, no gh call made" {
  F="${FIXTURES}/../../release-readiness/fixtures/ticket_not_done"
  run bash "$SCRIPT" "${F}/manifest.json" --execute \
    --evidence-dir "${F}/evidence" --repo-fixture-dir "${F}/repos"
  [ "$status" -eq 3 ]
  [ ! -s "$FAKE_GH_LOG" ]
}

# --- yank ----------------------------------------------------------------

@test "yank marks a release deprecated without touching the tag, and is not readiness-gated" {
  export FAKE_RELEASE_EXISTS=true
  run bash "$SCRIPT" --yank v1.2.3 --reason "critical regression" --repo horonomy/fornax-core
  [ "$status" -eq 0 ]
  echo "$output" | jq -e '.overall == "PASS"'
  echo "$output" | jq -e '.steps[] | select(.name=="yank.annotate" and .status=="pass")'
  ! grep -q "git/refs\|git/tags" "$FAKE_GH_LOG"
}

@test "yank on a nonexistent release fails closed" {
  export FAKE_RELEASE_EXISTS=false
  run bash "$SCRIPT" --yank v9.9.9 --reason "does not exist" --repo horonomy/fornax-core
  [ "$status" -eq 1 ]
}

@test "yank without --reason is a usage error" {
  run bash "$SCRIPT" --yank v1.2.3 --repo horonomy/fornax-core
  [ "$status" -eq 2 ]
}

# --- usage -----------------------------------------------------------------

@test "missing --evidence-dir is a usage error, exit 2" {
  F="${FIXTURES}/happy"
  run bash "$SCRIPT" "${F}/manifest.json" --dry-run
  [ "$status" -eq 2 ]
}

@test "missing manifest file is a usage error, exit 2" {
  run bash "$SCRIPT" "/nonexistent/manifest.json" --dry-run --evidence-dir "${FIXTURES}/happy/evidence"
  [ "$status" -eq 2 ]
}
