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

resource_isolation_state_valid() {
  jq -e --arg alpha "$alpha_id" --arg beta "$beta_id" --argjson enabled "$2" '
    .schema_version == 1 and .ok and .value.service_ready and
    .value.snapshot.connected and (.value.snapshot.busy | not) and
    ([.value.snapshot.networks[].id]|sort) == ([$alpha,$beta]|sort) and
    all(.value.snapshot.networks[];
      if .id == $alpha then .enabled == $enabled and .phase == (if $enabled then "running" else "disabled" end)
      else .enabled and .phase == "running" end)
  ' "$1" >/dev/null
}

resource_isolation_observation() {
  local destination="$1" started finished
  local control="$state_dir/isolation-observer-control.json"
  local native="$state_dir/isolation-observer-native.json"
  local diagnostic="$state_dir/isolation-observer-diagnostic.json"
  started="$(monotonic_millis)"
  android_automation status >"$control" || return 1
  android_automation resource-status >"$native" || return 1
  android_automation diagnostics >"$diagnostic" || return 1
  finished="$(monotonic_millis)"
  jq -nc --argjson started "$started" --argjson finished "$finished" --arg alpha "$alpha_id" --arg beta "$beta_id" \
    --slurpfile control "$control" --slurpfile native "$native" --slurpfile diagnostic "$diagnostic" '
    (all([$control[0],$native[0],$diagnostic[0]][]; .schema_version == 1 and .ok == true) and
      $control[0].value.service_ready and $diagnostic[0].value.report.resources.total_pss_kib != null and
      ([$control[0].value.snapshot.networks[]?.id]|sort)==([$alpha,$beta]|sort) and
      any($control[0].value.snapshot.networks[]?; .id==$beta and .enabled) and
      ($native[0].value.networks|type)=="array" and
      all($native[0].value.networks[]?; .id==$alpha or .id==$beta) and
      ([$native[0].value.networks[]?.id]|unique|length)==($native[0].value.networks|length) and
      (if $native[0].value.phase=="stopped" then ($native[0].value.networks|length)==0
       elif $native[0].value.phase=="starting" then
         all($native[0].value.networks[]?; .phase=="starting" or .phase=="running")
       elif $native[0].value.phase=="running" then
         ([$native[0].value.networks[]? | select(.id==$beta and .phase=="running" and
           any(.counters[]?; startswith("queue_queued_packets ")) and
           any(.counters[]?; startswith("path_peers_with_supported_path ")))]|length)==1
       else false end)) as $valid |
    {valid:$valid,started_millis:$started,finished_millis:$finished,
      control:($control[0].value.snapshot | {connected,busy,runtime_generation,paths,
        networks:[.networks[]? | {id,name,hostname,peer_id,addresses,enabled,phase}]}),
      native:$native[0].value,
      diagnostic:($diagnostic[0].value.report | {resources,lifecycle,paths,queue,drops,underlay})}
  ' >"$state_dir/isolation-observer-row.json" || return 1
  cat "$state_dir/isolation-observer-row.json" >>"$destination" || return 1
  jq -e '.valid' "$state_dir/isolation-observer-row.json" >/dev/null
}

resource_isolation_set_alpha() {
  local enabled="$1" prefix="$2" attempts="$3"
  local command="$state_dir/isolation-command.json" status="$state_dir/isolation-status.json"
  android_automation set-network-enabled --es network_id "$alpha_id" --ez enabled "$enabled" >"$command" || return 1
  jq -e '.schema_version == 1 and .ok and .value.accepted and .value.command == "set-network-enabled"' "$command" >/dev/null || return 1
  local attempt
  for ((attempt = 0; attempt < attempts; attempt++)); do
    if android_automation status >"$status" && resource_isolation_state_valid "$status" "$enabled"; then
      network_identity_signature_matches "$status" || return 1
      jq '.value.snapshot | {connected,runtime_generation,networks:[.networks[] | {id,name,hostname,peer_id,addresses,enabled,phase}]}' \
        "$status" >"$prefix-state.json" || return 1
      return 0
    fi
    sleep 1
  done
  return 1
}

