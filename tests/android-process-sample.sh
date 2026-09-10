#!/usr/bin/env bash
set -euo pipefail
collector="$(dirname "$0")/../scripts/android-process-sample.sh"
expect_status() {
  local expected="$1" actual=0
  shift
  "$@" || actual=$?
  [[ "$actual" == "$expected" ]]
}
expect_status 2 sh "$collector"
expect_status 2 sh "$collector" 0 1
expect_status 2 sh "$collector" 1 901
expect_status 2 sh "$collector" invalid 1
expect_status 2 sh "$collector" 01 1
expect_status 1 env P2P_VPN_SAMPLE_PROC_ROOT=/nonexistent sh "$collector" 1 1
sh "$collector" "$$" 2 | jq -es --argjson pid "$$" '
  length == 2 and all(.[];
    .schema_version == 1 and .pid == $pid and .start_ticks > 0 and
    .os_threads >= 1 and .fds >= 3 and .rss_kib > 0 and
    .leader_voluntary_context_switches >= 0 and
    .leader_involuntary_context_switches >= 0 and
    .finished_uptime_seconds >= .started_uptime_seconds) and
  .[0].start_ticks == .[1].start_ticks and
  .[1].started_uptime_seconds >= (.[0].started_uptime_seconds + 1)
' >/dev/null
printf 'Process collector live samples and invalid-input checks passed.\n'

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir "$fixture/42"
printf '123.45 200.00\n' >"$fixture/uptime"
# comm contains both spaces and a close parenthesis; numeric fields are distinct.
printf '42 (test ) process) S 4 5 6 7 8 9 10 11 12 13 140 150 16 17 18 19 20 21 220 23 24\n' \
  >"$fixture/42/stat"
env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1 | jq -e '
  .start_ticks == 220 and .user_ticks == 140 and .system_ticks == 150 and
  .os_threads == 20 and .started_uptime_seconds == 123.45 and
  .rss_kib == null and .fds == null and
  .leader_voluntary_context_switches == null and
  .leader_involuntary_context_switches == null
' >/dev/null
printf '42 (short) S 1 2\n' >"$fixture/42/stat"
expect_status 1 env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1
printf 'Process collector comm parsing, missing fields and malformed stat checks passed.\n'
