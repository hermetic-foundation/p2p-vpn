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
expect_status 2 sh "$collector" 1 1 --unknown
expect_status 1 env P2P_VPN_SAMPLE_PROC_ROOT=/nonexistent sh "$collector" 1 1
sh "$collector" "$$" 2 | jq -es --argjson pid "$$" '
  length == 2 and all(.[];
    .schema_version == 1 and .pid == $pid and .start_ticks > 0 and
    .os_threads >= 1 and .fds >= 3 and .rss_kib > 0 and
    .leader_voluntary_context_switches >= 0 and
    .leader_involuntary_context_switches >= 0 and
    .finished_uptime_seconds >= .started_uptime_seconds) and
  .[0].start_ticks == .[1].start_ticks and
  all(.[]; has("thread_scan") | not) and
  .[1].started_uptime_seconds >= (.[0].started_uptime_seconds + 1)
' >/dev/null
printf 'Process collector live samples and invalid-input checks passed.\n'
sh "$collector" "$$" 2 --threads | jq -es --argjson pid "$$" '
  length == 2 and all(.[];
    .thread_scan.listed >= 1 and .thread_scan.skipped == 0 and
    .thread_scan.observed == (.thread_scan.threads | length) and
    any(.thread_scan.threads[]; .tid == $pid and .start_ticks > 0 and
      .user_ticks >= 0 and .system_ticks >= 0 and
      .voluntary_context_switches >= 0 and .involuntary_context_switches >= 0))
' >/dev/null

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
expect_status 1 env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1 --threads
mkdir -p "$fixture/42/task/42" "$fixture/42/task/43" "$fixture/42/task/44"
cp "$fixture/42/stat" "$fixture/42/task/42/stat"
printf 'Name: secret-thread-name\nvoluntary_ctxt_switches: 7\nnonvoluntary_ctxt_switches: 9\n' >"$fixture/42/task/42/status"
cp "$fixture/42/stat" "$fixture/42/task/43/stat"
printf 'Name: private\nvoluntary_ctxt_switches: invalid\n' >"$fixture/42/task/43/status"
# Thread 44 models an enumerated task that vanished before its stat could be read.
env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1 --threads | jq -e '
  .thread_scan.listed == 3 and .thread_scan.observed == 2 and .thread_scan.skipped == 1 and
  .thread_scan.threads[0] == {tid:42,start_ticks:220,user_ticks:140,system_ticks:150,
    voluntary_context_switches:7,involuntary_context_switches:9} and
  .thread_scan.threads[1].voluntary_context_switches == null and
  .thread_scan.threads[1].involuntary_context_switches == null and
  (tostring | contains("secret") or contains("private") | not)
' >/dev/null
rm "$fixture/42/task/42/status"
mkfifo "$fixture/42/task/42/status"
# Hold status open while replacing stat, so the second identity read must differ.
# Expansion belongs to the bounded child shell.
# shellcheck disable=SC2016
timeout --kill-after=1s 5 bash -c '
  fixture=$1 collector=$2
  (
    printf "voluntary_ctxt_switches: 7\n"
    printf "42 (replacement) S 4 5 6 7 8 9 10 11 12 13 140 150 16 17 18 19 20 21 221 23 24\n" \
      >"$fixture/42/task/42/stat"
  ) >"$fixture/42/task/42/status" &
  writer=$!
  env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1 --threads |
    jq -e ".thread_scan.listed == 3 and .thread_scan.observed == 1 and .thread_scan.skipped == 2 and .thread_scan.threads[0].tid == 43"
  status=${PIPESTATUS[1]}
  wait "$writer"
  exit "$status"
' bash "$fixture" "$collector" >/dev/null
rm "$fixture/42/task/42/status"
printf 'voluntary_ctxt_switches: 7\n' >"$fixture/42/task/42/status"
for tid in $(seq 1000 1252); do mkdir "$fixture/42/task/$tid"; done
env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1 --threads | jq -e '
  .thread_scan.listed == 256 and .thread_scan.observed == 2 and .thread_scan.skipped == 254
' >/dev/null
mkdir "$fixture/42/task/1253"
expect_status 1 env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1 --threads
printf 'Thread collector identity fields, missing counters, skips, privacy and bounds passed.\n'
printf '42 (short) S 1 2\n' >"$fixture/42/stat"
expect_status 1 env P2P_VPN_SAMPLE_PROC_ROOT="$fixture" sh "$collector" 42 1
printf 'Process collector comm parsing, missing fields and malformed stat checks passed.\n'

# Load only the timing helpers; do not invoke the collector entry point.
eval "$(sed -n '/^unsigned() {/,/^}/p' "$collector")"
eval "$(sed -n '/^uptime_centiseconds() {/,/^if { /p' "$collector" | sed '$d')"
# Timing helpers read these globals through the extracted definitions.
# shellcheck disable=SC2034
proc="$fixture"
slept=""
sleep() { slept="$1"; }
for timing in '123.45 123.45 1.00' '123.45 123.48 0.97' \
  '123.99 124.08 0.91' '123.45 124.44 0.01'; do
  read -r started now expected <<<"$timing"
  printf '%s 0.00\n' "$now" >"$fixture/uptime"
  thread_sample_sleep
  [[ "$slept" == "$expected" ]]
done
for now in 124.45 124.46 123.44 invalid 124.4; do
  # shellcheck disable=SC2034
  started=123.45
  printf '%s 0.00\n' "$now" >"$fixture/uptime"
  expect_status 1 thread_sample_sleep
done
printf 'Thread timing subtracts scan work and rejects overruns or invalid clocks.\n'
