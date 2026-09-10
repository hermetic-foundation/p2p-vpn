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
