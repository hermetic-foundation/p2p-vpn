#!/usr/bin/env bash
# The extracted harness helper consumes these stubs and fixture variables.
# shellcheck disable=SC2034,SC2329
set -euo pipefail
harness="${P2P_VPN_ANDROID_E2E_SCRIPT:-$(dirname "$0")/../scripts/android-e2e.sh}"
# shellcheck disable=SC1090
source <(sed -n '/^measure_concurrent_multi_network_traffic() {$/,/^}$/p' "$harness")
state_dir=$(mktemp -d)
trap 'rm -rf "$state_dir"' EXIT
fixture_command=fixture_stub
fixture_packet_socket=alpha fixture_secondary_packet_socket=beta
fixture_ipv4=192.0.2.1 fixture_ipv6=2001:db8::1
fixture_secondary_ipv4=192.0.2.2 fixture_secondary_ipv6=2001:db8::2
android_primary_ipv4=192.0.2.3 android_primary_ipv6=2001:db8::3
android_secondary_ipv4=192.0.2.4 android_secondary_ipv6=2001:db8::4
monotonic_millis() { printf '100\n'; }
sleep() { :; }
ping_received_count() { printf '0\n'; }
record_step() { printf '%s\n' "$*" >>"$state_dir/steps"; }
fixture_stub() {
  printf 'fixture\n' >>"$state_dir/calls"
  local family=ipv4 arg
  for arg in "$@"; do [[ "$arg" != *:* ]] || family=ipv6; done
  jq -nc --arg family "$family" --argjson received "$replies" \
    '{schema_version:1,ok:($received==5),family:$family,sent:5,received:$received}'
  [[ "$replies" == 5 ]]
}
adb_run() {
  printf 'adb\n' >>"$state_dir/calls"
  printf '5 packets transmitted, %s received, 0%% packet loss\n' "$replies"
  [[ "$replies" == 5 ]]
}
for scenario in multi-network-resource-isolation multi-network-resource-load multi-network; do
  for replies in 0 5; do
    : >"$state_dir/calls"
    actual=0 started=0 duration=0
    measure_concurrent_multi_network_traffic sample test started duration || actual=$?
    expected_calls=8
    if [[ "$replies" == 0 ]]; then
      [[ "$actual" == 1 ]]
      [[ "$scenario" != multi-network ]] || expected_calls=24
    else
      [[ "$actual" == 0 && "$started" == 100 && "$duration" == 0 ]]
    fi
    [[ "$(wc -l <"$state_dir/calls")" == "$expected_calls" ]]
  done
done
echo 'Resource measurements are single-attempt; legacy retries and success remain covered.'
