#!/usr/bin/env bats
# Negative-path and happy-path tests for scripts/release-readiness.sh (FORNX-234).
#
# All cases run against local fixtures (tests/release-readiness/fixtures/<case>/)
# via --evidence-dir and --repo-fixture-dir, so no live gh/Jira calls happen here.

setup() {
  SCRIPT="${BATS_TEST_DIRNAME}/../../scripts/release-readiness.sh"
  FIXTURES="${BATS_TEST_DIRNAME}/fixtures"
}

run_case() {
  local case_name="$1"
  run bash "$SCRIPT" "${FIXTURES}/${case_name}/manifest.json" \
    --evidence-dir "${FIXTURES}/${case_name}/evidence" \
    --repo-fixture-dir "${FIXTURES}/${case_name}/repos"
}

@test "happy path: all real, all Done, all consistent -> ready true, exit 0" {
  run_case happy
  [ "$status" -eq 0 ]
  ready="$(echo "$output" | jq -r '.ready')"
  [ "$ready" = "true" ]
  fails="$(echo "$output" | jq '[.checks[] | select(.status=="fail")] | length')"
  [ "$fails" -eq 0 ]
}

@test "missing sign-off ticket key in manifest -> fails closed, exit 1" {
  run_case missing_ticket
  [ "$status" -eq 1 ]
  ready="$(echo "$output" | jq -r '.ready')"
  [ "$ready" = "false" ]
  echo "$output" | jq -e '.checks[] | select(.name=="evidence.security.PROJ-2.exists" and .status=="fail")'
}

@test "referenced ticket is not Done -> fails closed, exit 1" {
  run_case ticket_not_done
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="evidence.qa.PROJ-1.done" and .status=="fail")'
}

@test "SHA does not exist on target repo -> fails closed, exit 1" {
  run_case sha_not_found
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="repo.acme/widget.sha_exists" and .status=="fail")'
}

@test "SHA is real but not on main -> fails closed, exit 1" {
  run_case sha_not_on_main
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="repo.acme/widget.sha_on_main" and .status=="fail")'
}

@test "stale SHA vs sign-off comment (candidate mutation) -> fails closed, exit 1" {
  run_case stale_signoff
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="evidence.security.PROJ-2.candidate_reference" and .status=="fail")'
}

@test "BLOCK verdict in sign-off ticket -> fails closed even though Done, exit 1" {
  run_case block_verdict
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="evidence.security.PROJ-2.done" and .status=="pass")'
  echo "$output" | jq -e '.checks[] | select(.name=="evidence.security.PROJ-2.not_blocked" and .status=="fail")'
}

@test "manifest missing a required gate entirely -> fails closed, exit 1" {
  run_case missing_gate
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="manifest.gates.presence" and .status=="fail" and (.detail | contains("docs")))'
}

@test "referenced Jira ticket does not exist -> fails closed, exit 1" {
  run_case ticket_does_not_exist
  [ "$status" -eq 1 ]
  echo "$output" | jq -e '.checks[] | select(.name=="evidence.docs.PROJ-3.exists" and .status=="fail")'
}

@test "manifest is not valid JSON -> fails closed, exit 1" {
  tmp="$(mktemp -d)"
  echo "not json" > "$tmp/manifest.json"
  mkdir -p "$tmp/evidence"
  run bash "$SCRIPT" "$tmp/manifest.json" --evidence-dir "$tmp/evidence"
  [ "$status" -eq 1 ]
  ready="$(echo "$output" | jq -r '.ready')"
  [ "$ready" = "false" ]
  rm -rf "$tmp"
}

@test "usage error when manifest arg is missing" {
  run bash "$SCRIPT" --evidence-dir "${FIXTURES}/happy/evidence"
  [ "$status" -eq 2 ]
}
