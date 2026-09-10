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
echo 'Synthetic 60/300-second window scaling and duration rejection checks passed.'
