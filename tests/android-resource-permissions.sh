#!/usr/bin/env bash
# The extracted harness helper calls adb_run and reads scenario indirectly.
# shellcheck disable=SC2329,SC2034
set -euo pipefail
harness="${P2P_VPN_ANDROID_E2E_SCRIPT:-$(dirname "$0")/../scripts/android-e2e.sh}"
# Load only this helper; never launch an emulator from the contract test.
# shellcheck disable=SC1090
source <(sed -n '/^prepare_android_resource_permissions() {$/,/^}$/p' "$harness")
calls=0
adb_run() {
  [[ "$*" == 'shell pm grant org.hermeticfoundation.p2pvpn.debug android.permission.POST_NOTIFICATIONS' ]]
  calls=$((calls + 1))
}
for scenario in boot-smoke process-sample-smoke network-workflow multi-network; do
  prepare_android_resource_permissions
done
[[ "$calls" == 0 ]]
for scenario in multi-network-resource-admission multi-network-resource-thread-controls multi-network-resource-load; do
  prepare_android_resource_permissions
done
[[ "$calls" == 3 ]]
adb_run() { return 1; }
if prepare_android_resource_permissions; then
  echo 'Notification grant failure was ignored.' >&2
  exit 1
fi
echo 'Resource-only notification prerequisite and failure propagation passed.'
