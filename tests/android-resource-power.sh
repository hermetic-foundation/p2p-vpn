#!/usr/bin/env bash
set -euo pipefail
controls="${P2P_VPN_ANDROID_RESOURCE_CONTROLS:-$(dirname "$0")/../scripts/android-resource-controls.sh}"
# shellcheck disable=SC1090
source "$controls"
for state in Asleep Dozing; do
  resource_power_is_background <(printf '  mWakefulness=%s\n  mWakefulnessChanging=false\n  mHalInteractiveModeEnabled=false\n' "$state")
done
for invalid in \
  'Awake false false' 'Dozing true false' 'Dozing false true' 'Asleep true true'; do
  read -r state changing interactive <<<"$invalid"
  if resource_power_is_background <(printf '  mWakefulness=%s\n  mWakefulnessChanging=%s\n  mHalInteractiveModeEnabled=%s\n' "$state" "$changing" "$interactive"); then
    echo 'Unsettled or interactive power state accepted.' >&2
    exit 1
  fi
done
if resource_power_is_background /dev/null; then
  echo 'Missing power state accepted.' >&2
  exit 1
fi
echo 'Background power-state checks passed.'
