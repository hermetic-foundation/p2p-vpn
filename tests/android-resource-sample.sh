#!/usr/bin/env bash
set -euo pipefail
controls="${P2P_VPN_ANDROID_RESOURCE_CONTROLS:-$(dirname "$0")/../scripts/android-resource-controls.sh}"
# shellcheck disable=SC1090
source "$controls"
state_dir=$(mktemp -d)
trap 'rm -rf "$state_dir"' EXIT
# Consumed by the sourced sampler.
# shellcheck disable=SC2034
alpha_id=alpha beta_id=beta mode=valid
monotonic_millis() { echo 100; }
android_automation() {
  if [[ "$1" == resource-status ]]; then
    jq -n --arg mode "$mode" '{schema_version:1,ok:true,value:{networks:
      ["alpha","beta"] | map({id:(if $mode == "identity" then "other" else . end),phase:(if $mode == "phase" then "failed" else "running" end),
      counters:(if $mode == "missing" then [] else ["queue_queued_packets 0","path_peers_with_supported_path 1"] end)})}}'
  else
    jq -n --arg mode "$mode" '{schema_version:1,ok:true,value:{report:{resources:{
      total_pss_kib:(if $mode == "diagnostic" then null else 100 end)}}}}'
  fi
}
resource_native_sample "$state_dir/sample.jsonl"
jq -es 'length == 1 and .[0].native.networks[0].counters[0] == "queue_queued_packets 0"' "$state_dir/sample.jsonl" >/dev/null
for mode in identity phase missing diagnostic; do
  if resource_native_sample "$state_dir/rejected-$mode.jsonl"; then
    echo "Invalid $mode sample was accepted." >&2
    exit 1
  fi
  [[ ! -s "$state_dir/rejected-$mode.jsonl" ]]
done
echo 'Resource sample identity, phase and missing-data checks passed.'
