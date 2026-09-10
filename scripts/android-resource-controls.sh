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

resource_ping_summary() {
  jq -Rsec '
    [split("\n")[] | select(test("packets transmitted"))] |
    if length != 1 then error("Missing or ambiguous ping summary") else .[0] end |
    [
      capture("^(?<sent>[0-9]+) packets transmitted, (?<received>[0-9]+)( packets)? received, (?<loss>[0-9.]+)% packet loss") |
      {sent:(.sent|tonumber),received:(.received|tonumber),loss_percent:(.loss|tonumber)}] |
    if length == 1 then .[0] else error("Missing or ambiguous ping summary") end
  ' "$1"
}

resource_ping_leg() (
  local prefix="$1" command="$2" destination="$3" duration="$4" started finished status=0 child=""
  trap 'if [[ -n "$child" ]]; then kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; fi' EXIT
  started="$(monotonic_millis)"
  timeout --signal=TERM --kill-after=2s "$((duration + 15))" "${adb[@]}" shell -T \
    "$command" -n -q -i 0.02 -s 512 -c "$((duration * 50))" -W 1 "$destination" \
    >"$prefix.txt" 2>&1 &
  child=$!
  wait "$child" || status=$?
  child=""
  finished="$(monotonic_millis)"
  resource_ping_summary "$prefix.txt" >"$prefix-counts.json" || return 1
  jq -n --argjson started "$started" --argjson finished "$finished" --argjson status "$status" \
    --argjson duration "$duration" --slurpfile counts "$prefix-counts.json" \
    '{started_millis:$started,finished_millis:$finished,status:$status,
      requested_packets:($duration*50),interval_seconds:0.02,payload_bytes:512,counts:$counts[0]}' \
    >"$prefix.json"
  jq -e --argjson duration "$duration" '.status == 0 and
    .counts.sent == ($duration*50) and .counts.received == .counts.sent and .counts.loss_percent == 0 and
    (.finished_millis-.started_millis) >= (($duration-1)*1000) and
    (.finished_millis-.started_millis) <= (($duration+10)*1000)' "$prefix.json" >/dev/null
)

resource_thread_samples_valid() {
  jq -es 'length > 0 and all(.[];
    .thread_scan.listed >= 1 and .thread_scan.listed <= 256 and
    .thread_scan.observed == .thread_scan.listed and .thread_scan.skipped == 0 and
    .thread_scan.observed == (.thread_scan.threads | length) and
    ([.thread_scan.threads[].tid] | unique | length) == .thread_scan.observed and
    all(.thread_scan.threads[]; .tid > 0 and .start_ticks > 0 and
      .user_ticks != null and .user_ticks >= 0 and .system_ticks != null and .system_ticks >= 0 and
      .voluntary_context_switches != null and .voluntary_context_switches >= 0 and
      .involuntary_context_switches != null and .involuntary_context_switches >= 0))' "$1" >/dev/null
}