run_android_resource_isolation_cycles() (
  local sampler="" observer="" worker cycle prefix family source destination command result started duration file
  trap 'for worker in "$sampler" "$observer"; do [[ -z "$worker" ]] || { kill "$worker" 2>/dev/null || true; wait "$worker" 2>/dev/null || true; }; done' EXIT
  jq '.value.snapshot.networks | map({id,name,hostname,peer_id,addresses}) | sort_by(.id)' \
    "$both_running" >"$state_dir/multi-network-identity-signature.json" || exit 1
  adb_run shell -T sh -s -- "$resource_app_pid" 1 <"$resource_collector" >"$output_dir/isolation-process-before.jsonl" || exit 1
  sh "$resource_collector" "$resource_emulator_pid" 1 >"$output_dir/isolation-emulator-before.jsonl" || exit 1
  timeout --signal=TERM --kill-after=2s 900 "${adb[@]}" shell -T sh -s -- "$resource_app_pid" 900 \
    <"$resource_collector" >"$output_dir/isolation-process.jsonl" &
  sampler=$!
  (
    first="$(monotonic_millis)"
    for sample in $(seq 0 179); do
      resource_sleep_until "$((first + sample * 5000))"
      [[ ! -e "$state_dir/isolation-observer-stop" ]] || exit 0
      resource_isolation_observation "$output_dir/isolation-runtime.jsonl" || exit 1
    done
  ) &
  observer=$!
  for cycle in $(seq 1 5); do
    prefix="$output_dir/isolation-$cycle"
    record_step "isolation_$cycle" started "Disable alpha, retain beta, reject disabled traffic, restore alpha"
    record_step "isolation_${cycle}_disable" started "Shared runtime reconfiguration requested"
    resource_isolation_set_alpha false "$prefix-disabled" 120 || exit 1
    wait_for_transition_traffic_ready "isolation-$cycle-beta" "after disabling alpha" \
      "$fixture_secondary_packet_socket" "$fixture_secondary_ipv4" "$fixture_secondary_ipv6" \
      "$android_secondary_ipv4" "$android_secondary_ipv6" || exit 1
    record_step "isolation_${cycle}_disable" passed "Disabled state and surviving beta traffic readiness verified"
    result=0
    measure_bidirectional_traffic "isolation-$cycle-beta" "while alpha is disabled" \
      "$fixture_secondary_packet_socket" "$fixture_secondary_ipv4" "$fixture_secondary_ipv6" \
      "$android_secondary_ipv4" "$android_secondary_ipv6" || result=$?
    for file in "$state_dir/isolation-$cycle-beta-"*; do
      [[ ! -f "$file" ]] || cp -- "$file" "$output_dir/" || exit 1
    done
    [[ "$result" == 0 ]] || exit 1
    for family in ipv4 ipv6; do
      source="$fixture_ipv4" destination="$android_primary_ipv4" command=ping
      [[ "$family" != ipv6 ]] || { source="$fixture_ipv6" destination="$android_primary_ipv6" command=ping6; }
      result=0
      "$fixture_command" probe --socket "$fixture_packet_socket" --source "$source" --destination "$destination" \
        --count 1 --timeout-millis 2000 >"$prefix-disabled-$family-inbound.json" 2>"$prefix-disabled-$family-inbound-error.txt" || result=$?
      [[ "$result" != 0 ]] || exit 1
      jq -e --arg family "$family" '.schema_version==1 and (.ok|not) and .family==$family and .sent==1 and .received==0' \
        "$prefix-disabled-$family-inbound.json" >/dev/null || exit 1
      if adb_run shell "$command" -c 1 -W 2 "$source" >"$prefix-disabled-$family-outbound.txt" 2>&1; then exit 1; fi
      grep -Eq 'Network is unreachable|1 packets transmitted, 0 (packets )?received' "$prefix-disabled-$family-outbound.txt" || exit 1
    done
    record_step "isolation_${cycle}_enable" started "Shared runtime reconfiguration requested"
    resource_isolation_set_alpha true "$prefix-enabled" 180 || exit 1
    wait_for_multi_network_transition_traffic_ready "isolation-$cycle-restored" "after alpha re-enabled" || exit 1
    record_step "isolation_${cycle}_enable" passed "Enabled state and both networks traffic readiness verified"
    result=0
    measure_concurrent_multi_network_traffic "isolation-$cycle-restored" "after alpha re-enabled" started duration || result=$?
    for file in "$state_dir/isolation-$cycle-restored-"*; do
      [[ ! -f "$file" ]] || cp -- "$file" "$output_dir/" || exit 1
    done
    [[ "$result" == 0 ]] || exit 1
    kill -0 "$sampler" && kill -0 "$observer" || exit 1
    record_step "isolation_$cycle" passed "Both families isolated; beta and restored alpha passed measured traffic"
  done
  record_step isolation_settle started "60-second settling with existing collectors and no deliberate traffic"
  resource_sleep_until "$(($(monotonic_millis) + 60000))"
  record_step isolation_settle passed "Fixed settling interval finished"
  kill -0 "$sampler" && kill -0 "$observer" || exit 1
  : >"$state_dir/isolation-observer-stop"
  wait "$observer" || exit 1
  observer=""
  kill -0 "$sampler" || exit 1
  kill "$sampler" || exit 1
  wait "$sampler" 2>/dev/null || true
  sampler=""
  adb_run shell -T sh -s -- "$resource_app_pid" 1 <"$resource_collector" >"$output_dir/isolation-process-after.jsonl" || exit 1
  sh "$resource_collector" "$resource_emulator_pid" 1 >"$output_dir/isolation-emulator-after.jsonl" || exit 1
  jq -es 'length>=2 and ([.[].pid]|unique|length)==1 and ([.[].start_ticks]|unique|length)==1 and
    (. as $r | all(range(1;length); ($r[.].started_uptime_seconds-$r[.-1].started_uptime_seconds)>=1 and
      ($r[.].started_uptime_seconds-$r[.-1].started_uptime_seconds)<=1.5))' "$output_dir/isolation-process.jsonl" >/dev/null || exit 1
  jq -es 'length>=2 and all(.[]; .valid and (.finished_millis-.started_millis)<=2000) and
    (. as $r | all(range(1;length); ($r[.].started_millis-$r[.-1].started_millis)>=4500 and
      ($r[.].started_millis-$r[.-1].started_millis)<=5500))' "$output_dir/isolation-runtime.jsonl" >/dev/null || exit 1
  jq -es --arg beta "$beta_id" --slurpfile signature "$state_dir/multi-network-identity-signature.json" '
    all(.[];
      (.control.networks|map({id,name,hostname,peer_id,addresses})|sort_by(.id))==$signature[0] and
      any(.control.networks[]; .id==$beta and .enabled))
  ' "$output_dir/isolation-runtime.jsonl" >/dev/null || exit 1
  jq -nes --slurpfile before "$output_dir/isolation-process-before.jsonl" \
    --slurpfile after "$output_dir/isolation-process-after.jsonl" \
    --slurpfile samples "$output_dir/isolation-process.jsonl" '
    $before[0].pid==$after[0].pid and $before[0].start_ticks==$after[0].start_ticks and
    all($samples[]; .pid==$before[0].pid and .start_ticks==$before[0].start_ticks) and
    ($samples[0].started_uptime_seconds-$before[0].started_uptime_seconds)>=0 and
    ($samples[0].started_uptime_seconds-$before[0].started_uptime_seconds)<=2 and
    ($after[0].started_uptime_seconds-$samples[-1].started_uptime_seconds)>=0 and
    ($after[0].started_uptime_seconds-$samples[-1].started_uptime_seconds)<=2
  ' >/dev/null || exit 1
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

resource_profile_load() (
  local binary="$1" guest="" profiler="" status=0 size
  [[ -f "$binary" ]] || exit 2
  guest="$(adb_run shell mktemp -d /data/local/tmp/p2p-vpn-profile.XXXXXX | tr -d '\r')"
  [[ "$guest" =~ ^/data/local/tmp/p2p-vpn-profile\.[a-zA-Z0-9]+$ ]] || exit 1
  trap 'if [[ -n "$profiler" ]]; then kill "$profiler" 2>/dev/null || true; wait "$profiler" 2>/dev/null || true; fi; adb_run shell rm -rf "$guest" >/dev/null 2>&1 || true' EXIT
  adb_run push "$binary" "$guest/simpleperf" >"$output_dir/profile-push.txt" || exit 1
  adb_run shell chmod 500 "$guest/simpleperf" || exit 1
  timeout --signal=TERM --kill-after=2s 75 "${adb[@]}" shell "$guest/simpleperf" record \
    -p "$resource_app_pid" -e cpu-clock:u -f 99 --duration 60 --no-inherit \
    -m 64 --user-buffer-size 1M --size-limit 8M --no-dump-symbols \
    --no-dump-kernel-symbols --exit-with-parent -o "$guest/perf.data" \
    >"$output_dir/profile-record.txt" 2>&1 &
  profiler=$!
  resource_control_window on "$output_dir/load-smoke" 60 load || status=1
  wait "$profiler" || status=1
  profiler=""
  adb_run pull "$guest/perf.data" "$output_dir/profile.data" >"$output_dir/profile-pull.txt" 2>&1 || status=1
  [[ "$status" == 0 ]] || exit 1
  size="$(stat -c %s "$output_dir/profile.data")" || exit 1
  [[ "$size" -gt 0 && "$size" -lt 8388608 ]] || exit 1
)

run_android_sustained_load() {
  local detail="${1:-process}" phase duration traffic
  [[ "$detail" == process || "$detail" == threads ]] || return 2
  for phase in idle load drain; do
    duration=300 traffic=idle
    [[ "$phase" != load ]] || traffic=load
    [[ "$phase" != drain ]] || duration=60
    record_step "sustained_$phase" started "Fixed $duration-second phase with $traffic traffic; $detail sampling"
    resource_control_window on "$output_dir/sustained-$phase" "$duration" "$traffic" "$detail" || return 1
    record_step "sustained_$phase" passed "Phase observations and traffic checks passed"
  done
}

run_android_resource_controls() {
  local resource_collector="${P2P_VPN_ANDROID_PROCESS_COLLECTOR:-$(dirname "$0")/android-process-sample.sh}"
  local resource_app_pid resource_emulator_pid index=0 mode detail
  local load_detail="${P2P_VPN_ANDROID_RESOURCE_LOAD_DETAIL:-process}"
  local profile_binary="${P2P_VPN_ANDROID_SIMPLEPERF_BINARY:-}"
  [[ "$load_detail" == process || "$load_detail" == threads ]] || return 2
  [[ "$load_detail" == process || "${scenario:-}" == multi-network-resource-load ]] || return 2
  [[ -z "$profile_binary" || ("${scenario:-}" == multi-network-resource-load-smoke && -f "$profile_binary") ]] || return 2
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
  if [[ "${scenario:-}" == multi-network-resource-isolation ]]; then
    run_android_resource_isolation_cycles
    return $?
  fi
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
    if [[ -n "$profile_binary" ]]; then
      resource_profile_load "$profile_binary" || return 1
    else
      resource_control_window on "$output_dir/load-smoke" 60 load || return 1
    fi
    record_step load_smoke passed "Paced traffic and resource collection verified; not S7 acceptance"
    return 0
  fi
  if [[ "${scenario:-}" == multi-network-resource-load ]]; then
    run_android_sustained_load "$load_detail"
    return $?
  fi
  for mode in off on on off; do
    index=$((index + 1))
    record_step "resource_control_$index" started "Collector $mode; fixed 60-second idle window"
    resource_control_window "$mode" "$output_dir/control-$index-$mode" || return 1
    record_step "resource_control_$index" passed "Bounded process and runtime observations verified"
  done
}
