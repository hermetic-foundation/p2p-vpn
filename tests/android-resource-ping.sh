#!/usr/bin/env bash
set -euo pipefail
controls="${P2P_VPN_ANDROID_RESOURCE_CONTROLS:-$(dirname "$0")/../scripts/android-resource-controls.sh}"
# shellcheck disable=SC1090
source "$controls"
for received in 'received' 'packets received'; do
  resource_ping_summary <(printf '3000 packets transmitted, 3000 %s, 0%% packet loss, time 60000ms\n' "$received") |
    jq -e '.sent == 3000 and .received == 3000 and .loss_percent == 0' >/dev/null
done
resource_ping_summary <(printf '3000 packets transmitted, 2999 received, 0.03%% packet loss\n') |
  jq -e '.sent == 3000 and .received == 2999 and .loss_percent > 0' >/dev/null
for text in '' 'invalid' '3000 packets transmitted, unknown received, 0% packet loss'; do
  if resource_ping_summary <(printf '%s\n' "$text"); then
    echo 'Missing or malformed packet evidence was accepted.' >&2
    exit 1
  fi
done
if resource_ping_summary <(printf '1 packets transmitted, 1 received, 0%% packet loss\n%.0s' 1 2); then
  echo 'Ambiguous packet summaries were accepted.' >&2
  exit 1
fi
if resource_ping_summary <(printf '%s\n' \
  '1 packets transmitted, 1 received, 0% packet loss' \
  '1 packets transmitted, unknown received, 0% packet loss'); then
  echo 'A malformed extra summary was ignored.' >&2
  exit 1
fi
echo 'Paced ping summary checks passed.'
