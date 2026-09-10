#!/usr/bin/env bash
# The sourced helper consumes the fixture globals and invokes these test stubs.
# shellcheck disable=SC2034,SC2329
set -euo pipefail
controls="${P2P_VPN_ANDROID_RESOURCE_CONTROLS:-$(dirname "$0")/../scripts/android-resource-controls.sh}"
# shellcheck disable=SC1090
source "$controls"
state_dir=$(mktemp -d)
trap 'rm -rf "$state_dir"' EXIT
alpha_id=alpha beta_id=beta
state='{"schema_version":1,"ok":true,"value":{"service_ready":true,"snapshot":{"connected":true,"busy":false,"networks":[{"id":"alpha","enabled":true,"phase":"running"},{"id":"beta","enabled":true,"phase":"running"}]}}}'
resource_isolation_state_valid <(printf '%s\n' "$state") true
disabled=$(jq '.value.snapshot.networks[0] |= (.enabled=false|.phase="disabled")' <<<"$state")
resource_isolation_state_valid <(printf '%s\n' "$disabled") false
for filter in '.value.snapshot.connected=false' '.value.snapshot.busy=true' \
  '.value.snapshot.networks[1].enabled=false' '.value.snapshot.networks[1].phase="starting"' \
  '.value.snapshot.networks[1].id="alpha"' '.value.snapshot.networks|=.[0:1]' \
  '.value.service_ready=false' '.ok=false'; do
  if resource_isolation_state_valid <(jq "$filter" <<<"$state") true; then
    echo "Invalid isolation state accepted: $filter" >&2
    exit 1
  fi
done
if resource_isolation_state_valid <(printf '%s\n' "$state") false; then exit 1; fi
accept=true identity_ok=true status_calls=0
android_automation() {
  case "$1" in
    set-network-enabled)
      [[ "$*" == 'set-network-enabled --es network_id alpha --ez enabled false' ]]
      jq -nc --argjson accepted "$accept" '{schema_version:1,ok:true,value:{accepted:$accepted,command:"set-network-enabled"}}'
      ;;
    status)
      status_calls=$((status_calls + 1))
      printf '%s\n' "$returned_state"
      ;;
    *) return 1 ;;
  esac
}
network_identity_signature_matches() { [[ "$identity_ok" == true ]]; }
sleep() { :; }
returned_state=$disabled
resource_isolation_set_alpha false "$state_dir/passed" 3
[[ "$status_calls" == 1 ]]
jq -e '.networks[0].enabled==false and .networks[1].enabled' "$state_dir/passed-state.json" >/dev/null
identity_ok=false
if resource_isolation_set_alpha false "$state_dir/identity" 3; then exit 1; fi
identity_ok=true returned_state=$state status_calls=0
if resource_isolation_set_alpha false "$state_dir/timeout" 3; then exit 1; fi
[[ "$status_calls" == 3 ]]
accept=false status_calls=0
if resource_isolation_set_alpha false "$state_dir/rejected" 3; then exit 1; fi
[[ "$status_calls" == 0 ]]
native_ok=true native_phase=running native_filter='.'
monotonic_millis() { printf '1000\n'; }
android_automation() {
  case "$1" in
    status) printf '%s\n' "$state" ;;
    resource-status)
      jq -nc --argjson ok "$native_ok" --arg phase "$native_phase" \
        '{schema_version:1,ok:$ok,value:{phase:$phase,networks:(if $phase=="stopped" then [] else [{id:"beta",phase:$phase,counters:["queue_queued_packets 0","path_peers_with_supported_path 1"]}] end)}}' | jq "$native_filter"
      ;;
    diagnostics) printf '%s\n' '{"schema_version":1,"ok":true,"value":{"report":{"resources":{"total_pss_kib":42},"secret":"not-for-output"}}}' ;;
    *) return 1 ;;
  esac
}
resource_isolation_observation "$state_dir/observation.jsonl"
jq -e '.native.networks[0].id=="beta" and .diagnostic.resources.total_pss_kib==42 and (.diagnostic|has("secret")|not)' "$state_dir/observation.jsonl" >/dev/null
native_ok=false
if resource_isolation_observation "$state_dir/missing.jsonl"; then exit 1; fi
jq -e '.valid==false' "$state_dir/missing.jsonl" >/dev/null
native_ok=true
for native_phase in stopped starting; do
  resource_isolation_observation "$state_dir/$native_phase.jsonl"
  jq -e --arg phase "$native_phase" '.valid and .native.phase==$phase' "$state_dir/$native_phase.jsonl" >/dev/null
done
native_phase=failed
if resource_isolation_observation "$state_dir/failed.jsonl"; then exit 1; fi
native_phase=running
for native_filter in '.value.networks=[]' '.value.networks[0].id="unknown"' '.value.networks[0].counters=[]'; do
  if resource_isolation_observation "$state_dir/invalid.jsonl"; then exit 1; fi
done
echo 'Isolation role states, identity rejection and bounded transition polling passed.'
