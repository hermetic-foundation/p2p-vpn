#!/usr/bin/env bash
set -euo pipefail
controls="${P2P_VPN_ANDROID_RESOURCE_CONTROLS:-$(dirname "$0")/../scripts/android-resource-controls.sh}"
# shellcheck disable=SC1090
source "$controls"
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
export P2P_VPN_TEST_CLOCK="$root/clock"
resource_collector="$root/collector.sh"
# Emulator ID is consumed by the sourced window function.
# shellcheck disable=SC2034
resource_app_pid=42 resource_emulator_pid=43
# Keep expansions and the line continuation for the generated collector.
# shellcheck disable=SC2016,SC1003
printf '%s\n' '#!/bin/sh' \
  'jq -nc --argjson pid "$1" --argjson t "$(cat "$P2P_VPN_TEST_CLOCK")" \' \
  "'{pid:\$pid,start_ticks:123,started_uptime_seconds:(\$t/1000),user_ticks:1,system_ticks:1}'" \
  >"$resource_collector"
monotonic_millis() { cat "$P2P_VPN_TEST_CLOCK"; }
resource_sleep_until() { printf '%s\n' "$1" >"$P2P_VPN_TEST_CLOCK"; }
resource_native_sample() { printf '{}\n' >>"$1"; }
# Called by the sourced window implementation.
# shellcheck disable=SC2329
adb_run() { sh "$resource_collector" "$resource_app_pid" 1; }
for duration in 60 300; do
  printf '1000\n' >"$P2P_VPN_TEST_CLOCK"
  resource_control_window off "$root/$duration" "$duration"
  jq -e --argjson duration "$duration" '
    .requested_window_millis == ($duration * 1000) and
    (.finished_millis - .started_millis) == ($duration * 1000)
  ' "$root/$duration-window.json" >/dev/null
done
for duration in 0 61 301 invalid; do
  result=0
  resource_control_window off "$root/invalid" "$duration" || result=$?
  [[ "$result" == 2 ]]
done
result=0
resource_control_window invalid "$root/invalid" 60 || result=$?
[[ "$result" == 2 ]]
for detail in invalid threads; do
  result=0
  resource_control_window off "$root/invalid" 60 idle "$detail" || result=$?
  [[ "$result" == 2 ]]
done
valid='{"thread_scan":{"listed":1,"observed":1,"skipped":0,"threads":[{"tid":42,"start_ticks":1,"user_ticks":0,"system_ticks":0,"voluntary_context_switches":0,"involuntary_context_switches":0}]}}'
resource_thread_samples_valid <(printf '%s\n' "$valid")
for filter in '.thread_scan.skipped=1' '.thread_scan.listed=257' \
  '.thread_scan.threads[0].start_ticks=0' \
  '.thread_scan.threads[0].user_ticks=null' \
  '.thread_scan.threads[0].voluntary_context_switches=null' \
  '.thread_scan.threads += .thread_scan.threads | .thread_scan.listed=2 | .thread_scan.observed=2'; do
  if resource_thread_samples_valid <(jq "$filter" <<<"$valid"); then
    echo "Incomplete or ambiguous thread evidence accepted: $filter" >&2
    exit 1
  fi
done
echo 'Synthetic 60/300-second window scaling and duration rejection checks passed.'

# Exercise phase dispatch without ADB, wall-clock waits or traffic.
# shellcheck disable=SC2034
output_dir="$root"
calls=()
record_step() { :; }
# Invoked by the sourced phase runner.
# shellcheck disable=SC2329
resource_control_window() { calls+=("$*"); }
for detail in process threads; do
  calls=()
  run_android_sustained_load "$detail"
  [[ "${#calls[@]}" == 3 ]]
  [[ "${calls[0]}" == "on $root/sustained-idle 300 idle $detail" ]]
  [[ "${calls[1]}" == "on $root/sustained-load 300 load $detail" ]]
  [[ "${calls[2]}" == "on $root/sustained-drain 60 idle $detail" ]]
done
calls=()
run_android_sustained_load
[[ "${calls[0]}" == "on $root/sustained-idle 300 idle process" ]]
calls=()
result=0
run_android_sustained_load invalid || result=$?
[[ "$result" == 2 && "${#calls[@]}" == 0 ]]
# Called by the sourced phase runner.
# shellcheck disable=SC2329
resource_control_window() {
  calls+=("$*")
  [[ "$4" != load ]]
}
result=0
run_android_sustained_load threads || result=$?
[[ "$result" == 1 && "${#calls[@]}" == 2 ]]
# Read by the sourced controls entry point.
# shellcheck disable=SC2034
for scenario in multi-network-resource-load multi-network-resource-idle; do
  result=0
  P2P_VPN_ANDROID_RESOURCE_LOAD_DETAIL=invalid run_android_resource_controls || result=$?
  [[ "$result" == 2 ]]
done
result=0
P2P_VPN_ANDROID_RESOURCE_LOAD_DETAIL=threads run_android_resource_controls || result=$?
[[ "$result" == 2 ]]
echo 'Sustained phase defaults, thread dispatch and fail-fast behavior passed.'

# Profile lifecycle uses only mocked ADB and recorder commands.
# shellcheck disable=SC2034
adb=(mock-adb)
profile_size=100 record_status=0 traffic_status=0
touch "$root/simpleperf"
adb_run() {
  printf '%s\n' "$*" >>"$root/profile-adb"
  case "$1 $2" in
    'shell mktemp') printf '/data/local/tmp/p2p-vpn-profile.test\n' ;;
    'pull /data/local/tmp/p2p-vpn-profile.test/perf.data') printf 'fixture\n' >"$3" ;;
  esac
}
timeout() {
  printf '%s\n' "$*" >"$root/profile-command"
  return "$record_status"
}
stat() { printf '%s\n' "$profile_size"; }
resource_control_window() {
  [[ "$*" == "on $root/load-smoke 60 load" ]] || return 1
  return "$traffic_status"
}
resource_profile_load "$root/simpleperf"
for option in '-p 42' '-e cpu-clock:u' '-f 99' '--duration 60' '--no-inherit' \
  '-m 64' '--user-buffer-size 1M' '--size-limit 8M' '--no-dump-symbols' \
  '--no-dump-kernel-symbols' '--exit-with-parent'; do
  grep -Fq -- "$option" "$root/profile-command"
done
grep -Fq 'shell rm -rf /data/local/tmp/p2p-vpn-profile.test' "$root/profile-adb"
for failure in recorder traffic empty capped; do
  profile_size=100 record_status=0 traffic_status=0
  case "$failure" in
    recorder) record_status=1 ;;
    traffic) traffic_status=1 ;;
    empty) profile_size=0 ;;
    capped) profile_size=8388608 ;;
  esac
  result=0
  resource_profile_load "$root/simpleperf" || result=$?
  [[ "$result" == 1 ]]
  [[ -f "$root/profile.data" ]]
done
result=0
resource_profile_load "$root/missing" || result=$?
[[ "$result" == 2 ]]
echo 'Bounded profile commands, failure retention and cleanup passed.'
