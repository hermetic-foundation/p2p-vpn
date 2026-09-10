#!/usr/bin/env bash
# Sourced by the isolated Android E2E harness after two-network admission.
# Harness-owned paths, network IDs and ADB command are supplied by the caller.
# shellcheck disable=SC2154

resource_power_is_background() {
  awk '
    /^  mWakefulness=(Asleep|Dozing)$/ { background = 1 }
    /^  mWakefulnessChanging=false$/ { settled = 1 }
    /^  mHalInteractiveModeEnabled=false$/ { noninteractive = 1 }
    END { exit !(background && settled && noninteractive) }
  ' "$1"
}

resource_sleep_until() {
  local remaining=$(($1 - $(monotonic_millis)))
  if ((remaining > 0)); then
    sleep "$(awk -v ms="$remaining" 'BEGIN { printf "%.3f", ms / 1000 }')"
  fi
}

resource_native_sample() {
  local destination="$1" started response diagnostic finished
  started="$(monotonic_millis)"
  response="$state_dir/resource-native.json"
  diagnostic="$state_dir/resource-diagnostic.json"
  android_automation resource-status >"$response" || return 1
  android_automation diagnostics >"$diagnostic" || return 1
  finished="$(monotonic_millis)"
  jq -ec --argjson started "$started" --argjson finished "$finished" \
    --slurpfile diagnostic "$diagnostic" --arg alpha "$alpha_id" --arg beta "$beta_id" '
    if .schema_version == 1 and .ok and
      ([.value.networks[].id] | sort) == ([$alpha, $beta] | sort) and
      all(.value.networks[]; .phase == "running" and
        any(.counters[]; startswith("queue_queued_packets ")) and
        any(.counters[]; startswith("path_peers_with_supported_path "))) and
      $diagnostic[0].schema_version == 1 and $diagnostic[0].ok and
      $diagnostic[0].value.report.resources.total_pss_kib != null
    then {started_millis:$started, finished_millis:$finished, native:.value,
      diagnostic:($diagnostic[0].value.report | {resources,lifecycle,paths,queue,drops,underlay})}
    else error("Missing or mismatched resource sample") end
  ' "$response" >>"$destination"
}

resource_control_window() (
  local mode="$1" prefix="$2" started deadline sampler="" status=0
  trap 'if [[ -n "$sampler" ]]; then kill "$sampler" 2>/dev/null || true; wait "$sampler" 2>/dev/null || true; fi' EXIT
  resource_native_sample "$prefix-boundary-before.jsonl" || exit 1
  adb_run shell -T sh -s -- "$resource_app_pid" 1 <"$resource_collector" \
    >"$prefix-process-before.jsonl" || exit 1
  sh "$resource_collector" "$resource_emulator_pid" 1 >"$prefix-emulator-before.jsonl" || exit 1
  cat /proc/loadavg >"$prefix-host-load-before.txt"
  started="$(monotonic_millis)"
  deadline=$((started + 60000))
  if [[ "$mode" == on ]]; then
    timeout --signal=TERM --kill-after=2s 75 "${adb[@]}" shell -T sh -s \
      -- "$resource_app_pid" 60 <"$resource_collector" >"$prefix-process.jsonl" &
    sampler=$!
    for index in $(seq 0 11); do
      resource_sleep_until "$((started + index * 5000))"
      resource_native_sample "$prefix-runtime.jsonl" || exit 1
    done
  fi
  resource_sleep_until "$deadline"
  if [[ -n "$sampler" ]]; then
    wait "$sampler" || status=$?
    sampler=""
    [[ "$status" == 0 ]] || exit 1
  fi
  adb_run shell -T sh -s -- "$resource_app_pid" 1 <"$resource_collector" \
    >"$prefix-process-after.jsonl" || exit 1
  sh "$resource_collector" "$resource_emulator_pid" 1 >"$prefix-emulator-after.jsonl" || exit 1
  cat /proc/loadavg >"$prefix-host-load-after.txt"
  resource_native_sample "$prefix-boundary-after.jsonl" || exit 1
  jq -n --arg mode "$mode" --argjson started "$started" \
    --argjson finished "$(monotonic_millis)" \
    '{mode:$mode, started_millis:$started, finished_millis:$finished, requested_window_millis:60000}' \
    >"$prefix-window.json"
  for process in process emulator; do
    jq -es 'length == 2 and .[0].pid == .[1].pid and .[0].start_ticks == .[1].start_ticks and
      .[1].started_uptime_seconds >= (.[0].started_uptime_seconds + 60) and
      .[1].started_uptime_seconds <= (.[0].started_uptime_seconds + 70) and
      .[1].user_ticks >= .[0].user_ticks and .[1].system_ticks >= .[0].system_ticks' \
      "$prefix-$process-before.jsonl" "$prefix-$process-after.jsonl" >/dev/null || exit 1
  done
  if [[ "$mode" == on ]]; then
    jq -es 'length == 60 and ([.[].start_ticks] | unique | length) == 1 and
      (. as $rows | all(range(1; length);
        ($rows[.].started_uptime_seconds - $rows[. - 1].started_uptime_seconds) >= 1 and
        ($rows[.].started_uptime_seconds - $rows[. - 1].started_uptime_seconds) <= 1.5))' \
      "$prefix-process.jsonl" >/dev/null || exit 1
    jq -es 'length == 12 and all(.[]; (.finished_millis - .started_millis) <= 2000) and
      (. as $rows | all(range(1; length);
        ($rows[.].started_millis - $rows[. - 1].started_millis) >= 4500 and
        ($rows[.].started_millis - $rows[. - 1].started_millis) <= 5500))' \
      "$prefix-runtime.jsonl" >/dev/null || exit 1
  fi
)

run_android_resource_controls() {
  local resource_collector="${P2P_VPN_ANDROID_PROCESS_COLLECTOR:-$(dirname "$0")/android-process-sample.sh}"
  local resource_app_pid resource_emulator_pid index=0 mode
  adb_run root >"$output_dir/resource-root.txt" || return 1
  adb_run wait-for-device || return 1
  [[ "$(adb_run shell id -u | tr -d '\r')" == 0 ]] || return 1
  resource_app_pid="$(adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r')"
  resource_emulator_pid="$(pgrep -f '[q]emu-system-x86_64.*-avd p2p-vpn')"
  [[ "$resource_app_pid" =~ ^[1-9][0-9]*$ && "$resource_emulator_pid" =~ ^[1-9][0-9]*$ ]] || return 1
  adb_run shell getconf CLK_TCK >"$output_dir/resource-guest-clock.txt" || return 1
  getconf CLK_TCK >"$output_dir/resource-host-clock.txt" || return 1
  adb_run shell input keyevent KEYCODE_HOME || return 1
  adb_run shell input keyevent KEYCODE_SLEEP || return 1
  sleep 30
  adb_run shell dumpsys power >"$output_dir/resource-power.txt" || return 1
  resource_power_is_background "$output_dir/resource-power.txt" || return 1
  for mode in off on on off; do
    index=$((index + 1))
    record_step "resource_control_$index" started "Collector $mode; fixed 60-second idle window"
    resource_control_window "$mode" "$output_dir/control-$index-$mode" || return 1
    record_step "resource_control_$index" passed "Bounded process and runtime observations verified"
  done
}