resource_control_window() (
  local mode="$1" prefix="$2" duration="${3:-60}" started deadline sampler="" status=0
  local traffic="${4:-idle}" worker
  local detail="${5:-process}"
  local -a collector_arguments=()
  local -a traffic_workers=()
  [[ "$duration" == 60 || "$duration" == 300 ]] || exit 2
  [[ "$mode" == on || "$mode" == off ]] || exit 2
  [[ "$traffic" == idle || ("$traffic" == load && "$mode" == on) ]] || exit 2
  [[ "$detail" == process || ("$detail" == threads && "$mode" == on) ]] || exit 2
  [[ "$detail" != threads ]] || collector_arguments+=(--threads)
  trap 'for worker in "${traffic_workers[@]}" "$sampler"; do [[ -z "$worker" ]] || { kill "$worker" 2>/dev/null || true; wait "$worker" 2>/dev/null || true; }; done' EXIT
  resource_native_sample "$prefix-boundary-before.jsonl" || exit 1
  adb_run shell -T sh -s -- "$resource_app_pid" 1 <"$resource_collector" \
    >"$prefix-process-before.jsonl" || exit 1
  sh "$resource_collector" "$resource_emulator_pid" 1 >"$prefix-emulator-before.jsonl" || exit 1
  cat /proc/loadavg >"$prefix-host-load-before.txt"
  started="$(monotonic_millis)"
  deadline=$((started + duration * 1000))
  if [[ "$traffic" == load ]]; then
    resource_ping_leg "$prefix-alpha-ipv4" ping "$fixture_ipv4" "$duration" &
    traffic_workers+=("$!")
    resource_ping_leg "$prefix-alpha-ipv6" ping6 "$fixture_ipv6" "$duration" &
    traffic_workers+=("$!")
    resource_ping_leg "$prefix-beta-ipv4" ping "$fixture_secondary_ipv4" "$duration" &
    traffic_workers+=("$!")
    resource_ping_leg "$prefix-beta-ipv6" ping6 "$fixture_secondary_ipv6" "$duration" &
    traffic_workers+=("$!")
  fi
  if [[ "$mode" == on ]]; then
    timeout --signal=TERM --kill-after=2s "$((duration + 15))" "${adb[@]}" shell -T sh -s \
      -- "$resource_app_pid" "$duration" "${collector_arguments[@]}" <"$resource_collector" >"$prefix-process.jsonl" &
    sampler=$!
    for index in $(seq 0 "$((duration / 5 - 1))"); do
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
  for worker in "${traffic_workers[@]}"; do
    wait "$worker" || status=$?
  done
  traffic_workers=()
  [[ "$status" == 0 ]] || exit 1
  adb_run shell -T sh -s -- "$resource_app_pid" 1 <"$resource_collector" \
    >"$prefix-process-after.jsonl" || exit 1
  sh "$resource_collector" "$resource_emulator_pid" 1 >"$prefix-emulator-after.jsonl" || exit 1
  cat /proc/loadavg >"$prefix-host-load-after.txt"
  resource_native_sample "$prefix-boundary-after.jsonl" || exit 1
  jq -n --arg mode "$mode" --argjson started "$started" \
    --arg detail "$detail" \
    --argjson duration "$duration" \
    --argjson finished "$(monotonic_millis)" \
    '{mode:$mode, detail:$detail, started_millis:$started, finished_millis:$finished, requested_window_millis:($duration * 1000)}' \
    >"$prefix-window.json"
  for process in process emulator; do
    jq -es --argjson duration "$duration" 'length == 2 and .[0].pid == .[1].pid and .[0].start_ticks == .[1].start_ticks and
      .[1].started_uptime_seconds >= (.[0].started_uptime_seconds + $duration) and
      .[1].started_uptime_seconds <= (.[0].started_uptime_seconds + $duration + 10) and
      .[1].user_ticks >= .[0].user_ticks and .[1].system_ticks >= .[0].system_ticks' \
      "$prefix-$process-before.jsonl" "$prefix-$process-after.jsonl" >/dev/null || exit 1
  done
  if [[ "$mode" == on ]]; then
    jq -es --argjson duration "$duration" 'length == $duration and ([.[].start_ticks] | unique | length) == 1 and
      (. as $rows | all(range(1; length);
        ($rows[.].started_uptime_seconds - $rows[. - 1].started_uptime_seconds) >= 1 and
        ($rows[.].started_uptime_seconds - $rows[. - 1].started_uptime_seconds) <= 1.5))' \
      "$prefix-process.jsonl" >/dev/null || exit 1
    jq -es --argjson duration "$duration" 'length == ($duration / 5) and all(.[]; (.finished_millis - .started_millis) <= 2000) and
      (. as $rows | all(range(1; length);
        ($rows[.].started_millis - $rows[. - 1].started_millis) >= 4500 and
        ($rows[.].started_millis - $rows[. - 1].started_millis) <= 5500))' \
      "$prefix-runtime.jsonl" >/dev/null || exit 1
    if [[ "$detail" == threads ]]; then
      resource_thread_samples_valid "$prefix-process.jsonl" || exit 1
    fi
  fi
)

run_android_resource_controls() {
  local resource_collector="${P2P_VPN_ANDROID_PROCESS_COLLECTOR:-$(dirname "$0")/android-process-sample.sh}"
  local resource_app_pid resource_emulator_pid index=0 mode phase duration traffic detail
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
  if [[ "${scenario:-}" == multi-network-resource-thread-controls ]]; then
    for detail in process threads threads process; do
      index=$((index + 1))
      record_step "thread_control_$index" started "Fixed 60-second idle window; $detail sampling"
      resource_control_window on "$output_dir/thread-control-$index-$detail" 60 idle "$detail" || return 1
      record_step "thread_control_$index" passed "Process, runtime and selected thread observations verified"
    done
    return 0
  fi
  if [[ "${scenario:-}" == multi-network-resource-idle ]]; then
    record_step sustained_idle started "Two background networks; fixed 300-second idle window"
    resource_control_window on "$output_dir/sustained-idle" 300 || return 1
    record_step sustained_idle passed "300 process and 60 runtime observations verified"
    return 0
  fi
  if [[ "${scenario:-}" == multi-network-resource-load-smoke ]]; then
    record_step load_smoke started "Four paced streams; 60-second compatibility window"
    resource_control_window on "$output_dir/load-smoke" 60 load || return 1
    record_step load_smoke passed "Paced traffic and resource collection verified; not S7 acceptance"
    return 0
  fi
  if [[ "${scenario:-}" == multi-network-resource-load ]]; then
    for phase in idle load drain; do
      duration=300 traffic=idle
      [[ "$phase" != load ]] || traffic=load
      [[ "$phase" != drain ]] || duration=60
      record_step "sustained_$phase" started "Fixed $duration-second phase with $traffic traffic"
      resource_control_window on "$output_dir/sustained-$phase" "$duration" "$traffic" || return 1
      record_step "sustained_$phase" passed "Phase observations and traffic checks passed"
    done
    return 0
  fi
  for mode in off on on off; do
    index=$((index + 1))
    record_step "resource_control_$index" started "Collector $mode; fixed 60-second idle window"
    resource_control_window "$mode" "$output_dir/control-$index-$mode" || return 1
    record_step "resource_control_$index" passed "Bounded process and runtime observations verified"
  done
}
