#!/usr/bin/env bash
set -euo pipefail
harness="${P2P_VPN_ANDROID_HARNESS:-$(dirname "$0")/../scripts/android-e2e.sh}"
# shellcheck disable=SC1090
source <(sed -n '/^fixture_paths_fit() {$/,/^}$/p' "$harness")
for scenario in pairing-traffic multi-network multi-network-resource-admission; do
  suffix=/fixture/packet-control.sock
  [[ "$scenario" != multi-network* ]] || suffix=/fixture-secondary/packet-control.sock
  printf -v directory '%*s' "$((107 - ${#suffix}))" ''
  directory=${directory// /x}
  fixture_paths_fit "$directory" "$scenario"
  if fixture_paths_fit "${directory}x" "$scenario"; then
    echo 'An oversized Unix socket path was accepted.' >&2
    exit 1
  fi
done
echo 'Fixture socket byte-budget boundary checks passed.'
