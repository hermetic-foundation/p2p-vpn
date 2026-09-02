#!/usr/bin/env bash
# shellcheck disable=SC2329
set -euo pipefail

umask 077

readonly evidence_schema_version=1
readonly maximum_log_bytes=$((1024 * 1024))
readonly default_minimum_free_bytes=$((16 * 1024 * 1024 * 1024))
readonly default_maximum_runtime_growth_bytes=$((8 * 1024 * 1024 * 1024))
readonly hard_maximum_runtime_growth_bytes=$((32 * 1024 * 1024 * 1024))
readonly default_adb_timeout_seconds=120
readonly cleanup_adb_timeout_seconds=5

scenario=boot-smoke
path_mode=automatic
preflight_only=0
allow_skip=0
output_dir="${P2P_VPN_ANDROID_E2E_DIR:-}"
minimum_free_bytes="${P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES:-$default_minimum_free_bytes}"
maximum_runtime_growth_bytes="${P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES:-$default_maximum_runtime_growth_bytes}"
started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
outcome=running
outcome_detail="E2E harness exited before recording a result"
evidence_finalized=0
emulator_pid=""
fixture_pid=""
fixture_secondary_pid=""
runtime_storage_watchdog_pid=""
emulator_serial=""
state_dir=""
runtime_storage_failure_file=""
runtime_tmp_baseline_available_bytes=""
runtime_output_baseline_available_bytes=""
adb=()
adb_timeout_seconds="${P2P_VPN_ANDROID_E2E_ADB_TIMEOUT_SECONDS:-$default_adb_timeout_seconds}"
cleanup_emulator_stopped=false
cleanup_fixture_stopped=false
cleanup_private_state_removed=false
cleanup_logs_redacted=false
cleanup_diagnostic_report_redacted=true
cleanup_always_on_cleared=true
diagnostic_report_required=false
always_on_configured=false
readonly harness_pid="$BASHPID"

adb_run() {
  timeout --signal=TERM --kill-after=2s "${adb_timeout_seconds}s" "${adb[@]}" "$@"
}

usage() {
  cat <<'EOF'
Usage: p2p-vpn-android-e2e [OPTIONS]

Options:
  --scenario NAME        Select boot-smoke, profile-persistence, always-on,
                         pairing-traffic, underlay-recovery, network-workflow,
                         or multi-network.
  --path-mode MODE       Select automatic, quic-stream, tcp-stream, owned-quic, relay-only,
                         or relay-to-direct.
  --preflight            Check requirements without starting an emulator.
  --allow-skip           Exit 77 instead of 2 when requirements are unavailable.
  --output DIRECTORY     Write bounded evidence to DIRECTORY.
  -h, --help             Show this help.

Environment:
  P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES
                         Required runtime free space; defaults to 16 GiB.
  P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES
                         Runtime growth limit; defaults to 8 GiB and cannot exceed 32 GiB.
  P2P_VPN_ANDROID_E2E_ADB_TIMEOUT_SECONDS
                         Per-command ADB limit; defaults to 120 seconds.

Exit codes:
  0   Scenario passed.
  2   Usage error or a required host capability is unavailable.
  75  Storage budget exhausted.
  77  Scenario skipped after an explicit preflight or --allow-skip.
  1   Scenario ran and failed.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --scenario)
      [[ $# -ge 2 ]] || {
        echo "--scenario requires a value" >&2
        exit 2
      }
      scenario="$2"
      shift 2
      ;;
    --preflight)
      preflight_only=1
      shift
      ;;
    --path-mode)
      [[ $# -ge 2 ]] || {
        echo "--path-mode requires a value" >&2
        exit 2
      }
      path_mode="$2"
      shift 2
      ;;
    --allow-skip)
      allow_skip=1
      shift
      ;;
    --output)
      [[ $# -ge 2 ]] || {
        echo "--output requires a value" >&2
        exit 2
      }
      output_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

pairing_scenario=0
case "$scenario" in
  boot-smoke|profile-persistence|always-on) ;;
  pairing-traffic|underlay-recovery|network-workflow|multi-network) pairing_scenario=1 ;;
  *)
    echo "unsupported Android E2E scenario: $scenario" >&2
    exit 2
    ;;
esac

case "$path_mode" in
  automatic|quic-stream|tcp-stream|owned-quic|relay-only|relay-to-direct) ;;
  *)
    echo "unsupported Android E2E path mode: $path_mode" >&2
    exit 2
    ;;
esac
if [[ "$pairing_scenario" -eq 0 && "$path_mode" != automatic ]]; then
  echo "--path-mode is supported only by pairing scenarios" >&2
  exit 2
fi
if [[ "$scenario" == underlay-recovery && "$path_mode" != automatic ]]; then
  echo "underlay-recovery requires --path-mode automatic" >&2
  exit 2
fi
if [[ "$scenario" == multi-network && "$path_mode" != automatic ]]; then
  echo "multi-network requires --path-mode automatic" >&2
  exit 2
fi
if [[ "$scenario" == network-workflow && "$path_mode" != automatic ]]; then
  echo "network-workflow requires --path-mode automatic" >&2
  exit 2
fi

if [[ ! "$minimum_free_bytes" =~ ^[0-9]{1,18}$ ]]; then
  echo "P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES must be an integer from 0 to 999999999999999999" >&2
  exit 2
fi
minimum_free_bytes=$((10#$minimum_free_bytes))
readonly minimum_free_bytes

if [[ ! "$maximum_runtime_growth_bytes" =~ ^[0-9]{1,18}$ ]]; then
  echo "P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES must be an unsigned integer" >&2
  exit 2
fi
maximum_runtime_growth_bytes=$((10#$maximum_runtime_growth_bytes))
if ((maximum_runtime_growth_bytes < 1 \
  || maximum_runtime_growth_bytes > hard_maximum_runtime_growth_bytes)); then
  printf 'P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES must be between 1 and %s\n' \
    "$hard_maximum_runtime_growth_bytes" >&2
  exit 2
fi
readonly maximum_runtime_growth_bytes

if [[ ! "$adb_timeout_seconds" =~ ^[0-9]{1,3}$ ]]; then
  echo "P2P_VPN_ANDROID_E2E_ADB_TIMEOUT_SECONDS must be an integer from 1 to 300" >&2
  exit 2
fi
adb_timeout_seconds=$((10#$adb_timeout_seconds))
if ((adb_timeout_seconds < 1 || adb_timeout_seconds > 300)); then
  echo "P2P_VPN_ANDROID_E2E_ADB_TIMEOUT_SECONDS must be between 1 and 300" >&2
  exit 2
fi

if [[ -z "$output_dir" ]]; then
  output_dir="$(mktemp -d -t p2p-vpn-android-e2e-evidence.XXXXXXXX)"
else
  mkdir -p "$output_dir"
  output_dir="$(cd "$output_dir" && pwd -P)"
fi

readonly output_dir
readonly checks_file="$output_dir/.preflight.ndjson"
readonly steps_file="$output_dir/.steps.ndjson"
readonly device_file="$output_dir/.device.json"
readonly evidence_file="$output_dir/evidence.json"
readonly emulator_log="$output_dir/emulator.log"
readonly fixture_log="$output_dir/fixture.log"
readonly fixture_secondary_log="$output_dir/fixture-secondary.log"
readonly android_log="$output_dir/android.log"

: > "$checks_file"
: > "$steps_file"
: > "$emulator_log"
: > "$fixture_log"
: > "$fixture_secondary_log"
: > "$android_log"
printf '{}\n' > "$device_file"

record_check() {
  local name="$1"
  local required="$2"
  local available="$3"
  local detail="$4"
  jq -cn \
    --arg name "$name" \
    --argjson required "$required" \
    --argjson available "$available" \
    --arg detail "$detail" \
    '{name: $name, required: $required, available: $available, detail: $detail}' \
    >> "$checks_file"
}

record_step() {
  local name="$1"
  local status="$2"
  local detail="$3"
  jq -cn \
    --arg name "$name" \
    --arg status "$status" \
    --arg detail "$detail" \
    '{name: $name, status: $status, detail: $detail}' \
    >> "$steps_file"
}

monotonic_millis() {
  awk '{printf "%.0f\n", $1 * 1000}' /proc/uptime
}

available_bytes_for_path() {
  local path="$1"
  "$df_command" --output=avail -B1 "$path" 2>/dev/null \
    | tail -n 1 \
    | tr -d '[:space:]'
}

check_runtime_storage_budget() {
  local current_tmp_available_bytes
  local current_output_available_bytes
  local tmp_growth_bytes=0
  local output_growth_bytes=0

  current_tmp_available_bytes="$({ available_bytes_for_path "${TMPDIR:-/tmp}"; } || true)"
  current_output_available_bytes="$({ available_bytes_for_path "$output_dir"; } || true)"
  if [[ ! "$current_tmp_available_bytes" =~ ^[0-9]+$ \
    || ! "$current_output_available_bytes" =~ ^[0-9]+$ ]]; then
    echo "Android E2E stopped because runtime free space could not be monitored"
    return 1
  fi

  if ((current_tmp_available_bytes < runtime_tmp_baseline_available_bytes)); then
    tmp_growth_bytes=$((runtime_tmp_baseline_available_bytes - current_tmp_available_bytes))
  fi
  if ((current_output_available_bytes < runtime_output_baseline_available_bytes)); then
    output_growth_bytes=$((runtime_output_baseline_available_bytes - current_output_available_bytes))
  fi

  if ((current_tmp_available_bytes < minimum_free_bytes \
    || current_output_available_bytes < minimum_free_bytes)); then
    echo "Android E2E stopped because runtime free space fell below the required reserve"
    return 1
  fi
  if ((tmp_growth_bytes > maximum_runtime_growth_bytes \
    || output_growth_bytes > maximum_runtime_growth_bytes)); then
    echo "Android E2E stopped because runtime storage growth exceeded the per-run limit"
    return 1
  fi
  return 0
}

start_runtime_storage_watchdog() {
  (
    trap 'exit 0' INT TERM
    local failure
    while sleep 1; do
      if ! failure="$(check_runtime_storage_budget)"; then
        printf '%s\n' "$failure" > "$runtime_storage_failure_file"
        kill -TERM "$harness_pid" 2>/dev/null || true
        exit 0
      fi
    done
  ) &
  runtime_storage_watchdog_pid=$!
}

stop_runtime_storage_watchdog() {
  [[ -n "$runtime_storage_watchdog_pid" ]] || return 0
  if kill -0 "$runtime_storage_watchdog_pid" 2>/dev/null; then
    kill -TERM "$runtime_storage_watchdog_pid" 2>/dev/null || true
  fi
  wait "$runtime_storage_watchdog_pid" 2>/dev/null || true
  runtime_storage_watchdog_pid=""
}

android_automation() {
  local command="$1"
  shift
  local broadcast encoded
  if ! broadcast="$(
    adb_run shell am broadcast \
      --receiver-foreground \
      -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION \
      -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver \
      --es command "$command" \
      "$@"
  )"; then
    return 1
  fi
  encoded="$(
    sed -nE \
      's/^Broadcast completed: result=[^,]+, data="([A-Za-z0-9+\/=]+)".*$/\1/p' \
      <<< "$broadcast"
  )"
  [[ -n "$encoded" ]] || return 1
  printf '%s' "$encoded" | base64 --decode
}

wait_for_automation_status() {
  local path="$1"
  local predicate="$2"
  local attempts="${3:-30}"
  for _ in $(seq 1 "$attempts"); do
    if android_automation status > "$path" \
      && jq -e ".schema_version == 1 and .ok and .value.service_ready and ($predicate)" \
        "$path" >/dev/null; then
      return 0
    fi
    sleep 1
  done
  return 1
}

wait_for_pair_status() {
  local path="$1"
  local operation_id="$2"
  local predicate="$3"
  local attempts="${4:-120}"
  local control_socket="${5:-$fixture_control_socket}"
  for _ in $(seq 1 "$attempts"); do
    if "$p2p_vpn_command" pair status "$operation_id" \
      --socket "$control_socket" \
      --format json > "$path" 2>/dev/null \
      && jq -e "$predicate" "$path" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}

wait_for_fixture_probe_ready() {
  local source="$1"
  local destination="$2"
  local family="$3"
  local path="$4"
  local attempts_variable="$5"
  local packet_socket="${6:-$fixture_packet_socket}"
  local attempt
  for attempt in $(seq 1 30); do
    if "$fixture_command" probe \
      --socket "$packet_socket" \
      --source "$source" \
      --destination "$destination" \
      --count 1 \
      --timeout-millis 1000 > "$path" 2>/dev/null \
      && jq -e \
        --arg family "$family" \
        '.schema_version == 1 and .ok and .family == $family and .sent == 1 and .received == 1' \
        "$path" >/dev/null 2>&1; then
      printf -v "$attempts_variable" '%d' "$attempt"
      return 0
    fi
    sleep 1
  done
  printf -v "$attempts_variable" '%d' 30
  return 1
}

wait_for_android_ping_ready() {
  local family="$1"
  local destination="$2"
  local path="$3"
  local attempts_variable="$4"
  local attempt
  local -a command
  if [[ "$family" == ipv4 ]]; then
    command=(ping)
  else
    command=(ping6)
  fi
  for attempt in $(seq 1 30); do
    if adb_run shell "${command[@]}" -c 1 -W 2 "$destination" \
      > "$path" 2>&1 \
      && grep -Eq '1 packets? transmitted, 1 (packets? )?received' "$path"; then
      printf -v "$attempts_variable" '%d' "$attempt"
      return 0
    fi
    sleep 1
  done
  printf -v "$attempts_variable" '%d' 30
  return 1
}

wait_for_transition_traffic_ready() {
  local prefix="$1"
  local context="$2"
  local packet_socket="${3:-$fixture_packet_socket}"
  local linux_ipv4="${4:-$fixture_ipv4}"
  local linux_ipv6="${5:-$fixture_ipv6}"
  local device_ipv4="${6:-$android_ipv4}"
  local device_ipv6="${7:-$android_ipv6}"
  local fixture_ipv4_attempts=0
  local fixture_ipv6_attempts=0
  local android_ipv4_attempts=0
  local android_ipv6_attempts=0
  if ! wait_for_fixture_probe_ready \
    "$linux_ipv4" \
    "$device_ipv4" \
    ipv4 \
    "$state_dir/${prefix}-linux-ipv4-readiness.json" \
    fixture_ipv4_attempts \
    "$packet_socket" \
    || ! wait_for_fixture_probe_ready \
      "$linux_ipv6" \
      "$device_ipv6" \
      ipv6 \
      "$state_dir/${prefix}-linux-ipv6-readiness.json" \
      fixture_ipv6_attempts \
      "$packet_socket" \
    || ! wait_for_android_ping_ready \
      ipv4 \
      "$linux_ipv4" \
      "$state_dir/${prefix}-android-ipv4-readiness.txt" \
      android_ipv4_attempts \
    || ! wait_for_android_ping_ready \
      ipv6 \
      "$linux_ipv6" \
      "$state_dir/${prefix}-android-ipv6-readiness.txt" \
      android_ipv6_attempts; then
    outcome=failed
    outcome_detail="Bidirectional dual-stack forwarding did not converge $context"
    record_step "${prefix}_traffic_readiness" failed "$outcome_detail"
    return 1
  fi
  record_step "${prefix}_traffic_readiness" passed \
    "Bidirectional dual-stack forwarding converged $context after $fixture_ipv4_attempts/$fixture_ipv6_attempts Linux and $android_ipv4_attempts/$android_ipv6_attempts Android attempts"
}

wait_for_multi_network_transition_traffic_ready() {
  local prefix="$1"
  local context="$2"
  wait_for_transition_traffic_ready \
    "${prefix}_alpha" \
    "$context on alpha" \
    "$fixture_packet_socket" \
    "$fixture_ipv4" \
    "$fixture_ipv6" \
    "$android_primary_ipv4" \
    "$android_primary_ipv6" \
    && wait_for_transition_traffic_ready \
      "${prefix}_beta" \
      "$context on beta" \
      "$fixture_secondary_packet_socket" \
      "$fixture_secondary_ipv4" \
      "$fixture_secondary_ipv6" \
      "$android_secondary_ipv4" \
      "$android_secondary_ipv6"
}

measure_bidirectional_traffic() {
  local prefix="$1"
  local context="$2"
  local packet_socket="${3:-$fixture_packet_socket}"
  local linux_ipv4="${4:-$fixture_ipv4}"
  local linux_ipv6="${5:-$fixture_ipv6}"
  local device_ipv4="${6:-$android_ipv4}"
  local device_ipv6="${7:-$android_ipv6}"
  local step_prefix=""
  local detail_suffix=""
  local file_prefix="$state_dir/baseline"
  if [[ -n "$prefix" ]]; then
    step_prefix="${prefix}_"
    file_prefix="$state_dir/$prefix"
  fi
  if [[ -n "$context" ]]; then
    detail_suffix=" $context"
  fi

  if ! "$fixture_command" probe \
    --socket "$packet_socket" \
    --source "$linux_ipv4" \
    --destination "$device_ipv4" \
    --count 5 > "${file_prefix}-linux-ipv4.json" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .family == "ipv4" and .sent == 5 and .received == 5' \
      "${file_prefix}-linux-ipv4.json" >/dev/null; then
    outcome=failed
    outcome_detail="Linux-to-Android IPv4 overlay probe failed$detail_suffix"
    record_step "${step_prefix}linux_to_android_ipv4" failed "$outcome_detail"
    return 1
  fi
  record_step "${step_prefix}linux_to_android_ipv4" passed \
    "Linux received 5 of 5 IPv4 replies$detail_suffix"

  if ! "$fixture_command" probe \
    --socket "$packet_socket" \
    --source "$linux_ipv6" \
    --destination "$device_ipv6" \
    --count 5 > "${file_prefix}-linux-ipv6.json" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .family == "ipv6" and .sent == 5 and .received == 5' \
      "${file_prefix}-linux-ipv6.json" >/dev/null; then
    outcome=failed
    outcome_detail="Linux-to-Android IPv6 overlay probe failed$detail_suffix"
    record_step "${step_prefix}linux_to_android_ipv6" failed "$outcome_detail"
    return 1
  fi
  record_step "${step_prefix}linux_to_android_ipv6" passed \
    "Linux received 5 of 5 IPv6 replies$detail_suffix"

  if ! adb_run shell ping -c 5 -W 5 "$linux_ipv4" \
    > "${file_prefix}-android-ipv4.txt" 2>&1 \
    || ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
      "${file_prefix}-android-ipv4.txt"; then
    local received
    received="$(ping_received_count "${file_prefix}-android-ipv4.txt")"
    outcome=failed
    outcome_detail="Android-to-Linux IPv4 ping received $received of 5 replies$detail_suffix"
    record_step "${step_prefix}android_to_linux_ipv4" failed "$outcome_detail"
    return 1
  fi
  record_step "${step_prefix}android_to_linux_ipv4" passed \
    "Android received 5 of 5 IPv4 replies$detail_suffix"

  if ! adb_run shell ping6 -c 5 -W 5 "$linux_ipv6" \
    > "${file_prefix}-android-ipv6.txt" 2>&1 \
    || ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
      "${file_prefix}-android-ipv6.txt"; then
    local received
    received="$(ping_received_count "${file_prefix}-android-ipv6.txt")"
    outcome=failed
    outcome_detail="Android-to-Linux IPv6 ping received $received of 5 replies$detail_suffix"
    record_step "${step_prefix}android_to_linux_ipv6" failed "$outcome_detail"
    return 1
  fi
  record_step "${step_prefix}android_to_linux_ipv6" passed \
    "Android received 5 of 5 IPv6 replies$detail_suffix"
}

pair_selected_network() {
  local prefix="$1"
  local network_id="$2"
  local expected_peer_id="$3"
  local expected_hostname="$4"
  local control_socket="$5"
  local minimum_connected_peers="$6"
  local status_variable="$7"
  local pair_open="$state_dir/$prefix-pair-open.json"
  local inviter_status="$state_dir/$prefix-inviter-status.json"
  local pair_approved="$state_dir/$prefix-pair-approved.json"
  local command_response="$state_dir/$prefix-pair-command.json"
  local paired_status="$state_dir/$prefix-status-paired.json"
  local pair_operation pair_code approval_id

  if ! "$p2p_vpn_command" pair open \
    --socket "$control_socket" \
    --expires-in-seconds 300 \
    --format json > "$pair_open" \
    || ! jq -e '
      (.operation_id | type == "string" and length > 0 and length <= 128) and
      (.code | type == "string" and length > 0 and length <= 64)
    ' "$pair_open" >/dev/null; then
    outcome=failed
    outcome_detail="The $prefix fixture could not open a bounded pairing operation"
    record_step "${prefix}_pairing" failed "$outcome_detail"
    return 1
  fi
  pair_operation="$(jq -r '.operation_id' "$pair_open")"
  pair_code="$(jq -r '.code' "$pair_open")"

  if ! android_automation join-pairing --es code "$pair_code" > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "join-pairing"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Android did not accept the $prefix pairing code"
    record_step "${prefix}_pairing" failed "$outcome_detail"
    return 1
  fi

  if ! wait_for_pair_status \
    "$inviter_status" \
    "$pair_operation" \
    '.phase == "awaiting_approval" and (.candidate.approval_id | type == "string" and length > 0 and length <= 128)' \
    180 \
    "$control_socket"; then
    outcome=failed
    outcome_detail="$prefix pairing did not discover the Android candidate"
    record_step "${prefix}_pairing" failed "$outcome_detail"
    return 1
  fi
  if [[ "$(jq -r '.candidate.peer_id' "$inviter_status")" != "$expected_peer_id" \
    || "$(jq -r '.candidate.requested_hostname // empty' "$inviter_status")" \
      != "$expected_hostname" ]]; then
    outcome=failed
    outcome_detail="$prefix pairing candidate metadata did not match the selected network"
    record_step "${prefix}_pairing" failed "$outcome_detail"
    return 1
  fi

  approval_id="$(jq -r '.candidate.approval_id' "$inviter_status")"
  if ! "$p2p_vpn_command" pair approve \
    "$pair_operation" \
    "$approval_id" \
    --socket "$control_socket" \
    --format json > "$pair_approved" \
    || ! jq -e '.phase == "completed"' "$pair_approved" >/dev/null; then
    outcome=failed
    outcome_detail="The $prefix fixture could not approve the Android candidate"
    record_step "${prefix}_pairing" failed "$outcome_detail"
    return 1
  fi

  if ! wait_for_automation_status \
    "$paired_status" \
    ".value.snapshot.connected and (.value.snapshot.busy | not) and ([.value.snapshot.networks[] | select(.id == \"$network_id\" and .selected and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= $minimum_connected_peers)" \
    180; then
    outcome=failed
    outcome_detail="Android did not apply the $prefix pairing artifacts"
    record_step "${prefix}_pairing" failed "$outcome_detail"
    return 1
  fi

  printf -v "$status_variable" '%s' "$paired_status"
  record_step "${prefix}_pairing" passed \
    "Android paired the selected $prefix network without a configured overlay address"
}

create_isolated_android_profile() {
  local prefix="$1"
  local network="$2"
  local bootstrap_peer="$3"
  local bootstrap_address="$4"
  local kademlia_protocol="$5"
  local expected_networks="$6"
  local status_variable="$7"
  local command_response="$state_dir/$prefix-create-command.json"
  local created_status="$state_dir/$prefix-status-created.json"

  if ! android_automation create-profile \
    --es network "$network" \
    --es bootstrap_peer_id "$bootstrap_peer" \
    --es bootstrap_address "$bootstrap_address" \
    --es kademlia_protocol "$kademlia_protocol" > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "create-profile"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept $prefix profile creation"
    record_step "${prefix}_profile_creation" failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_automation_status \
    "$created_status" \
    ".value.snapshot.has_profile and .value.snapshot.profile_stored and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == $expected_networks) and ([.value.snapshot.networks[] | select(.name == \"$network\" and .selected and .enabled)] | length == 1)" \
    120; then
    outcome=failed
    outcome_detail="$prefix encrypted profile creation did not complete"
    record_step "${prefix}_profile_creation" failed "$outcome_detail"
    return 1
  fi

  printf -v "$status_variable" '%s' "$created_status"
  record_step "${prefix}_profile_creation" passed \
    "$prefix was added with isolated discovery bootstrap configuration"
}

measure_concurrent_multi_network_traffic() {
  local prefix="$1"
  local context="$2"
  local started_variable="$3"
  local duration_variable="$4"
  local file_prefix="$state_dir/$prefix"
  local batch_started batch_completed process received failed_summary
  local attempt attempt_detail=""
  local max_attempts=3
  local failed_processes=0
  local -a failed_legs=()
  local -a processes=()

  for attempt in $(seq 1 "$max_attempts"); do
    failed_processes=0
    failed_legs=()
    processes=()
    batch_started="$(monotonic_millis)"
    "$fixture_command" probe \
      --socket "$fixture_packet_socket" \
      --source "$fixture_ipv4" \
      --destination "$android_primary_ipv4" \
      --count 5 > "$file_prefix-alpha-linux-ipv4.json" 2>/dev/null &
    processes+=("$!")
    "$fixture_command" probe \
      --socket "$fixture_packet_socket" \
      --source "$fixture_ipv6" \
      --destination "$android_primary_ipv6" \
      --count 5 > "$file_prefix-alpha-linux-ipv6.json" 2>/dev/null &
    processes+=("$!")
    "$fixture_command" probe \
      --socket "$fixture_secondary_packet_socket" \
      --source "$fixture_secondary_ipv4" \
      --destination "$android_secondary_ipv4" \
      --count 5 > "$file_prefix-beta-linux-ipv4.json" 2>/dev/null &
    processes+=("$!")
    "$fixture_command" probe \
      --socket "$fixture_secondary_packet_socket" \
      --source "$fixture_secondary_ipv6" \
      --destination "$android_secondary_ipv6" \
      --count 5 > "$file_prefix-beta-linux-ipv6.json" 2>/dev/null &
    processes+=("$!")
    adb_run shell ping -c 5 -W 5 "$fixture_ipv4" \
      > "$file_prefix-alpha-android-ipv4.txt" 2>&1 &
    processes+=("$!")
    adb_run shell ping6 -c 5 -W 5 "$fixture_ipv6" \
      > "$file_prefix-alpha-android-ipv6.txt" 2>&1 &
    processes+=("$!")
    adb_run shell ping -c 5 -W 5 "$fixture_secondary_ipv4" \
      > "$file_prefix-beta-android-ipv4.txt" 2>&1 &
    processes+=("$!")
    adb_run shell ping6 -c 5 -W 5 "$fixture_secondary_ipv6" \
      > "$file_prefix-beta-android-ipv6.txt" 2>&1 &
    processes+=("$!")

    for process in "${processes[@]}"; do
      if ! wait "$process"; then
        failed_processes=$((failed_processes + 1))
      fi
    done
    batch_completed="$(monotonic_millis)"

    if ! jq -e '.schema_version == 1 and .ok and .family == "ipv4" and .sent == 5 and .received == 5' \
      "$file_prefix-alpha-linux-ipv4.json" >/dev/null 2>&1; then
      received="$(jq -r '.received // 0' "$file_prefix-alpha-linux-ipv4.json" 2>/dev/null || printf '0')"
      failed_legs+=("alpha/Linux-to-Android/IPv4=$received/5")
    fi
    if ! jq -e '.schema_version == 1 and .ok and .family == "ipv6" and .sent == 5 and .received == 5' \
      "$file_prefix-alpha-linux-ipv6.json" >/dev/null 2>&1; then
      received="$(jq -r '.received // 0' "$file_prefix-alpha-linux-ipv6.json" 2>/dev/null || printf '0')"
      failed_legs+=("alpha/Linux-to-Android/IPv6=$received/5")
    fi
    if ! jq -e '.schema_version == 1 and .ok and .family == "ipv4" and .sent == 5 and .received == 5' \
      "$file_prefix-beta-linux-ipv4.json" >/dev/null 2>&1; then
      received="$(jq -r '.received // 0' "$file_prefix-beta-linux-ipv4.json" 2>/dev/null || printf '0')"
      failed_legs+=("beta/Linux-to-Android/IPv4=$received/5")
    fi
    if ! jq -e '.schema_version == 1 and .ok and .family == "ipv6" and .sent == 5 and .received == 5' \
      "$file_prefix-beta-linux-ipv6.json" >/dev/null 2>&1; then
      received="$(jq -r '.received // 0' "$file_prefix-beta-linux-ipv6.json" 2>/dev/null || printf '0')"
      failed_legs+=("beta/Linux-to-Android/IPv6=$received/5")
    fi
    if ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
      "$file_prefix-alpha-android-ipv4.txt"; then
      received="$(ping_received_count "$file_prefix-alpha-android-ipv4.txt")"
      failed_legs+=("alpha/Android-to-Linux/IPv4=$received/5")
    fi
    if ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
      "$file_prefix-alpha-android-ipv6.txt"; then
      received="$(ping_received_count "$file_prefix-alpha-android-ipv6.txt")"
      failed_legs+=("alpha/Android-to-Linux/IPv6=$received/5")
    fi
    if ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
      "$file_prefix-beta-android-ipv4.txt"; then
      received="$(ping_received_count "$file_prefix-beta-android-ipv4.txt")"
      failed_legs+=("beta/Android-to-Linux/IPv4=$received/5")
    fi
    if ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
      "$file_prefix-beta-android-ipv6.txt"; then
      received="$(ping_received_count "$file_prefix-beta-android-ipv6.txt")"
      failed_legs+=("beta/Android-to-Linux/IPv6=$received/5")
    fi

    if [[ "$failed_processes" -eq 0 && "${#failed_legs[@]}" -eq 0 ]]; then
      if [[ "$attempt" -gt 1 ]]; then
        attempt_detail=" after $attempt bounded attempts"
      fi
      printf -v "$started_variable" '%s' "$batch_started"
      printf -v "$duration_variable" '%s' "$((batch_completed - batch_started))"
      record_step "${prefix}_concurrent_traffic" passed \
        "Both networks carried 5 of 5 packets in every direction and address family $context$attempt_detail"
      return 0
    fi

    if [[ "${#failed_legs[@]}" -eq 0 ]]; then
      failed_legs+=("$failed_processes subprocesses exited nonzero")
    fi
    failed_summary="$(printf '%s, ' "${failed_legs[@]}")"
    failed_summary="${failed_summary%, }"
    if [[ "$attempt" -lt "$max_attempts" ]]; then
      sleep 2
    fi
  done

  outcome=failed
  outcome_detail="Concurrent dual-stack traffic failed $context after $max_attempts bounded attempts: $failed_summary"
  record_step "${prefix}_concurrent_traffic" failed "$outcome_detail"
  return 1
}

wait_for_new_android_process() {
  local previous_pid="$1"
  local process_variable="$2"
  local current_pid=""
  for _ in $(seq 1 90); do
    current_pid="$(
      adb_run shell pidof org.hermeticfoundation.p2pvpn.debug 2>/dev/null | tr -d '\r'
    )"
    if [[ "$current_pid" =~ ^[0-9]+$ && "$current_pid" != "$previous_pid" ]]; then
      printf -v "$process_variable" '%s' "$current_pid"
      return 0
    fi
    sleep 1
  done
  return 1
}

wait_for_android_boot() {
  local adb_timeout_seconds=5
  for _ in $(seq 1 240); do
    if [[ "$(adb_run get-state 2>/dev/null || true)" == device \
      && "$(adb_run shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" == 1 ]]; then
      return 0
    fi
    sleep 1
  done
  return 1
}

wait_for_android_process() {
  local process_variable="$1"
  local adb_timeout_seconds=5
  local current_pid=""
  for _ in $(seq 1 90); do
    current_pid="$(
      adb_run shell pidof org.hermeticfoundation.p2pvpn.debug 2>/dev/null | tr -d '\r'
    )"
    if [[ "$current_pid" =~ ^[0-9]+$ ]]; then
      printf -v "$process_variable" '%s' "$current_pid"
      return 0
    fi
    sleep 1
  done
  return 1
}

network_identity_signature_matches() {
  local status_path="$1"
  jq -e -s '
    .[0] == (.[1].value.snapshot.networks |
      map({id, name, hostname, peer_id, addresses}) | sort_by(.id))
  ' "$state_dir/multi-network-identity-signature.json" "$status_path" >/dev/null
}

run_multi_network_restoration_and_isolation() {
  local outage_status connected_peers_after

  process_before="$(
    adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r'
  )"
  if [[ ! "$process_before" =~ ^[0-9]+$ ]] \
    || ! android_automation terminate-process > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "terminate-process"
    ' "$command_response" >/dev/null \
    || ! wait_for_new_android_process "$process_before" process_after; then
    outcome=failed
    outcome_detail="Android always-on did not autonomously restart after process death"
    record_step multi_network_process_restore failed "$outcome_detail"
    return 1
  fi
  process_status="$state_dir/status-after-process-death.json"
  if ! wait_for_automation_status \
    "$process_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not) and .value.snapshot.selected_network_id == \"$alpha_id\" and ([.value.snapshot.networks[] | select(.enabled and .phase == \"running\")] | length == 2) and (.value.snapshot.paths.connected_peers >= 2)" \
    180 \
    || ! network_identity_signature_matches "$process_status"; then
    outcome=failed
    outcome_detail="Process-death restoration did not recover both enabled networks"
    record_step multi_network_process_restore failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    process-restored \
    "after autonomous process-death restoration" \
    || ! measure_concurrent_multi_network_traffic \
    process-restored \
    "after autonomous process-death restoration" \
    process_traffic_started \
    process_traffic_duration; then
    return 1
  fi
  record_step multi_network_process_restore passed \
    "A fresh always-on process restored both identities and traffic"

  process_before="$process_after"
  if ! adb_run install -r "$android_apk" >/dev/null \
    || ! wait_for_new_android_process "$process_before" process_after; then
    outcome=failed
    outcome_detail="APK replacement did not restart the complete enabled set"
    record_step multi_network_update_restore failed "$outcome_detail"
    return 1
  fi
  update_status="$state_dir/status-both-after-update.json"
  if ! wait_for_automation_status \
    "$update_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not) and .value.snapshot.selected_network_id == \"$alpha_id\" and ([.value.snapshot.networks[] | select(.enabled and .phase == \"running\")] | length == 2) and (.value.snapshot.paths.connected_peers >= 2)" \
    180 \
    || ! network_identity_signature_matches "$update_status"; then
    outcome=failed
    outcome_detail="APK replacement did not restore both enabled network identities"
    record_step multi_network_update_restore failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    update-restored \
    "after restoring both networks through an APK update" \
    || ! measure_concurrent_multi_network_traffic \
    update-restored \
    "after restoring both networks through an APK update" \
    update_traffic_started \
    update_traffic_duration; then
    return 1
  fi
  record_step multi_network_update_restore passed \
    "APK replacement restored both enabled network runtimes and traffic"

  if ! set_android_vpn_mode true true; then
    outcome=failed
    outcome_detail="Android did not enable the temporary lockdown rejection state"
    record_step multi_network_lockdown_restore failed "$outcome_detail"
    return 1
  fi
  lockdown_status="$state_dir/status-multi-network-lockdown.json"
  if ! wait_for_automation_status \
    "$lockdown_status" \
    '.value.snapshot.always_on and .value.snapshot.lockdown and (.value.snapshot.connected | not) and .value.snapshot.connection_requested and (.value.snapshot.networks | length == 2) and ([.value.snapshot.networks[] | select(.enabled)] | length == 2) and (.value.snapshot.connection_detail | contains("Block connections without VPN"))' \
    45 \
    || ! network_identity_signature_matches "$lockdown_status"; then
    outcome=failed
    outcome_detail="Lockdown rejection did not retain the complete stored network set"
    record_step multi_network_lockdown_restore failed "$outcome_detail"
    return 1
  fi
  if ! set_android_vpn_mode true false; then
    outcome=failed
    outcome_detail="Android did not leave the temporary lockdown state"
    record_step multi_network_lockdown_restore failed "$outcome_detail"
    return 1
  fi
  restored_status="$state_dir/status-multi-network-lockdown-restored.json"
  if ! wait_for_automation_status \
    "$restored_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not) and ([.value.snapshot.networks[] | select(.enabled and .phase == \"running\")] | length == 2) and (.value.snapshot.paths.connected_peers >= 2)" \
    180 \
    || ! network_identity_signature_matches "$restored_status"; then
    outcome=failed
    outcome_detail="Both networks did not recover after lockdown rejection ended"
    record_step multi_network_lockdown_restore failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    lockdown-restored \
    "after temporary lockdown rejection" \
    || ! measure_concurrent_multi_network_traffic \
    lockdown-restored \
    "after temporary lockdown rejection" \
    lockdown_traffic_started \
    lockdown_traffic_duration; then
    return 1
  fi
  record_step multi_network_lockdown_restore passed \
    "Both networks recovered automatically after temporary lockdown rejection"

  boot_id_before="$(
    adb_run shell cat /proc/sys/kernel/random/boot_id | tr -d '\r'
  )"
  if [[ ! "$boot_id_before" =~ ^[0-9a-f-]{36}$ ]] \
    || ! adb_run reboot >/dev/null \
    || ! wait_for_android_boot; then
    outcome=failed
    outcome_detail="The managed emulator did not complete the reboot probe"
    record_step multi_network_reboot_restore failed "$outcome_detail"
    return 1
  fi
  adb_run shell wm dismiss-keyguard >/dev/null 2>&1 || true
  adb_run shell input keyevent 82 >/dev/null 2>&1 || true
  boot_id_after="$(
    adb_run shell cat /proc/sys/kernel/random/boot_id | tr -d '\r'
  )"
  if [[ ! "$boot_id_after" =~ ^[0-9a-f-]{36}$ \
    || "$boot_id_after" == "$boot_id_before" ]] \
    || ! wait_for_android_process process_after; then
    outcome=failed
    outcome_detail="Android did not autonomously launch the always-on app after reboot"
    record_step multi_network_reboot_restore failed "$outcome_detail"
    return 1
  fi
  reboot_status="$state_dir/status-multi-network-after-reboot.json"
  if ! wait_for_automation_status \
    "$reboot_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not) and .value.snapshot.selected_network_id == \"$alpha_id\" and ([.value.snapshot.networks[] | select(.enabled and .phase == \"running\")] | length == 2) and (.value.snapshot.paths.connected_peers >= 2)" \
    240 \
    || ! network_identity_signature_matches "$reboot_status"; then
    outcome=failed
    outcome_detail="Reboot restoration did not recover both enabled network identities"
    record_step multi_network_reboot_restore failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    reboot-restored \
    "after a full managed-emulator reboot" \
    || ! measure_concurrent_multi_network_traffic \
    reboot-restored \
    "after a full managed-emulator reboot" \
    reboot_traffic_started \
    reboot_traffic_duration; then
    return 1
  fi
  record_step multi_network_reboot_restore passed \
    "Android rebooted and restored both always-on network runtimes and traffic"

  if ! android_automation diagnostics > "$diagnostic_response" \
    || ! jq -c '.value.report' "$diagnostic_response" > "$diagnostic_report" \
    || ! diagnostic_report_is_valid "$diagnostic_report" \
    || ! jq -e '
      .resources.active_threads <= 128 and
      .resources.total_pss_kib <= 524288 and
      .queue.queued_packets <= 512 and
      .queue.queued_bytes <= 2097152
    ' "$diagnostic_report" >/dev/null; then
    outcome=failed
    outcome_detail="Concurrent runtime resources exceeded the bounded E2E contract"
    record_step multi_network_resource_bounds failed "$outcome_detail"
    return 1
  fi
  active_threads="$(jq -r '.resources.active_threads' "$diagnostic_report")"
  total_pss_kib="$(jq -r '.resources.total_pss_kib' "$diagnostic_report")"
  queued_packets="$(jq -r '.queue.queued_packets' "$diagnostic_report")"
  queued_bytes="$(jq -r '.queue.queued_bytes' "$diagnostic_report")"
  record_step multi_network_resource_bounds passed \
    "Two active networks remained within thread, memory, packet, and byte limits"

  isolation_generation="$(jq -r '.value.snapshot.runtime_generation' "$reboot_status")"
  connected_peers_before="$(jq -r '.value.snapshot.paths.connected_peers' "$reboot_status")"
  isolation_process_before="$(
    adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r'
  )"
  if [[ ! "$isolation_process_before" =~ ^[0-9]+$ \
    || "$connected_peers_before" -lt 2 ]] \
    || ! stop_fixture_process "$fixture_pid"; then
    outcome=failed
    outcome_detail="The alpha fixture could not be stopped for failure isolation"
    record_step per_network_failure_isolation failed "$outcome_detail"
    return 1
  fi
  outage_status="$state_dir/status-alpha-fixture-unavailable.json"
  if ! wait_for_automation_status \
    "$outage_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.busy | not) and (.value.snapshot.runtime_generation == $isolation_generation) and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .enabled and .phase == \"running\")] | length == 1) and ([.value.snapshot.networks[] | select(.id == \"$beta_id\" and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= 1) and (.value.snapshot.paths.connected_peers < $connected_peers_before)" \
    180; then
    outcome=failed
    outcome_detail="The shared runtime did not isolate the unavailable alpha path"
    record_step per_network_failure_isolation failed "$outcome_detail"
    return 1
  fi
  if timeout 5s "$fixture_command" probe \
    --socket "$fixture_packet_socket" \
    --source "$fixture_ipv4" \
    --destination "$android_primary_ipv4" \
    --count 1 > "$state_dir/unavailable-alpha-linux-ipv4.json" 2>/dev/null; then
    outcome=failed
    outcome_detail="The terminated alpha fixture remained unexpectedly reachable"
    record_step per_network_failure_isolation failed "$outcome_detail"
    return 1
  fi
  if adb_run shell ping -c 1 -W 2 "$fixture_ipv4" \
    > "$unavailable_ping" 2>&1 \
    || [[ "$(ping_received_count "$unavailable_ping")" -ne 0 ]]; then
    outcome=failed
    outcome_detail="Alpha traffic did not fail closed after its fixture terminated"
    record_step per_network_failure_isolation failed "$outcome_detail"
    return 1
  fi
  if ! measure_bidirectional_traffic \
    isolated-beta \
    "after the alpha fixture terminated" \
    "$fixture_secondary_packet_socket" \
    "$fixture_secondary_ipv4" \
    "$fixture_secondary_ipv6" \
    "$android_secondary_ipv4" \
    "$android_secondary_ipv6"; then
    return 1
  fi
  isolation_process_after="$(
    adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r'
  )"
  connected_peers_after="$(jq -r '.value.snapshot.paths.connected_peers' "$outage_status")"
  if [[ "$isolation_process_after" != "$isolation_process_before" ]] \
    || ! kill -0 "$fixture_secondary_pid" 2>/dev/null; then
    outcome=failed
    outcome_detail="A sibling endpoint restarted during alpha path failure"
    record_step per_network_failure_isolation failed "$outcome_detail"
    return 1
  fi
  if ! android_automation diagnostics > "$outage_diagnostic_response" \
    || ! jq -c '.value.report' "$outage_diagnostic_response" > "$outage_diagnostic_report" \
    || ! diagnostic_report_is_valid "$outage_diagnostic_report" \
    || ! jq -e '
      .queue.queued_packets <= 512 and .queue.queued_bytes <= 2097152
    ' "$outage_diagnostic_report" >/dev/null; then
    outcome=failed
    outcome_detail="Alpha path failure exceeded the bounded queue contract"
    record_step per_network_failure_isolation failed "$outcome_detail"
    return 1
  fi
  outage_queued_packets="$(jq -r '.queue.queued_packets' "$outage_diagnostic_report")"
  outage_queued_bytes="$(jq -r '.queue.queued_bytes' "$outage_diagnostic_report")"
  record_step per_network_failure_isolation passed \
    "Alpha became unreachable while beta traffic, process identity, and queue bounds remained stable"

  jq \
    --argjson primary_ipv4_attempts "$primary_ipv4_attempts" \
    --argjson primary_ipv6_attempts "$primary_ipv6_attempts" \
    --argjson secondary_ipv4_attempts "$secondary_ipv4_attempts" \
    --argjson secondary_ipv6_attempts "$secondary_ipv6_attempts" \
    --argjson android_primary_ipv4_attempts "$android_primary_ipv4_attempts" \
    --argjson android_primary_ipv6_attempts "$android_primary_ipv6_attempts" \
    --argjson android_secondary_ipv4_attempts "$android_secondary_ipv4_attempts" \
    --argjson android_secondary_ipv6_attempts "$android_secondary_ipv6_attempts" \
    --argjson initial_traffic_started "$initial_traffic_started" \
    --argjson overlap_traffic_started "$overlap_traffic_started" \
    --argjson reenabled_traffic_started "$reenabled_traffic_started" \
    --argjson cellular_traffic_started "$cellular_traffic_started" \
    --argjson wifi_traffic_started "$wifi_traffic_started" \
    --argjson process_traffic_started "$process_traffic_started" \
    --argjson update_traffic_started "$update_traffic_started" \
    --argjson reboot_traffic_started "$reboot_traffic_started" \
    --argjson lockdown_traffic_started "$lockdown_traffic_started" \
    --argjson initial_traffic_duration "$initial_traffic_duration" \
    --argjson overlap_traffic_duration "$overlap_traffic_duration" \
    --argjson reenabled_traffic_duration "$reenabled_traffic_duration" \
    --argjson cellular_traffic_duration "$cellular_traffic_duration" \
    --argjson wifi_traffic_duration "$wifi_traffic_duration" \
    --argjson process_traffic_duration "$process_traffic_duration" \
    --argjson update_traffic_duration "$update_traffic_duration" \
    --argjson reboot_traffic_duration "$reboot_traffic_duration" \
    --argjson lockdown_traffic_duration "$lockdown_traffic_duration" \
    --argjson active_threads "$active_threads" \
    --argjson total_pss_kib "$total_pss_kib" \
    --argjson queued_packets "$queued_packets" \
    --argjson queued_bytes "$queued_bytes" \
    --argjson outage_queued_packets "$outage_queued_packets" \
    --argjson outage_queued_bytes "$outage_queued_bytes" \
    --argjson connected_peers_before "$connected_peers_before" \
    --argjson connected_peers_after "$connected_peers_after" '
    . + {
      multi_network: {
        migration: {
          legacy_profile_migrated: true,
          identity_preserved: true,
          encrypted_storage: true
        },
        activation: {
          network_count: 2,
          independently_paired: true,
          isolated_identities: true,
          shared_tun: true
        },
        readiness_attempts: {
          alpha_linux_ipv4: $primary_ipv4_attempts,
          alpha_linux_ipv6: $primary_ipv6_attempts,
          beta_linux_ipv4: $secondary_ipv4_attempts,
          beta_linux_ipv6: $secondary_ipv6_attempts,
          alpha_android_ipv4: $android_primary_ipv4_attempts,
          alpha_android_ipv6: $android_primary_ipv6_attempts,
          beta_android_ipv4: $android_secondary_ipv4_attempts,
          beta_android_ipv6: $android_secondary_ipv6_attempts
        },
        traffic: {
          packets_per_direction_and_family: 5,
          started_monotonic_millis: {
            initial: $initial_traffic_started,
            after_overlap: $overlap_traffic_started,
            after_reenable: $reenabled_traffic_started,
            cellular_underlay: $cellular_traffic_started,
            wifi_underlay_restored: $wifi_traffic_started,
            process_restore: $process_traffic_started,
            update_restore: $update_traffic_started,
            reboot_restore: $reboot_traffic_started,
            lockdown_restore: $lockdown_traffic_started
          },
          initial_concurrent_millis: $initial_traffic_duration,
          after_overlap_millis: $overlap_traffic_duration,
          after_reenable_millis: $reenabled_traffic_duration,
          cellular_underlay_millis: $cellular_traffic_duration,
          wifi_underlay_restored_millis: $wifi_traffic_duration,
          process_restore_millis: $process_traffic_duration,
          update_restore_millis: $update_traffic_duration,
          reboot_restore_millis: $reboot_traffic_duration,
          lockdown_restore_millis: $lockdown_traffic_duration
        },
        overlap_rejection: {
          rejected_before_activation: true,
          collection_unchanged: true,
          runtime_generation_unchanged: true,
          runtime_directories: 2,
          live_traffic_preserved: true
        },
        lifecycle: {
          disabled_route_removed: true,
          sibling_remained_reachable: true,
          disabled_set_restored_after_update: true,
          reenabled_set_restored: true,
          underlay_changed_without_runtime_restart: true,
          process_death_restored: true,
          update_restored: true,
          reboot_restored: true,
          lockdown_restored: true,
          identities_preserved: true
        },
        failure_isolation: {
          failed_network_unreachable: true,
          sibling_traffic_preserved: true,
          android_process_continuous: true,
          runtime_generation_continuous: true,
          connected_peers_before: $connected_peers_before,
          connected_peers_after: $connected_peers_after,
          queued_packets: $outage_queued_packets,
          queued_bytes: $outage_queued_bytes
        },
        resources: {
          active_threads: $active_threads,
          maximum_active_threads: 128,
          total_pss_kib: $total_pss_kib,
          maximum_total_pss_kib: 524288,
          queued_packets: $queued_packets,
          maximum_queued_packets: 512,
          queued_bytes: $queued_bytes,
          maximum_queued_bytes: 2097152
        }
      }
    }
  ' "$device_file" > "$device_file.updated"
  mv -f "$device_file.updated" "$device_file"

  outcome=passed
  outcome_detail="Concurrent multi-network traffic, isolation, and lifecycle restoration passed"
}

run_multi_network_lifecycle() {
  local command_response="$state_dir/multi-network-lifecycle-command.json"
  local always_on_status selected_alpha_status disabled_status
  local disabled_update_status reenabled_status cellular_status wifi_status
  local process_status update_status reboot_status lockdown_status restored_status
  local process_before process_after boot_id_before boot_id_after
  local underlay_generation isolation_generation
  local connected_peers_before isolation_process_before isolation_process_after
  local disabled_probe="$state_dir/disabled-alpha-linux-ipv4.json"
  local disabled_ping="$state_dir/disabled-alpha-android-ipv4.txt"
  local unavailable_ping="$state_dir/unavailable-alpha-android-ipv4.txt"
  local diagnostic_response="$state_dir/multi-network-diagnostics.json"
  local diagnostic_report="$state_dir/multi-network-diagnostic-report.json"
  local outage_diagnostic_response="$state_dir/multi-network-outage-diagnostics.json"
  local outage_diagnostic_report="$state_dir/multi-network-outage-report.json"
  local reenabled_traffic_started=0 reenabled_traffic_duration=0
  local cellular_traffic_started=0 cellular_traffic_duration=0
  local wifi_traffic_started=0 wifi_traffic_duration=0
  local process_traffic_started=0 process_traffic_duration=0
  local update_traffic_started=0 update_traffic_duration=0
  local reboot_traffic_started=0 reboot_traffic_duration=0
  local lockdown_traffic_started=0 lockdown_traffic_duration=0
  local active_threads total_pss_kib queued_packets queued_bytes
  local outage_queued_packets outage_queued_bytes

  if ! set_android_vpn_mode true false; then
    outcome=failed
    outcome_detail="Android did not enable always-on mode for the network collection"
    record_step multi_network_always_on failed "$outcome_detail"
    return 1
  fi
  always_on_status="$state_dir/status-multi-network-always-on.json"
  if ! wait_for_automation_status \
    "$always_on_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not) and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .enabled and .phase == \"running\")] | length == 1) and ([.value.snapshot.networks[] | select(.id == \"$beta_id\" and .enabled and .phase == \"running\")] | length == 1)" \
    45; then
    outcome=failed
    outcome_detail="The service did not retain both networks when always-on became authoritative"
    record_step multi_network_always_on failed "$outcome_detail"
    return 1
  fi
  record_step multi_network_always_on passed \
    "Android always-on ownership retained both enabled networks"

  if ! android_automation select-network --es network_id "$alpha_id" \
    > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "select-network"
    ' "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not select alpha"
    record_step independent_disable failed "$outcome_detail"
    return 1
  fi
  selected_alpha_status="$state_dir/status-alpha-selected.json"
  if ! wait_for_automation_status \
    "$selected_alpha_status" \
    ".value.snapshot.selected_network_id == \"$alpha_id\" and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .selected)] | length == 1)"; then
    outcome=failed
    outcome_detail="Alpha did not become the selected management network"
    record_step independent_disable failed "$outcome_detail"
    return 1
  fi

  if ! android_automation set-network-enabled \
    --es network_id "$alpha_id" \
    --ez enabled false > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "set-network-enabled"
    ' "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept disabling alpha"
    record_step independent_disable failed "$outcome_detail"
    return 1
  fi
  disabled_status="$state_dir/status-alpha-disabled.json"
  if ! wait_for_automation_status \
    "$disabled_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == 2) and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .selected and (.enabled | not) and .phase == \"disabled\")] | length == 1) and ([.value.snapshot.networks[] | select(.id == \"$beta_id\" and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= 1)" \
    120 \
    || ! network_identity_signature_matches "$disabled_status"; then
    outcome=failed
    outcome_detail="Disabling alpha did not leave beta independently active"
    record_step independent_disable failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_transition_traffic_ready \
    disabled-beta \
    "while alpha was disabled" \
    "$fixture_secondary_packet_socket" \
    "$fixture_secondary_ipv4" \
    "$fixture_secondary_ipv6" \
    "$android_secondary_ipv4" \
    "$android_secondary_ipv6" \
    || ! measure_bidirectional_traffic \
    disabled-beta \
    "while alpha was disabled" \
    "$fixture_secondary_packet_socket" \
    "$fixture_secondary_ipv4" \
    "$fixture_secondary_ipv6" \
    "$android_secondary_ipv4" \
    "$android_secondary_ipv6"; then
    return 1
  fi
  if "$fixture_command" probe \
    --socket "$fixture_packet_socket" \
    --source "$fixture_ipv4" \
    --destination "$android_primary_ipv4" \
    --count 1 \
    --timeout-millis 2000 > "$disabled_probe" 2>/dev/null \
    || ! jq -e '
      .schema_version == 1 and (.ok | not) and .family == "ipv4" and
      .sent == 1 and .received == 0
    ' "$disabled_probe" >/dev/null 2>&1; then
    outcome=failed
    outcome_detail="Disabled alpha still accepted inbound overlay traffic"
    record_step independent_disable failed "$outcome_detail"
    return 1
  fi
  if adb_run shell ping -c 1 -W 2 "$fixture_ipv4" \
    > "$disabled_ping" 2>&1 \
    || [[ "$(ping_received_count "$disabled_ping")" -ne 0 ]]; then
    outcome=failed
    outcome_detail="Disabled alpha left a stale outbound route"
    record_step independent_disable failed "$outcome_detail"
    return 1
  fi
  record_step independent_disable passed \
    "Disabling selected alpha removed its routes while beta remained dual-stack reachable"

  process_before="$(
    adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r'
  )"
  if [[ ! "$process_before" =~ ^[0-9]+$ ]] \
    || ! adb_run install -r "$android_apk" >/dev/null \
    || ! wait_for_new_android_process "$process_before" process_after; then
    outcome=failed
    outcome_detail="APK replacement did not autonomously start a fresh always-on process"
    record_step disabled_set_update_restore failed "$outcome_detail"
    return 1
  fi
  disabled_update_status="$state_dir/status-disabled-set-after-update.json"
  if ! wait_for_automation_status \
    "$disabled_update_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not) and .value.snapshot.selected_network_id == \"$alpha_id\" and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .selected and (.enabled | not) and .phase == \"disabled\")] | length == 1) and ([.value.snapshot.networks[] | select(.id == \"$beta_id\" and .enabled and .phase == \"running\")] | length == 1)" \
    120 \
    || ! network_identity_signature_matches "$disabled_update_status"; then
    outcome=failed
    outcome_detail="The app update did not restore the exact enabled and selected network set"
    record_step disabled_set_update_restore failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_transition_traffic_ready \
    updated-disabled-beta \
    "after restoring the disabled set through an app update" \
    "$fixture_secondary_packet_socket" \
    "$fixture_secondary_ipv4" \
    "$fixture_secondary_ipv6" \
    "$android_secondary_ipv4" \
    "$android_secondary_ipv6" \
    || ! measure_bidirectional_traffic \
    updated-disabled-beta \
    "after restoring the disabled set through an app update" \
    "$fixture_secondary_packet_socket" \
    "$fixture_secondary_ipv4" \
    "$fixture_secondary_ipv6" \
    "$android_secondary_ipv4" \
    "$android_secondary_ipv6"; then
    return 1
  fi
  record_step disabled_set_update_restore passed \
    "A fresh app process restored selected-disabled alpha and active beta"

  if ! android_automation set-network-enabled \
    --es network_id "$alpha_id" \
    --ez enabled true > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "set-network-enabled"
    ' "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept re-enabling alpha"
    record_step independent_reenable failed "$outcome_detail"
    return 1
  fi
  reenabled_status="$state_dir/status-alpha-reenabled.json"
  if ! wait_for_automation_status \
    "$reenabled_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.busy | not) and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .selected and .enabled and .phase == \"running\")] | length == 1) and ([.value.snapshot.networks[] | select(.id == \"$beta_id\" and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= 2)" \
    180 \
    || ! network_identity_signature_matches "$reenabled_status"; then
    outcome=failed
    outcome_detail="Re-enabling alpha did not restore both isolated runtimes"
    record_step independent_reenable failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    reenabled \
    "after independently re-enabling alpha" \
    || ! measure_concurrent_multi_network_traffic \
    reenabled \
    "after independently re-enabling alpha" \
    reenabled_traffic_started \
    reenabled_traffic_duration; then
    return 1
  fi
  record_step independent_reenable passed \
    "Re-enabling alpha restored concurrent dual-stack traffic"

  underlay_generation="$(jq -r '.value.snapshot.runtime_generation' "$reenabled_status")"
  if ! adb_run shell svc wifi disable >/dev/null; then
    outcome=failed
    outcome_detail="Emulator Wi-Fi underlay could not be disabled"
    record_step multi_network_underlay_change failed "$outcome_detail"
    return 1
  fi
  cellular_status="$state_dir/status-multi-network-cellular.json"
  if ! wait_for_automation_status \
    "$cellular_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.runtime_generation == $underlay_generation) and (.value.snapshot.underlay.kind == \"cellular\") and .value.snapshot.underlay.validated and ([.value.snapshot.networks[] | select(.enabled and .phase == \"running\")] | length == 2) and (.value.snapshot.paths.connected_peers >= 2)" \
    180; then
    outcome=failed
    outcome_detail="Both networks did not recover on the cellular underlay"
    record_step multi_network_underlay_change failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    cellular \
    "after a Wi-Fi-to-cellular underlay transition" \
    || ! measure_concurrent_multi_network_traffic \
    cellular \
    "after a Wi-Fi-to-cellular underlay transition" \
    cellular_traffic_started \
    cellular_traffic_duration; then
    return 1
  fi
  if ! adb_run shell svc wifi enable >/dev/null; then
    outcome=failed
    outcome_detail="Emulator Wi-Fi underlay could not be restored"
    record_step multi_network_underlay_change failed "$outcome_detail"
    return 1
  fi
  wifi_status="$state_dir/status-multi-network-wifi-restored.json"
  if ! wait_for_automation_status \
    "$wifi_status" \
    ".value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.runtime_generation == $underlay_generation) and (.value.snapshot.underlay.kind == \"wifi\") and .value.snapshot.underlay.validated and ([.value.snapshot.networks[] | select(.enabled and .phase == \"running\")] | length == 2) and (.value.snapshot.paths.connected_peers >= 2)" \
    180; then
    outcome=failed
    outcome_detail="Both networks did not return to the preferred Wi-Fi underlay"
    record_step multi_network_underlay_change failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_multi_network_transition_traffic_ready \
    wifi-restored \
    "after returning to the preferred Wi-Fi underlay" \
    || ! measure_concurrent_multi_network_traffic \
      wifi-restored \
      "after returning to the preferred Wi-Fi underlay" \
      wifi_traffic_started \
      wifi_traffic_duration; then
    return 1
  fi
  record_step multi_network_underlay_change passed \
    "Both runtimes changed underlay twice without a shared-runtime restart"

  run_multi_network_restoration_and_isolation
}

run_network_workflow_scenario() {
  local add_xpath="//node[contains(@resource-id, ':id/add_network')]"
  local show_create_xpath="//node[contains(@resource-id, ':id/show_create_network')]"
  local show_join_xpath="//node[contains(@resource-id, ':id/show_join_network')]"
  local back_xpath="//node[contains(@resource-id, ':id/navigate_back')]"
  local code_xpath="//node[contains(@resource-id, ':id/join_code_input')]"
  local hostname_xpath="//node[contains(@resource-id, ':id/join_hostname_input')]"
  local join_xpath="//node[contains(@resource-id, ':id/join_network')]"
  local cancel_xpath="//node[contains(@resource-id, ':id/cancel_join')]"
  local detail_switch_xpath="//node[contains(@resource-id, ':id/detail_enabled')]"
  local home_switch_xpath="//node[contains(@resource-id, ':id/network_enabled')]"
  local pair_open="$state_dir/network-workflow-pair-open.json"
  local inviter_status="$state_dir/network-workflow-inviter-status.json"
  local pair_approved="$state_dir/network-workflow-pair-approved.json"
  local candidate_response="$state_dir/network-workflow-candidate.json"
  local joined_status="$state_dir/network-workflow-joined.json"
  local running_status="$state_dir/network-workflow-running.json"
  local disabled_status="$state_dir/network-workflow-disabled.json"
  local reenabled_status="$state_dir/network-workflow-reenabled.json"
  local ui_file pair_operation pair_code approval_id network_id night_status
  local expected_hostname="managed-test-phone"
  local actual_device_name

  if ! adb_run shell 'settings put global device_name "Managed Test Phone"' >/dev/null; then
    outcome=failed
    outcome_detail="Android did not accept the managed device name"
    record_step device_hostname failed "$outcome_detail"
    return 1
  fi
  actual_device_name="$(adb_run shell settings get global device_name | tr -d '\r')"
  if [[ "$actual_device_name" != "Managed Test Phone" ]]; then
    outcome=failed
    outcome_detail="Android did not retain the managed device name"
    record_step device_hostname failed "$outcome_detail"
    return 1
  fi
  if ! adb_run shell pm grant \
    org.hermeticfoundation.p2pvpn.debug \
    android.permission.POST_NOTIFICATIONS >/dev/null; then
    outcome=failed
    outcome_detail="Android did not grant the declared notification permission"
    record_step network_home failed "$outcome_detail"
    return 1
  fi
  adb_run shell am force-stop org.hermeticfoundation.p2pvpn.debug >/dev/null
  start_main_activity
  sleep 2
  capture_android_screen network-home-probe || true
  if ui_file="$(dump_android_settings_ui)"; then
    cp "$ui_file" "$output_dir/network-home-probe.xml"
  fi

  if ! wait_for_android_ui_xpath "$add_xpath" 30 \
    || ! wait_for_android_ui_xpath "//node[@text='No networks']" 5; then
    outcome=failed
    outcome_detail="The clean application did not open the network-list home screen"
    record_step network_home failed "$outcome_detail"
    return 1
  fi
  ui_file="$(dump_android_settings_ui)" || return 1
  if [[ "$(xmllint --nonet --xpath \
    "boolean(//node[@text='Connect' or @text='Disconnect'])" \
    "$ui_file" 2>/dev/null)" == true ]] \
    || ! capture_android_screen network-home-empty; then
    outcome=failed
    outcome_detail="The home screen still exposes a global connect or disconnect action"
    record_step network_home failed "$outcome_detail"
    return 1
  fi
  record_step network_home passed \
    "The clean home screen lists networks and exposes one Add Network action"

  if ! tap_android_ui_xpath "$add_xpath" \
    || ! wait_for_android_ui_xpath "$show_create_xpath" \
    || ! wait_for_android_ui_xpath "$show_join_xpath" \
    || ! capture_android_screen network-add; then
    outcome=failed
    outcome_detail="Add Network did not expose separate create and join paths"
    record_step add_navigation failed "$outcome_detail"
    return 1
  fi
  if ! tap_android_ui_xpath "$show_create_xpath" \
    || ! wait_for_android_ui_xpath \
      "//node[contains(@resource-id, ':id/network_name_input')]" \
    || ! wait_for_android_ui_xpath "//node[@text='Create network']" \
    || ! capture_android_screen network-create; then
    outcome=failed
    outcome_detail="The create-network path did not open its nested form"
    record_step create_navigation failed "$outcome_detail"
    return 1
  fi
  record_step create_navigation passed "Create Network opens a dedicated nested form"

  if ! tap_android_ui_xpath "$back_xpath" \
    || ! wait_for_android_ui_xpath "$show_join_xpath" \
    || ! tap_android_ui_xpath "$show_join_xpath" \
    || ! wait_for_android_ui_xpath "$code_xpath" \
    || ! wait_for_android_ui_xpath \
      "${hostname_xpath}[@text='$expected_hostname']" \
    || ! capture_android_screen network-join; then
    outcome=failed
    outcome_detail="The code-join form did not derive its hostname from the Android device name"
    record_step join_navigation failed "$outcome_detail"
    return 1
  fi
  record_step join_navigation passed \
    "Join by Code opens a dedicated form with a device-derived hostname"
  record_step device_hostname passed \
    "Managed Test Phone normalized to the requested hostname managed-test-phone"

  if ! "$p2p_vpn_command" pair open \
    --socket "$fixture_control_socket" \
    --expires-in-seconds 300 \
    --format json > "$pair_open" \
    || ! jq -e '
      (.operation_id | type == "string" and length > 0 and length <= 128) and
      (.code | type == "string" and length > 0 and length <= 64)
    ' "$pair_open" >/dev/null; then
    outcome=failed
    outcome_detail="The Linux fixture could not open a bounded pairing operation"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  pair_operation="$(jq -r '.operation_id' "$pair_open")"
  pair_code="$(jq -r '.code' "$pair_open")"

  if ! android_automation set-profile-join-candidate \
    --es peer_id "$fixture_peer_id" \
    --es address "$fixture_pairing_android_address" > "$candidate_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "set-profile-join-candidate"
    ' "$candidate_response" >/dev/null; then
    outcome=failed
    outcome_detail="Managed emulator discovery could not receive its bounded fixture hint"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi

  if ! input_android_ui_text "$code_xpath" "$pair_code" \
    || ! tap_android_ui_xpath "$join_xpath" \
    || ! wait_for_android_ui_xpath "$cancel_xpath" 30; then
    outcome=failed
    outcome_detail="The UI did not begin profile-free pairing from the supplied code"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_pair_status \
    "$inviter_status" \
    "$pair_operation" \
    '.phase == "awaiting_approval" and (.candidate.approval_id | type == "string" and length > 0 and length <= 128)' \
    180 \
    "$fixture_control_socket"; then
    outcome=failed
    outcome_detail="Profile-free pairing did not reach inviter approval"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  if [[ "$(jq -r '.candidate.requested_hostname // empty' "$inviter_status")" \
    != "$expected_hostname" ]]; then
    outcome=failed
    outcome_detail="Profile-free pairing did not send the device-derived hostname"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  approval_id="$(jq -r '.candidate.approval_id' "$inviter_status")"
  if ! "$p2p_vpn_command" pair approve \
    "$pair_operation" \
    "$approval_id" \
    --socket "$fixture_control_socket" \
    --format json > "$pair_approved" \
    || ! jq -e '.phase == "completed"' "$pair_approved" >/dev/null; then
    outcome=failed
    outcome_detail="The Linux fixture could not approve profile-free pairing"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_automation_status \
    "$joined_status" \
    ".value.snapshot.has_profile and .value.snapshot.profile_stored and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == 1) and ([.value.snapshot.networks[] | select(.name == \"$fixture_network\" and .hostname == \"$expected_hostname\" and .selected and (.enabled | not) and .phase == \"disabled\" and (.addresses | any(contains(\".\"))) and (.addresses | any(contains(\":\"))))] | length == 1)" \
    180; then
    outcome=failed
    outcome_detail="Code-only pairing did not create a disabled persisted network profile"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  network_id="$(jq -r '.value.snapshot.networks[0].id' "$joined_status")"
  if [[ ! "$network_id" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$ ]] \
    || ! wait_for_android_ui_xpath "//node[@text='$fixture_network']" 30 \
    || ! wait_for_android_ui_checked "$detail_switch_xpath" false \
    || ! capture_android_screen network-detail-disabled; then
    outcome=failed
    outcome_detail="The joined profile did not open as a disabled network detail page"
    record_step profile_free_join failed "$outcome_detail"
    return 1
  fi
  record_step profile_free_join passed \
    "The code alone created and persisted the signed network profile without a placeholder"

  if ! adb_run shell appops set \
    org.hermeticfoundation.p2pvpn.debug ACTIVATE_VPN allow >/dev/null \
    || ! tap_android_ui_xpath "$detail_switch_xpath"; then
    outcome=failed
    outcome_detail="The detail-page network switch could not request activation"
    record_step per_network_activation failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_automation_status \
    "$running_status" \
    ".value.snapshot.connected and .value.snapshot.connection_requested and (.value.snapshot.busy | not) and ([.value.snapshot.networks[] | select(.id == \"$network_id\" and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= 1)" \
    180 \
    || ! wait_for_android_ui_checked "$detail_switch_xpath" true \
    || ! wait_for_android_ui_xpath "//node[@text='Connected']" 30; then
    outcome=failed
    outcome_detail="The detail switch did not converge from desired enabled to observed Connected"
    record_step per_network_activation failed "$outcome_detail"
    return 1
  fi
  android_ipv4="$(jq -r '
    .value.snapshot.networks[0].addresses |
    [.[] | select(contains("."))][0] | split("/")[0]
  ' "$running_status")"
  android_ipv6="$(jq -r '
    .value.snapshot.networks[0].addresses |
    [.[] | select(contains(":"))][0] | split("/")[0]
  ' "$running_status")"
  if [[ ! "$android_ipv4" =~ ^[0-9.]{7,15}$ \
    || ! "$android_ipv6" =~ ^[0-9a-fA-F:]{2,45}$ ]]; then
    outcome=failed
    outcome_detail="The activated network did not expose dual-stack overlay addresses"
    record_step per_network_activation failed "$outcome_detail"
    return 1
  fi
  record_step per_network_activation passed \
    "The detail switch alone activated the shared VPN and reported Connected"

  if ! scroll_until_android_ui_xpath "//node[contains(@text, '$fixture_ipv4')]" 8; then
    outcome=failed
    outcome_detail="The detail page did not render the live fixture peer address"
    record_step live_peer_display failed "$outcome_detail"
    return 1
  fi
  ui_file="$(dump_android_settings_ui)" || return 1
  if [[ "$(xmllint --nonet --xpath \
    "boolean(//node[contains(@text, 'Connected |')] and //node[contains(@text, 'Membership:')])" \
    "$ui_file" 2>/dev/null)" != true ]] \
    || ! capture_android_screen network-live-peers; then
    outcome=failed
    outcome_detail="The live peer row omitted connection path or membership provenance"
    record_step live_peer_display failed "$outcome_detail"
    return 1
  fi
  record_step live_peer_display passed \
    "The detail page rendered bounded peer identity, address, path, and provenance data"

  if ! wait_for_transition_traffic_ready \
    network-workflow \
    "after UI activation" \
    || ! measure_bidirectional_traffic \
      network-workflow \
      "after UI activation"; then
    return 1
  fi
  if ! tap_android_ui_xpath "$back_xpath" \
    || ! wait_for_android_ui_checked "$home_switch_xpath" true \
    || ! tap_android_ui_xpath "$home_switch_xpath" \
    || ! wait_for_automation_status \
      "$disabled_status" \
      ".value.snapshot.has_profile and (.value.snapshot.connected | not) and (.value.snapshot.connection_requested | not) and ([.value.snapshot.networks[] | select(.id == \"$network_id\" and (.enabled | not) and .phase == \"disabled\")] | length == 1)" \
      90 \
    || ! wait_for_android_ui_checked "$home_switch_xpath" false \
    || ! wait_for_android_ui_xpath "//node[@text='Disabled']" 30 \
    || ! capture_android_screen network-home-disabled; then
    outcome=failed
    outcome_detail="Disabling the final home-screen switch did not stop the shared VPN"
    record_step final_network_disable failed "$outcome_detail"
    return 1
  fi
  record_step final_network_disable passed \
    "Disabling the final network stopped the VPN while preserving its profile"

  if ! tap_android_ui_xpath "$home_switch_xpath" \
    || ! wait_for_automation_status \
      "$reenabled_status" \
      ".value.snapshot.connected and .value.snapshot.connection_requested and ([.value.snapshot.networks[] | select(.id == \"$network_id\" and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= 1)" \
      180 \
    || ! wait_for_android_ui_checked "$home_switch_xpath" true \
    || ! wait_for_android_ui_xpath "//node[@text='Connected']" 30 \
    || ! wait_for_transition_traffic_ready \
      network-workflow-reenabled \
      "after switch re-enablement"; then
    outcome=failed
    outcome_detail="The preserved network did not reconnect from its home-screen switch"
    record_step network_reenable failed "$outcome_detail"
    return 1
  fi
  record_step network_reenable passed \
    "The same network profile reconnected without another pairing operation"

  if ! adb_run shell cmd uimode night yes >/dev/null; then
    outcome=failed
    outcome_detail="Android did not enable the requested system dark theme"
    record_step system_theme failed "$outcome_detail"
    return 1
  fi
  sleep 2
  night_status="$(adb_run shell cmd uimode night | tr -d '\r')"
  if [[ "$night_status" != *yes* ]] \
    || ! wait_for_android_ui_checked "$home_switch_xpath" true \
    || ! wait_for_android_ui_xpath "//node[@text='Connected']" 30 \
    || ! capture_android_screen network-home-dark; then
    outcome=failed
    outcome_detail="The running network did not survive the system dark-theme transition"
    record_step system_theme failed "$outcome_detail"
    return 1
  fi
  if ! adb_run shell cmd uimode night no >/dev/null; then
    outcome=failed
    outcome_detail="Android did not restore the managed emulator's light theme"
    record_step system_theme failed "$outcome_detail"
    return 1
  fi
  record_step system_theme passed \
    "The network list followed the system theme without changing desired state"

  jq \
    --arg hostname "$expected_hostname" '
    . + {
      network_workflow: {
        nested_navigation: true,
        profile_free_join: true,
        device_hostname: $hostname,
        detail_switch_activation: true,
        home_switch_disable: true,
        home_switch_reenable: true,
        system_theme: true,
        live_peer_display: true,
        bidirectional_dual_stack_traffic: true
      }
    }
  ' "$device_file" > "$device_file.updated"
  mv -f "$device_file.updated" "$device_file"

  outcome=passed
  outcome_detail="Nested UI, profile-free join, per-network controls, peers, and traffic passed"
}

run_multi_network_scenario() {
  local alpha_created="" alpha_migrated alpha_connected alpha_paired
  local beta_created="" beta_ready beta_paired both_running
  local alpha_id alpha_peer_id alpha_hostname
  local beta_id beta_peer_id beta_hostname
  local command_response="$state_dir/multi-network-command.json"
  local primary_ipv4_attempts=0 primary_ipv6_attempts=0
  local secondary_ipv4_attempts=0 secondary_ipv6_attempts=0
  local android_primary_ipv4_attempts=0 android_primary_ipv6_attempts=0
  local android_secondary_ipv4_attempts=0 android_secondary_ipv6_attempts=0
  local initial_traffic_started=0 initial_traffic_duration=0
  local overlap_traffic_started=0 overlap_traffic_duration=0
  local overlap_status overlap_generation_before runtime_entries

  if ! create_isolated_android_profile \
    alpha \
    android-e2e-alpha \
    "$fixture_bootstrap_peer" \
    "$fixture_bootstrap_address" \
    "$fixture_kademlia_protocol" \
    1 \
    alpha_created; then
    return 1
  fi
  if ! jq -e '
    (.value.snapshot.networks[0].peer_id | type == "string" and length > 0 and length <= 256) and
    (.value.snapshot.networks[0].hostname | test("^android-[0-9a-f]{16}$")) and
    (.value.snapshot.networks[0].addresses | any(contains("."))) and
    (.value.snapshot.networks[0].addresses | any(contains(":")))
  ' "$alpha_created" >/dev/null; then
    outcome=failed
    outcome_detail="The alpha profile does not expose valid dual-stack identity metadata"
    record_step legacy_collection_migration failed "$outcome_detail"
    return 1
  fi

  if ! android_automation stage-legacy-profile > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "stage-legacy-profile"
    ' "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation could not stage the encrypted legacy profile"
    record_step legacy_collection_migration failed "$outcome_detail"
    return 1
  fi
  if ! wait_for_automation_status \
    "$state_dir/status-legacy-staged.json" \
    '(.value.snapshot.busy | not) and (.value.snapshot.connection_detail | contains("Legacy profile staged"))'; then
    outcome=failed
    outcome_detail="The encrypted profile was not staged in the legacy format"
    record_step legacy_collection_migration failed "$outcome_detail"
    return 1
  fi

  adb_run shell am force-stop org.hermeticfoundation.p2pvpn.debug >/dev/null
  start_main_activity
  alpha_migrated="$state_dir/status-alpha-migrated.json"
  if ! wait_for_automation_status \
    "$alpha_migrated" \
    '.value.snapshot.has_profile and .value.snapshot.profile_stored and (.value.snapshot.profile_unreadable | not) and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == 1) and .value.snapshot.networks[0].selected and (.value.snapshot.networks[0].enabled | not) and .value.snapshot.networks[0].phase == "disabled"' \
    60 \
    || ! jq -e -s '
      .[0].value.snapshot.networks[0].name == .[1].value.snapshot.networks[0].name and
      .[0].value.snapshot.networks[0].peer_id == .[1].value.snapshot.networks[0].peer_id and
      .[0].value.snapshot.networks[0].hostname == .[1].value.snapshot.networks[0].hostname and
      .[0].value.snapshot.networks[0].addresses == .[1].value.snapshot.networks[0].addresses
    ' "$alpha_created" "$alpha_migrated" >/dev/null; then
    outcome=failed
    outcome_detail="Legacy profile migration did not preserve the alpha identity"
    record_step legacy_collection_migration failed "$outcome_detail"
    return 1
  fi
  alpha_id="$(jq -r '.value.snapshot.networks[0].id' "$alpha_migrated")"
  alpha_peer_id="$(jq -r '.value.snapshot.networks[0].peer_id' "$alpha_migrated")"
  alpha_hostname="$(jq -r '.value.snapshot.networks[0].hostname' "$alpha_migrated")"
  if [[ ! "$alpha_id" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$ ]]; then
    outcome=failed
    outcome_detail="Legacy profile migration produced an invalid network identifier"
    record_step legacy_collection_migration failed "$outcome_detail"
    return 1
  fi
  record_step legacy_collection_migration passed \
    "Encrypted legacy storage migrated disabled without changing identity or addresses"

  if ! adb_run shell appops set \
    org.hermeticfoundation.p2pvpn.debug ACTIVATE_VPN allow >/dev/null; then
    outcome=failed
    outcome_detail="ADB could not grant emulator VPN consent"
    record_step vpn_consent failed "$outcome_detail"
    return 1
  fi
  if ! android_automation set-network-enabled \
    --es network_id "$alpha_id" \
    --ez enabled true > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and
      .value.command == "set-network-enabled"
    ' "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not enable the migrated alpha network"
    record_step vpn_connect failed "$outcome_detail"
    return 1
  fi
  alpha_connected="$state_dir/status-alpha-connected.json"
  if ! wait_for_automation_status \
    "$alpha_connected" \
    ".value.snapshot.connected and (.value.snapshot.busy | not) and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .phase == \"running\")] | length == 1)" \
    90; then
    outcome=failed
    outcome_detail="The shared Android VPN did not start alpha"
    record_step vpn_connect failed "$outcome_detail"
    return 1
  fi
  record_step vpn_consent passed "ADB authorized the normal VpnService consent gate"
  record_step vpn_connect passed "Enabling alpha started the shared Android VPN"

  if ! pair_selected_network \
    alpha \
    "$alpha_id" \
    "$alpha_peer_id" \
    "$alpha_hostname" \
    "$fixture_control_socket" \
    1 \
    alpha_paired; then
    return 1
  fi
  : "$alpha_paired"

  if ! create_isolated_android_profile \
    beta \
    android-e2e-beta \
    "$fixture_secondary_bootstrap_peer" \
    "$fixture_secondary_bootstrap_address" \
    "$fixture_secondary_kademlia_protocol" \
    2 \
    beta_created; then
    return 1
  fi
  : "$beta_created"
  beta_ready="$state_dir/status-beta-ready.json"
  if ! wait_for_automation_status \
    "$beta_ready" \
    '.value.snapshot.connected and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == 2) and ([.value.snapshot.networks[] | select(.name == "android-e2e-alpha" and .enabled and .phase == "running")] | length == 1) and ([.value.snapshot.networks[] | select(.name == "android-e2e-beta" and .selected and .enabled and .phase == "running")] | length == 1)' \
    120; then
    outcome=failed
    outcome_detail="The shared VPN did not activate alpha and beta together"
    record_step beta_activation failed "$outcome_detail"
    return 1
  fi
  alpha_id="$(
    jq -r '.value.snapshot.networks[] | select(.name == "android-e2e-alpha") | .id' \
      "$beta_ready"
  )"
  beta_id="$(
    jq -r '.value.snapshot.networks[] | select(.name == "android-e2e-beta") | .id' \
      "$beta_ready"
  )"
  beta_peer_id="$(
    jq -r --arg id "$beta_id" \
      '.value.snapshot.networks[] | select(.id == $id) | .peer_id' \
      "$beta_ready"
  )"
  beta_hostname="$(
    jq -r --arg id "$beta_id" \
      '.value.snapshot.networks[] | select(.id == $id) | .hostname' \
      "$beta_ready"
  )"
  if [[ "$alpha_id" == "$beta_id" || "$alpha_peer_id" == "$beta_peer_id" \
    || ! "$beta_hostname" =~ ^android-[0-9a-f]{16}$ ]]; then
    outcome=failed
    outcome_detail="The two Android networks did not retain isolated identities"
    record_step beta_activation failed "$outcome_detail"
    return 1
  fi
  record_step beta_activation passed \
    "The shared TUN activated two independently identified network runtimes"

  if ! pair_selected_network \
    beta \
    "$beta_id" \
    "$beta_peer_id" \
    "$beta_hostname" \
    "$fixture_secondary_control_socket" \
    2 \
    beta_paired; then
    return 1
  fi
  : "$beta_paired"

  both_running="$state_dir/status-both-running.json"
  if ! wait_for_automation_status \
    "$both_running" \
    ".value.snapshot.connected and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == 2) and ([.value.snapshot.networks[] | select(.id == \"$alpha_id\" and .enabled and .phase == \"running\")] | length == 1) and ([.value.snapshot.networks[] | select(.id == \"$beta_id\" and .selected and .enabled and .phase == \"running\")] | length == 1) and (.value.snapshot.paths.connected_peers >= 2)" \
    180; then
    outcome=failed
    outcome_detail="Both independently paired networks did not remain active"
    record_step concurrent_activation failed "$outcome_detail"
    return 1
  fi

  android_primary_ipv4="$(
    jq -r --arg id "$alpha_id" '
      .value.snapshot.networks[] | select(.id == $id) |
      [.addresses[] | select(contains("."))][0] | split("/")[0]
    ' "$both_running"
  )"
  android_primary_ipv6="$(
    jq -r --arg id "$alpha_id" '
      .value.snapshot.networks[] | select(.id == $id) |
      [.addresses[] | select(contains(":"))][0] | split("/")[0]
    ' "$both_running"
  )"
  android_secondary_ipv4="$(
    jq -r --arg id "$beta_id" '
      .value.snapshot.networks[] | select(.id == $id) |
      [.addresses[] | select(contains("."))][0] | split("/")[0]
    ' "$both_running"
  )"
  android_secondary_ipv6="$(
    jq -r --arg id "$beta_id" '
      .value.snapshot.networks[] | select(.id == $id) |
      [.addresses[] | select(contains(":"))][0] | split("/")[0]
    ' "$both_running"
  )"
  if [[ ! "$android_primary_ipv4" =~ ^[0-9.]{7,15}$ \
    || ! "$android_primary_ipv6" =~ ^[0-9a-fA-F:]{2,45}$ \
    || ! "$android_secondary_ipv4" =~ ^[0-9.]{7,15}$ \
    || ! "$android_secondary_ipv6" =~ ^[0-9a-fA-F:]{2,45}$ \
    || "$android_primary_ipv4" == "$android_secondary_ipv4" \
    || "$android_primary_ipv6" == "$android_secondary_ipv6" ]]; then
    outcome=failed
    outcome_detail="Concurrent networks do not expose distinct valid overlay addresses"
    record_step concurrent_activation failed "$outcome_detail"
    return 1
  fi
  record_step concurrent_activation passed \
    "Both isolated runtimes are running behind one shared TUN"

  if ! wait_for_fixture_probe_ready \
    "$fixture_ipv4" "$android_primary_ipv4" ipv4 \
    "$state_dir/alpha-linux-ipv4-readiness.json" primary_ipv4_attempts \
    "$fixture_packet_socket" \
    || ! wait_for_fixture_probe_ready \
      "$fixture_ipv6" "$android_primary_ipv6" ipv6 \
      "$state_dir/alpha-linux-ipv6-readiness.json" primary_ipv6_attempts \
      "$fixture_packet_socket" \
    || ! wait_for_fixture_probe_ready \
      "$fixture_secondary_ipv4" "$android_secondary_ipv4" ipv4 \
      "$state_dir/beta-linux-ipv4-readiness.json" secondary_ipv4_attempts \
      "$fixture_secondary_packet_socket" \
    || ! wait_for_fixture_probe_ready \
      "$fixture_secondary_ipv6" "$android_secondary_ipv6" ipv6 \
      "$state_dir/beta-linux-ipv6-readiness.json" secondary_ipv6_attempts \
      "$fixture_secondary_packet_socket" \
    || ! wait_for_android_ping_ready \
      ipv4 "$fixture_ipv4" "$state_dir/alpha-android-ipv4-readiness.txt" \
      android_primary_ipv4_attempts \
    || ! wait_for_android_ping_ready \
      ipv6 "$fixture_ipv6" "$state_dir/alpha-android-ipv6-readiness.txt" \
      android_primary_ipv6_attempts \
    || ! wait_for_android_ping_ready \
      ipv4 "$fixture_secondary_ipv4" "$state_dir/beta-android-ipv4-readiness.txt" \
      android_secondary_ipv4_attempts \
    || ! wait_for_android_ping_ready \
      ipv6 "$fixture_secondary_ipv6" "$state_dir/beta-android-ipv6-readiness.txt" \
      android_secondary_ipv6_attempts; then
    outcome=failed
    outcome_detail="Concurrent bidirectional dual-stack forwarding did not converge"
    record_step concurrent_traffic_readiness failed "$outcome_detail"
    return 1
  fi
  record_step concurrent_traffic_readiness passed \
    "Both networks converged independently in both directions and address families"

  if ! measure_concurrent_multi_network_traffic \
    initial \
    "during the initial shared-TUN measurement" \
    initial_traffic_started \
    initial_traffic_duration; then
    return 1
  fi

  jq '
    .value.snapshot.networks |
    map({id, name, hostname, peer_id, addresses, enabled, selected})
  ' "$both_running" > "$state_dir/multi-network-signature.json"
  jq '
    .value.snapshot.networks |
    map({id, name, hostname, peer_id, addresses}) | sort_by(.id)
  ' "$both_running" > "$state_dir/multi-network-identity-signature.json"
  overlap_generation_before="$(jq -r '.value.snapshot.runtime_generation' "$both_running")"

  if ! android_automation create-profile \
    --es network android-e2e-overlap \
    --es bootstrap_peer_id "$fixture_secondary_bootstrap_peer" \
    --es bootstrap_address "$fixture_secondary_bootstrap_address" \
    --es kademlia_protocol "$fixture_secondary_kademlia_protocol" \
    --es additional_route "$fixture_secondary_ipv4/32" > "$command_response" \
    || ! jq -e '
      .schema_version == 1 and .ok and .value.accepted and .value.command == "create-profile"
    ' "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept the overlap rejection probe"
    record_step overlap_rejection failed "$outcome_detail"
    return 1
  fi
  overlap_status="$state_dir/status-overlap-rejected.json"
  if ! wait_for_automation_status \
    "$overlap_status" \
    ".value.snapshot.connected and (.value.snapshot.busy | not) and (.value.snapshot.networks | length == 2) and (.value.snapshot.runtime_generation == $overlap_generation_before) and (.value.snapshot.connection_detail | ascii_downcase | contains(\"overlap\"))" \
    60 \
    || ! jq -e -s '
      .[0] == (.[1].value.snapshot.networks |
        map({id, name, hostname, peer_id, addresses, enabled, selected}))
    ' "$state_dir/multi-network-signature.json" "$overlap_status" >/dev/null; then
    outcome=failed
    outcome_detail="An overlapping third network changed the live collection or runtime"
    record_step overlap_rejection failed "$outcome_detail"
    return 1
  fi

  runtime_entries="$state_dir/runtime-entries.txt"
  if ! adb_run exec-out run-as org.hermeticfoundation.p2pvpn.debug \
    ls -1 no_backup/runtime > "$runtime_entries" \
    || [[ "$(grep -Ec '^[0-9a-f-]{36}$' "$runtime_entries")" -ne 2 ]] \
    || ! grep -Fxq "$alpha_id" "$runtime_entries" \
    || ! grep -Fxq "$beta_id" "$runtime_entries"; then
    outcome=failed
    outcome_detail="Rejected profile runtime storage was not cleaned up"
    record_step overlap_rejection failed "$outcome_detail"
    return 1
  fi
  if ! measure_concurrent_multi_network_traffic \
    after-overlap \
    "after rejecting an overlapping network" \
    overlap_traffic_started \
    overlap_traffic_duration; then
    return 1
  fi
  record_step overlap_rejection passed \
    "Overlapping routes were rejected before persistence or live-runtime mutation"

  run_multi_network_lifecycle
}

ping_received_count() {
  local path="$1"
  local match
  match="$(grep -Eo '[0-9]+ (packets )?received' "$path" | tail -n 1 || true)"
  if [[ "$match" =~ ^([0-9]+) ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
  else
    printf '0\n'
  fi
}

start_main_activity() {
  adb_run shell am start \
    -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.MainActivity \
    >/dev/null
}

bound_file() {
  local path="$1"
  [[ -f "$path" ]] || return 0
  local size
  size="$(wc -c < "$path")"
  if (( size <= maximum_log_bytes )); then
    return 0
  fi
  tail -c "$maximum_log_bytes" "$path" > "$path.bounded"
  mv -f "$path.bounded" "$path"
}

sanitize_emulator_log() {
  [[ -f "$emulator_log" ]] || return 0
  sed -E \
    -e '/Sending adb public key/d' \
    -e '/androidboot\.qemu\.adb\.pubkey=/d' \
    -e 's#/home/[^/[:space:]]+#/home/REDACTED#g' \
    -e 's#/tmp/android-[^/[:space:]]+#/tmp/android-REDACTED#g' \
    "$emulator_log" > "$emulator_log.sanitized"
  mv -f "$emulator_log.sanitized" "$emulator_log"
}

sanitize_runtime_log() {
  local log_file="$1"
  [[ -f "$log_file" ]] || return 0
  sed -E \
    -e 's#/home/[^/[:space:]]+#/home/REDACTED#g' \
    -e 's#/tmp/p2p-vpn-android-e2e-state\.[^/[:space:]]+#/tmp/p2p-vpn-android-e2e-state.REDACTED#g' \
    -e 's/[A-Z2-9]{4}(-[A-Z2-9]{4}){3}/PAIRING-CODE-REDACTED/g' \
    -e 's/Qm[1-9A-HJ-NP-Za-km-z]{44}/PEER-ID-REDACTED/g' \
    -e 's/12D3KooW[A-Za-z0-9]+/PEER-ID-REDACTED/g' \
    -e 's/(^|[^[:xdigit:]])[[:xdigit:]]{64}([^[:xdigit:]]|$)/\1OVERLAY-ID-REDACTED\2/g' \
    -e 's#/members/[^[:space:]"}]*/membership-records#/members/MEMBERSHIP-TAG-REDACTED/membership-records#g' \
    -e '/(membership_key|membership_tag: Some\(|member_public_key|private_key|certificate_der: Some\(\[|signature:)/d' \
    -e 's#/dns(4|6)?/[^/[:space:]"}]+#/dns/UNDERLAY-REDACTED#g' \
    -e 's#/ip6/[^/[:space:]"}]+#/ip6/IPV6-REDACTED#g' \
    -e 's/(^|[^[:alnum:]_])(\[?[[:xdigit:]]{1,4}(:[[:xdigit:]]{0,4}){2,7}\]?)([^[:alnum:]_]|$)/\1IPV6-REDACTED\4/g' \
    -e 's/(^|[^[:alnum:]_])(\[?::[[:xdigit:]:]+\]?)([^[:alnum:]_]|$)/\1IPV6-REDACTED\3/g' \
    -e 's/([0-9]{1,3}\.){3}[0-9]{1,3}/IPV4-REDACTED/g' \
    "$log_file" > "$log_file.sanitized"
  mv -f "$log_file.sanitized" "$log_file"
}

sanitize_fixture_log() {
  sanitize_runtime_log "$fixture_log"
  sanitize_runtime_log "$fixture_secondary_log"
}

sanitize_android_log() {
  sanitize_runtime_log "$android_log"
}

sanitize_logs() {
  sanitize_emulator_log
  sanitize_fixture_log
  sanitize_android_log
}

logs_are_redacted() {
  local log_file
  for log_file in "$fixture_log" "$fixture_secondary_log" "$android_log"; do
    if grep -Eq \
      'Qm[1-9A-HJ-NP-Za-km-z]{44}|12D3KooW[A-Za-z0-9]+|(^|[^[:xdigit:]])[[:xdigit:]]{64}([^[:xdigit:]]|$)|[A-Z2-9]{4}(-[A-Z2-9]{4}){3}|membership_tag: Some\(|member_public_key|private_key|certificate_der: Some\(\[|signature:|([0-9]{1,3}\.){3}[0-9]{1,3}|/ip6/[^I]|/dns(4|6)?/[^U]|/members/[^M]|\[[[:xdigit:]]*:[[:xdigit:]:]*\]' \
      "$log_file"; then
      return 1
    fi
  done
}

diagnostic_report_is_valid() {
  local report_file="$1"
  local report_size
  report_size="$(wc -c < "$report_file")"
  if (( report_size > 64 * 1024 )); then
    return 1
  fi

  jq -e '
    def exact_keys($expected):
      type == "object" and ((keys | sort) == ($expected | sort));
    def natural:
      type == "number" and . >= 0 and . == floor;
    def boolean:
      type == "boolean";

    exact_keys([
      "app", "drops", "events", "generated_at", "kind", "lifecycle",
      "pairing", "paths", "privacy", "queue", "resources",
      "schema_version", "underlay"
    ]) and
    .schema_version == 1 and
    .kind == "p2p-vpn-android-diagnostics" and
    (.generated_at |
      type == "string" and
      test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?Z$")) and
    (.app |
      exact_keys(["android_api", "version"]) and
      (.android_api | natural) and
      (.version |
        type == "string" and
        length >= 1 and length <= 64 and
        test("^[A-Za-z0-9._+()-]+$") and
        (startswith("12D3KooW") | not))) and
    (.lifecycle |
      exact_keys([
        "always_on", "busy", "connected", "connection_requested", "lockdown",
        "profile_readable", "profile_stored", "runtime_generation",
        "service_uptime_millis"
      ]) and
      (.service_uptime_millis | natural) and
      (.runtime_generation | natural) and
      (.profile_stored | boolean) and
      (.profile_readable | boolean) and
      (.connection_requested | boolean) and
      (.connected | boolean) and
      (.always_on | boolean) and
      (.lockdown | boolean) and
      (.busy | boolean)) and
    (.underlay |
      exact_keys([
        "available_networks", "kind", "recoveries", "runtime_recovery_failures",
        "runtime_recovery_requests", "selected_losses", "selection_changes",
        "validated"
      ]) and
      (.kind | IN("unknown", "none", "ethernet", "wifi", "cellular", "bluetooth", "other")) and
      (.validated | boolean) and
      (.available_networks | natural) and
      (.selection_changes | natural) and
      (.selected_losses | natural) and
      (.recoveries | natural) and
      (.runtime_recovery_requests | natural) and
      (.runtime_recovery_failures | natural)) and
    (.paths |
      exact_keys([
        "connected_peers", "direct_quic_datagram", "direct_quic_stream",
        "direct_tcp_stream", "direct_udp_datagram", "packet_plane_quic_sessions",
        "peers_without_supported_path", "promotions_to_direct",
        "public_routing_peers", "relay"
      ]) and all(.[]; natural)) and
    (.queue |
      exact_keys([
        "blocked_no_supported_path_events", "blocked_packet_window_events",
        "oldest_packet_age_millis", "queued_bytes", "queued_packets"
      ]) and all(.[]; natural)) and
    (.drops |
      exact_keys([
        "expired_bytes", "expired_packets", "inbound_packets", "outbound_packets",
        "packet_plane_datagrams", "packet_plane_path_demotions",
        "path_fallbacks_to_relay", "queue_bytes", "queue_packets",
        "stream_path_demotions"
      ]) and all(.[]; natural)) and
    (.resources |
      exact_keys([
        "active_threads", "java_heap_max_bytes", "java_heap_used_bytes",
        "private_dirty_kib", "process_cpu_millis", "total_pss_kib"
      ]) and all(.[]; natural)) and
    (.pairing |
      exact_keys(["candidate_pending", "operation_active"]) and
      (.candidate_pending | boolean) and
      (.operation_active | boolean)) and
    (.events |
      exact_keys(["discarded", "items"]) and
      (.discarded | natural) and
      (.items | type == "array" and length <= 64) and
      all(.items[];
        exact_keys(["name", "sequence", "since_service_start_millis"]) and
        (.sequence | natural) and
        (.since_service_start_millis | natural) and
        (.name | type == "string" and test("^[a-z0-9_]{1,64}$")))) and
    (.privacy |
      exact_keys([
        "identity_material", "pairing_secrets", "peers", "underlay_addresses"
      ]) and
      .identity_material == "excluded" and
      .peers == "excluded" and
      .pairing_secrets == "excluded" and
      .underlay_addresses == "excluded")
  ' "$report_file" >/dev/null
}

collect_android_diagnostics() {
  [[ -n "$emulator_serial" && ${#adb[@]} -gt 0 ]] || return 0
  local adb_timeout_seconds="$cleanup_adb_timeout_seconds"
  if [[ "$(adb_run get-state 2>/dev/null || true)" != device ]]; then
    return 0
  fi

  adb_run logcat -d -v epoch -s 'p2p-vpn:I' '*:S' \
    > "$android_log" 2>&1 || true

  local final_status="$output_dir/.final-status.json"
  local coarse_status="$output_dir/.coarse-final-status.json"
  if android_automation status > "$final_status" 2>/dev/null \
    && jq -e '.schema_version == 1 and .ok and .value.service_ready' \
      "$final_status" >/dev/null 2>&1; then
    jq '{
      connected: (.value.snapshot.connected // false),
      busy: (.value.snapshot.busy // false),
      runtime_generation: (.value.snapshot.runtime_generation // 0),
      underlay: (.value.snapshot.underlay // {}),
      paths: (.value.snapshot.paths // {})
    }' "$final_status" > "$coarse_status"
    jq --slurpfile final_runtime "$coarse_status" \
      '.diagnostics = ((.diagnostics // {}) + {final_runtime: $final_runtime[0]})' \
      "$device_file" > "$device_file.updated"
    mv -f "$device_file.updated" "$device_file"
  fi

  if jq -e '.debug_automation == true' "$device_file" >/dev/null 2>&1; then
    diagnostic_report_required=true
    cleanup_diagnostic_report_redacted=false
    local diagnostic_response="$output_dir/.diagnostic-response.json"
    local diagnostic_report="$output_dir/.diagnostic-report.json"
    for _ in $(seq 1 3); do
      if android_automation diagnostics > "$diagnostic_response" 2>/dev/null \
        && jq -e '.schema_version == 1 and .ok and .value.service_ready' \
          "$diagnostic_response" >/dev/null 2>&1; then
        if jq -c '.value.report' "$diagnostic_response" > "$diagnostic_report" \
          && diagnostic_report_is_valid "$diagnostic_report"; then
          jq --slurpfile diagnostic_report "$diagnostic_report" \
            '.diagnostics = ((.diagnostics // {}) + {export: $diagnostic_report[0]})' \
            "$device_file" > "$device_file.updated"
          mv -f "$device_file.updated" "$device_file"
          cleanup_diagnostic_report_redacted=true
        fi
        break
      fi
      sleep 1
    done
    rm -f "$diagnostic_response" "$diagnostic_report"
  fi
  rm -f "$final_status" "$coarse_status"
}

dump_android_settings_ui() {
  local ui_file="$state_dir/android-settings-ui.xml"
  adb_run shell uiautomator dump /sdcard/p2p-vpn-window.xml >/dev/null 2>&1 \
    || return 1
  adb_run exec-out cat /sdcard/p2p-vpn-window.xml > "$ui_file" || return 1
  xmllint --nonet --noout "$ui_file" >/dev/null 2>&1 || return 1
  printf '%s\n' "$ui_file"
}

capture_android_screen() {
  local name="$1"
  [[ "$name" =~ ^[a-z0-9-]{1,64}$ ]] || return 1
  local destination="$output_dir/$name.png"
  adb_run exec-out screencap -p > "$destination" || return 1
  local signature
  signature="$(head -c 8 "$destination" | od -An -tx1 | tr -d ' \n')"
  [[ "$signature" == 89504e470d0a1a0a ]] || {
    rm -f "$destination"
    return 1
  }
}

wait_for_android_ui_xpath() {
  local xpath="$1"
  local attempts="${2:-10}"
  local ui_file
  for _ in $(seq 1 "$attempts"); do
    if ui_file="$(dump_android_settings_ui)" \
      && [[ "$(xmllint --nonet --xpath "boolean(($xpath)[1])" "$ui_file" 2>/dev/null)" == true ]]; then
      return 0
    fi
    sleep 1
  done
  return 1
}

tap_android_ui_xpath() {
  local xpath="$1"
  local ui_file bounds
  ui_file="$(dump_android_settings_ui)" || return 1
  bounds="$(
    xmllint --nonet --xpath "string(($xpath)[1]/@bounds)" "$ui_file" 2>/dev/null
  )"
  if [[ ! "$bounds" =~ ^\[([0-9]+),([0-9]+)\]\[([0-9]+),([0-9]+)\]$ ]]; then
    return 1
  fi
  local x=$(((BASH_REMATCH[1] + BASH_REMATCH[3]) / 2))
  local y=$(((BASH_REMATCH[2] + BASH_REMATCH[4]) / 2))
  adb_run shell input tap "$x" "$y" >/dev/null
}

android_ui_checked() {
  local xpath="$1"
  local ui_file
  ui_file="$(dump_android_settings_ui)" || return 1
  xmllint --nonet --xpath "string(($xpath)[1]/@checked)" "$ui_file" 2>/dev/null
}

wait_for_android_ui_checked() {
  local xpath="$1"
  local expected="$2"
  local attempts="${3:-30}"
  for _ in $(seq 1 "$attempts"); do
    if [[ "$(android_ui_checked "$xpath" 2>/dev/null || true)" == "$expected" ]]; then
      return 0
    fi
    sleep 1
  done
  return 1
}

input_android_ui_text() {
  local xpath="$1"
  local value="$2"
  [[ "$value" =~ ^[A-Za-z0-9._-]{1,128}$ ]] || return 1
  tap_android_ui_xpath "$xpath" || return 1
  adb_run shell input text "$value" >/dev/null || return 1
  adb_run shell input keyevent KEYCODE_BACK >/dev/null
}

scroll_until_android_ui_xpath() {
  local xpath="$1"
  local attempts="${2:-6}"
  local size width height x start_y end_y
  size="$(adb_run shell wm size | tr -d '\r' | sed -nE 's/.*: ([0-9]+)x([0-9]+)$/\1 \2/p' | tail -n 1)"
  read -r width height <<< "$size"
  [[ "$width" =~ ^[0-9]+$ && "$height" =~ ^[0-9]+$ ]] || return 1
  x=$((width / 2))
  start_y=$((height * 4 / 5))
  end_y=$((height / 5))
  for _ in $(seq 1 "$attempts"); do
    if wait_for_android_ui_xpath "$xpath" 1; then
      return 0
    fi
    adb_run shell input swipe "$x" "$start_y" "$x" "$end_y" 350 >/dev/null || return 1
    sleep 1
  done
  wait_for_android_ui_xpath "$xpath" 1
}

vpn_preference_xpath() {
  local label="$1"
  printf "//node[@clickable='true' and .//node[@text='%s']]\n" "$label"
}

vpn_preference_checked() {
  local label="$1"
  local ui_file row_xpath
  row_xpath="$(vpn_preference_xpath "$label")"
  ui_file="$(dump_android_settings_ui)" || return 1
  xmllint --nonet --xpath \
    "string(($row_xpath)[1]//node[@class='android.widget.Switch'][1]/@checked)" \
    "$ui_file" 2>/dev/null
}

wait_for_vpn_preference_state() {
  local label="$1"
  local expected="$2"
  local attempts="${3:-10}"
  for _ in $(seq 1 "$attempts"); do
    if [[ "$(vpn_preference_checked "$label" 2>/dev/null || true)" == "$expected" ]]; then
      return 0
    fi
    sleep 1
  done
  return 1
}

open_android_vpn_app_settings() {
  local settings_button="//node[@resource-id='com.android.settings:id/settings_button' and @content-desc='Settings']"
  adb_run shell am force-stop com.android.settings >/dev/null || return 1
  adb_run shell am start -a android.settings.VPN_SETTINGS >/dev/null || return 1
  wait_for_android_ui_xpath "$settings_button" || return 1
  tap_android_ui_xpath "$settings_button" || return 1
  wait_for_android_ui_xpath "//node[@text='Always-on VPN']"
}

set_android_vpn_mode() {
  local desired_always_on="$1"
  local desired_lockdown="$2"
  if [[ "$test_mode" == 1 ]]; then
    if [[ "$desired_always_on" == true ]]; then
      adb_run shell settings put secure always_on_vpn_app \
        org.hermeticfoundation.p2pvpn.debug >/dev/null || return 1
      adb_run shell settings put secure always_on_vpn_lockdown \
        "$([[ "$desired_lockdown" == true ]] && printf 1 || printf 0)" >/dev/null \
        || return 1
      always_on_configured=true
    else
      adb_run shell settings put secure always_on_vpn_lockdown 0 >/dev/null || return 1
      adb_run shell settings delete secure always_on_vpn_app >/dev/null || return 1
      always_on_configured=false
    fi
    return 0
  fi

  open_android_vpn_app_settings || return 1
  local always_on_row lockdown_row
  always_on_row="$(vpn_preference_xpath "Always-on VPN")"
  lockdown_row="$(vpn_preference_xpath "Block connections without VPN")"

  if [[ "$desired_always_on" == true \
    && "$(vpn_preference_checked "Always-on VPN" 2>/dev/null || true)" != true ]]; then
    tap_android_ui_xpath "$always_on_row" || return 1
    always_on_configured=true
    wait_for_vpn_preference_state "Always-on VPN" true || return 1
  fi

  if [[ "$desired_lockdown" == true \
    && "$(vpn_preference_checked "Block connections without VPN" 2>/dev/null || true)" != true ]]; then
    tap_android_ui_xpath "$lockdown_row" || return 1
    local confirm_button="//node[@resource-id='android:id/button1' and @text='TURN ON']"
    wait_for_android_ui_xpath "$confirm_button" || return 1
    tap_android_ui_xpath "$confirm_button" || return 1
    wait_for_vpn_preference_state "Block connections without VPN" true || return 1
  elif [[ "$desired_lockdown" == false \
    && "$(vpn_preference_checked "Block connections without VPN" 2>/dev/null || true)" == true ]]; then
    tap_android_ui_xpath "$lockdown_row" || return 1
    wait_for_vpn_preference_state "Block connections without VPN" false || return 1
  fi

  if [[ "$desired_always_on" == false \
    && "$(vpn_preference_checked "Always-on VPN" 2>/dev/null || true)" == true ]]; then
    tap_android_ui_xpath "$always_on_row" || return 1
    wait_for_vpn_preference_state "Always-on VPN" false || return 1
    always_on_configured=false
  fi
}

clear_always_on_mode() {
  if [[ "$always_on_configured" != true ]]; then
    cleanup_always_on_cleared=true
    return 0
  fi
  cleanup_always_on_cleared=false
  [[ -n "$emulator_serial" && ${#adb[@]} -gt 0 ]] || return 0
  local adb_timeout_seconds="$cleanup_adb_timeout_seconds"
  if [[ "$(adb_run get-state 2>/dev/null || true)" != device ]]; then
    return 0
  fi
  if set_android_vpn_mode false false >/dev/null 2>&1; then
    cleanup_always_on_cleared=true
  fi
}

start_fixture_instance() {
  local step_name="$1"
  local label="$2"
  local network="$3"
  local fixture_state="$4"
  local log_file="$5"
  local pid_variable="$6"
  local metadata_variable="$7"
  local metadata="$fixture_state/fixture.json"
  local control_socket packet_socket fixture_process

  mkdir -p "$fixture_state"
  "$fixture_command" run \
    --state-dir "$fixture_state" \
    --network "$network" \
    --path-mode "$path_mode" > "$log_file" 2>&1 &
  fixture_process=$!
  printf -v "$pid_variable" '%s' "$fixture_process"
  record_step "$step_name" started "Waiting for $label"

  for _ in $(seq 1 60); do
    if [[ -s "$metadata" ]]; then
      break
    fi
    if ! kill -0 "$fixture_process" 2>/dev/null; then
      outcome=failed
      outcome_detail="$label exited before readiness"
      record_step "$step_name" failed "$outcome_detail"
      return 1
    fi
    sleep 1
  done

  if [[ ! -s "$metadata" ]] || ! jq -e \
    --arg network "$network" \
    --arg path_mode "$path_mode" '
      .schema_version == 1 and
      .network == $network and
      .path_mode == $path_mode and
      (.bootstrap.peer_id | type == "string" and test("^[A-Za-z0-9]+$") and length <= 256) and
      (.bootstrap.android_address | type == "string" and test("^/[^[:space:]]+$") and length <= 1024) and
      (.bootstrap.kademlia_protocol | type == "string" and test("^/[^[:space:]]+$") and length <= 128) and
      (.peer.peer_id | type == "string" and test("^[A-Za-z0-9]+$") and length <= 256) and
      (if ($path_mode == "automatic" or $path_mode == "owned-quic" or
          $path_mode == "quic-stream" or $path_mode == "tcp-stream") then
        (.peer.pairing_android_address | type == "string" and
          test("^/[^[:space:]]+$") and length <= 1024)
      else
        (.peer.pairing_android_address // null) == null
      end) and
      (.peer.ipv4 | type == "string" and test("^[0-9.]+$") and length <= 15) and
      (.peer.ipv6 | type == "string" and test("^[0-9a-fA-F:]+$") and length <= 45) and
      (.peer.control_socket | type == "string" and length > 0 and length <= 4096) and
      (.packet_control_socket | type == "string" and length > 0 and length <= 4096) and
      (if $path_mode == "owned-quic" then
        (.owned_quic.android_listen | type == "string") and
        (.owned_quic.android_external_endpoint | type == "string") and
        (.owned_quic.host_forward_port | type == "number" and . >= 1 and . <= 65535) and
        (.owned_quic.guest_listen_port | type == "number" and . >= 1 and . <= 65535) and
        .owned_quic.android_listen ==
          ("0.0.0.0:" + (.owned_quic.guest_listen_port | tostring)) and
        .owned_quic.android_external_endpoint ==
          ("127.0.0.1:" + (.owned_quic.host_forward_port | tostring))
      else
        (.owned_quic // null) == null
      end) and
      (if ($path_mode == "relay-only" or $path_mode == "relay-to-direct") then
        (.relay.android_reservation | type == "string" and
          test("^/[^[:space:]]+/p2p-circuit$") and length <= 1024)
      else
        (.relay // null) == null
      end) and
      (if $path_mode == "relay-to-direct" then
        .promotion.direct_path == "tcp_stream"
      else
        (.promotion // null) == null
      end)
    ' "$metadata" >/dev/null 2>&1; then
    outcome=failed
    outcome_detail="$label metadata is unavailable or invalid"
    record_step "$step_name" failed "$outcome_detail"
    return 1
  fi

  control_socket="$(jq -r '.peer.control_socket' "$metadata")"
  packet_socket="$(jq -r '.packet_control_socket' "$metadata")"
  case "$control_socket:$packet_socket" in
    "$fixture_state"/*:"$fixture_state"/*) ;;
    *)
      outcome=failed
      outcome_detail="$label returned sockets outside private state"
      record_step "$step_name" failed "$outcome_detail"
      return 1
      ;;
  esac

  printf -v "$metadata_variable" '%s' "$metadata"
  record_step "$step_name" passed "$label is ready"
}

stop_emulator() {
  if [[ -z "$emulator_pid" ]]; then
    cleanup_emulator_stopped=true
    return 0
  fi
  if kill -0 "$emulator_pid" 2>/dev/null; then
    kill -TERM "$emulator_pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
      if ! kill -0 "$emulator_pid" 2>/dev/null; then
        break
      fi
      sleep 1
    done
  fi
  if kill -0 "$emulator_pid" 2>/dev/null; then
    kill -KILL "$emulator_pid" 2>/dev/null || true
  fi
  wait "$emulator_pid" 2>/dev/null || true
  if kill -0 "$emulator_pid" 2>/dev/null; then
    cleanup_emulator_stopped=false
  else
    cleanup_emulator_stopped=true
  fi
}

stop_fixture_process() {
  local pid="$1"
  if [[ -z "$pid" ]]; then
    return 0
  fi
  if kill -0 "$pid" 2>/dev/null; then
    kill -INT "$pid" 2>/dev/null || true
    for _ in $(seq 1 15); do
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 1
    done
  fi
  if kill -0 "$pid" 2>/dev/null; then
    kill -KILL "$pid" 2>/dev/null || true
  fi
  wait "$pid" 2>/dev/null || true
  ! kill -0 "$pid" 2>/dev/null
}

stop_fixture() {
  cleanup_fixture_stopped=true
  if ! stop_fixture_process "$fixture_pid"; then
    cleanup_fixture_stopped=false
  fi
  if ! stop_fixture_process "$fixture_secondary_pid"; then
    cleanup_fixture_stopped=false
  fi
}

remove_private_state() {
  if [[ -z "$state_dir" ]]; then
    cleanup_private_state_removed=true
    return 0
  fi
  case "$state_dir" in
    "${TMPDIR:-/tmp}"/p2p-vpn-android-e2e-state.*)
      chmod -R u+w "$state_dir" 2>/dev/null || true
      rm -rf -- "$state_dir"
      ;;
    *)
      cleanup_private_state_removed=false
      return 0
      ;;
  esac
  if [[ -e "$state_dir" ]]; then
    cleanup_private_state_removed=false
  else
    cleanup_private_state_removed=true
  fi
}

finalize_evidence() {
  [[ "$evidence_finalized" -eq 0 ]] || return 0
  evidence_finalized=1
  local finished_utc
  finished_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  sanitize_logs
  bound_file "$emulator_log"
  bound_file "$fixture_log"
  bound_file "$fixture_secondary_log"
  bound_file "$android_log"
  jq -n \
    --argjson schema_version "$evidence_schema_version" \
    --arg scenario "$scenario" \
    --arg status "$outcome" \
    --arg detail "$outcome_detail" \
    --arg started_utc "$started_utc" \
    --arg finished_utc "$finished_utc" \
    --arg serial "$emulator_serial" \
    --argjson preflight "$(jq -s . "$checks_file")" \
    --argjson steps "$(jq -s . "$steps_file")" \
    --argjson device "$(cat "$device_file")" \
    --argjson emulator_stopped "$cleanup_emulator_stopped" \
    --argjson fixture_stopped "$cleanup_fixture_stopped" \
    --argjson private_state_removed "$cleanup_private_state_removed" \
    --argjson logs_redacted "$cleanup_logs_redacted" \
    --argjson diagnostic_report_redacted "$cleanup_diagnostic_report_redacted" \
    --argjson always_on_cleared "$cleanup_always_on_cleared" \
    '{
      schema_version: $schema_version,
      kind: "p2p-vpn-android-e2e",
      scenario: $scenario,
      status: $status,
      detail: $detail,
      started_utc: $started_utc,
      finished_utc: $finished_utc,
      preflight: $preflight,
      device: ($device + {serial: $serial}),
      steps: $steps,
      cleanup: {
        emulator_stopped: $emulator_stopped,
        fixture_stopped: $fixture_stopped,
        private_state_removed: $private_state_removed,
        logs_redacted: $logs_redacted,
        diagnostic_report_redacted: $diagnostic_report_redacted,
        always_on_cleared: $always_on_cleared
      },
      artifacts: {
        android_log: "android.log",
        emulator_log: "emulator.log",
        fixture_log: "fixture.log",
        fixture_secondary_log: "fixture-secondary.log"
      }
    }' > "$evidence_file"
  rm -f "$checks_file" "$steps_file" "$device_file"
}

exit_handler() {
  local status="$1"
  local storage_failure=""
  local primary_failure=false
  trap - EXIT INT TERM
  set +e
  if [[ "$status" -eq 0 && -n "$runtime_storage_watchdog_pid" ]] \
    && ! storage_failure="$(check_runtime_storage_budget)"; then
    status=75
    outcome=failed
    outcome_detail="$storage_failure"
  fi
  stop_runtime_storage_watchdog
  if [[ "$outcome" == running ]]; then
    outcome=failed
    outcome_detail="E2E harness terminated unexpectedly"
  fi
  if [[ "$outcome" == failed ]]; then
    primary_failure=true
  fi
  collect_android_diagnostics
  clear_always_on_mode
  stop_emulator
  stop_fixture
  remove_private_state
  sanitize_logs
  if logs_are_redacted; then
    cleanup_logs_redacted=true
  else
    cleanup_logs_redacted=false
    status=1
    outcome=failed
    if [[ "$primary_failure" != true ]]; then
      outcome_detail="E2E diagnostic redaction validation failed"
      primary_failure=true
    fi
  fi
  if [[ "$diagnostic_report_required" == true \
    && "$cleanup_diagnostic_report_redacted" != true ]]; then
    status=1
    outcome=failed
    if [[ "$primary_failure" != true ]]; then
      outcome_detail="Android diagnostic report validation failed"
      primary_failure=true
    fi
  fi
  if [[ "$cleanup_always_on_cleared" != true ]]; then
    status=1
    outcome=failed
    if [[ "$primary_failure" != true ]]; then
      outcome_detail="Android always-on settings cleanup failed"
      primary_failure=true
    fi
  fi
  finalize_evidence
  printf 'Android E2E evidence: %s\n' "$evidence_file" >&2
  exit "$status"
}

termination_handler() {
  if [[ -n "$runtime_storage_failure_file" && -s "$runtime_storage_failure_file" ]]; then
    outcome=failed
    IFS= read -r outcome_detail < "$runtime_storage_failure_file"
    exit 75
  fi
  outcome=failed
  outcome_detail="E2E harness terminated"
  exit 143
}

trap 'exit_handler $?' EXIT
trap 'outcome=failed; outcome_detail="E2E harness interrupted"; exit 130' INT
trap termination_handler TERM

missing_requirements=()
test_mode="${P2P_VPN_ANDROID_E2E_TEST_MODE:-0}"
emulator_command="${P2P_VPN_ANDROID_EMULATOR:-}"
adb_command="${P2P_VPN_ADB:-adb}"
df_command="${P2P_VPN_ANDROID_E2E_DF:-df}"
android_apk="${P2P_VPN_ANDROID_APK:-}"
fixture_command="${P2P_VPN_ANDROID_E2E_FIXTURE:-}"
p2p_vpn_command="${P2P_VPN_BIN:-}"

if [[ "$(uname -s)" == Linux ]]; then
  record_check host_linux true true "Linux host"
else
  record_check host_linux true false "Android emulator E2E requires Linux"
  missing_requirements+=(host_linux)
fi

if [[ "$(uname -m)" == x86_64 ]]; then
  record_check host_architecture true true "x86_64 host"
else
  record_check host_architecture true false "API 35 emulator target requires x86_64"
  missing_requirements+=(host_architecture)
fi

tmp_available_bytes="$({ available_bytes_for_path "${TMPDIR:-/tmp}"; } || true)"
output_available_bytes="$({ available_bytes_for_path "$output_dir"; } || true)"
if [[ "$tmp_available_bytes" =~ ^[0-9]{1,18}$ \
  && "$output_available_bytes" =~ ^[0-9]{1,18}$ ]]; then
  tmp_available_bytes=$((10#$tmp_available_bytes))
  output_available_bytes=$((10#$output_available_bytes))
  if ((tmp_available_bytes >= minimum_free_bytes \
    && output_available_bytes >= minimum_free_bytes)); then
    record_check disk_space true true \
      "tmp has $tmp_available_bytes bytes; evidence has $output_available_bytes; $minimum_free_bytes required"
  else
    record_check disk_space true false \
      "tmp has $tmp_available_bytes bytes; evidence has $output_available_bytes; $minimum_free_bytes required"
    missing_requirements+=(disk_space)
  fi
else
  record_check disk_space true false "Available bytes could not be determined"
  missing_requirements+=(disk_space)
fi

if [[ -n "$emulator_command" && -x "$emulator_command" ]]; then
  record_check emulator_command true true "Nix emulator launcher is executable"
else
  record_check emulator_command true false "P2P_VPN_ANDROID_EMULATOR is unavailable"
  missing_requirements+=(emulator_command)
fi

if [[ -x "$adb_command" ]] || command -v "$adb_command" >/dev/null 2>&1; then
  record_check adb true true "ADB is executable"
else
  record_check adb true false "ADB is unavailable"
  missing_requirements+=(adb)
fi

if command -v timeout >/dev/null 2>&1; then
  record_check timeout true true "Bounded subprocess runner is executable"
else
  record_check timeout true false "GNU timeout is unavailable"
  missing_requirements+=(timeout)
fi

if [[ "$scenario" != boot-smoke ]]; then
  if [[ -n "$android_apk" && -f "$android_apk" ]]; then
    record_check android_apk true true "Reproducible debug APK is available"
  else
    record_check android_apk true false "P2P_VPN_ANDROID_APK is unavailable"
    missing_requirements+=(android_apk)
  fi
else
  record_check android_apk false false "boot-smoke does not reinstall the APK"
fi

if [[ "$pairing_scenario" -eq 1 ]]; then
  if [[ -n "$fixture_command" && -x "$fixture_command" ]]; then
    record_check fixture_command true true "Rootless Linux fixture is executable"
  else
    record_check fixture_command true false "P2P_VPN_ANDROID_E2E_FIXTURE is unavailable"
    missing_requirements+=(fixture_command)
  fi
  if [[ -n "$p2p_vpn_command" && -x "$p2p_vpn_command" ]]; then
    record_check p2p_vpn_command true true "p2p-vpn pairing CLI is executable"
  else
    record_check p2p_vpn_command true false "P2P_VPN_BIN is unavailable"
    missing_requirements+=(p2p_vpn_command)
  fi
else
  record_check fixture_command false false "Selected scenario does not use the Linux fixture"
  record_check p2p_vpn_command false false "Selected scenario does not use the pairing CLI"
fi

if [[ "$test_mode" == 1 ]]; then
  record_check kvm true true "test-mode capability stub"
else
  if [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]]; then
    record_check kvm true true "/dev/kvm is readable and writable"
  else
    record_check kvm true false "/dev/kvm is unavailable to the current user"
    missing_requirements+=(kvm)
  fi
fi

if [[ -c /dev/net/tun ]]; then
  record_check tun false true "/dev/net/tun is present for later packet scenarios"
else
  record_check tun false false "/dev/net/tun is absent; boot-smoke does not require it"
fi

if (( ${#missing_requirements[@]} > 0 )); then
  outcome=skipped
  outcome_detail="Missing requirements: ${missing_requirements[*]}"
  record_step preflight skipped "$outcome_detail"
  if [[ "$preflight_only" -eq 1 || "$allow_skip" -eq 1 ]]; then
    exit 77
  fi
  outcome=failed
  exit 2
fi

record_step preflight passed "Required Android E2E capabilities are available"

if [[ "$preflight_only" -eq 1 ]]; then
  outcome=passed
  outcome_detail="Preflight passed"
  exit 0
fi

state_dir="$(mktemp -d -t p2p-vpn-android-e2e-state.XXXXXXXX)"
ready_file="$state_dir/emulator.ready"
runtime_storage_failure_file="$state_dir/runtime-storage-failure"
runtime_tmp_baseline_available_bytes="$tmp_available_bytes"
runtime_output_baseline_available_bytes="$output_available_bytes"
start_runtime_storage_watchdog

fixture_metadata=""
fixture_network="android-e2e"
fixture_bootstrap_peer=""
fixture_bootstrap_address=""
fixture_kademlia_protocol=""
fixture_peer_id=""
fixture_pairing_android_address=""
fixture_ipv4=""
fixture_ipv6=""
fixture_control_socket=""
fixture_packet_socket=""
fixture_owned_quic_listen=""
fixture_owned_quic_external_endpoint=""
fixture_owned_quic_host_port=""
fixture_owned_quic_guest_port=""
fixture_relay_reservation=""
fixture_promotion_path=""
fixture_secondary_metadata=""
fixture_secondary_bootstrap_peer=""
fixture_secondary_bootstrap_address=""
fixture_secondary_kademlia_protocol=""
fixture_secondary_ipv4=""
fixture_secondary_ipv6=""
fixture_secondary_control_socket=""
fixture_secondary_packet_socket=""

if [[ "$scenario" == multi-network ]]; then
  fixture_network="android-e2e-alpha"
fi

if [[ "$pairing_scenario" -eq 1 ]]; then
  fixture_state_dir="$state_dir/fixture"
  mkdir -p "$fixture_state_dir"
  fixture_metadata="$fixture_state_dir/fixture.json"
  "$fixture_command" run \
    --state-dir "$fixture_state_dir" \
    --network "$fixture_network" \
    --path-mode "$path_mode" > "$fixture_log" 2>&1 &
  fixture_pid=$!
  record_step fixture_start started "Waiting for private discovery and rootless Linux peer"
  for _ in $(seq 1 60); do
    if [[ -s "$fixture_metadata" ]]; then
      break
    fi
    if ! kill -0 "$fixture_pid" 2>/dev/null; then
      outcome=failed
      outcome_detail="Linux E2E fixture exited before readiness"
      record_step fixture_start failed "$outcome_detail"
      exit 1
    fi
    sleep 1
  done
  if [[ ! -s "$fixture_metadata" ]] || ! jq -e \
    --arg network "$fixture_network" \
    --arg path_mode "$path_mode" '
    .schema_version == 1 and
    .network == $network and
    .path_mode == $path_mode and
    (.bootstrap.peer_id | type == "string" and test("^[A-Za-z0-9]+$") and length <= 256) and
    (.bootstrap.android_address | type == "string" and test("^/[^[:space:]]+$") and length <= 1024) and
    (.bootstrap.kademlia_protocol | type == "string" and test("^/[^[:space:]]+$") and length <= 128) and
    (.peer.peer_id | type == "string" and test("^[A-Za-z0-9]+$") and length <= 256) and
    (if ($path_mode == "automatic" or $path_mode == "owned-quic" or
        $path_mode == "quic-stream" or $path_mode == "tcp-stream") then
      (.peer.pairing_android_address | type == "string" and
        test("^/[^[:space:]]+$") and length <= 1024)
    else
      (.peer.pairing_android_address // null) == null
    end) and
    (.peer.ipv4 | type == "string" and test("^[0-9.]+$") and length <= 15) and
    (.peer.ipv6 | type == "string" and test("^[0-9a-fA-F:]+$") and length <= 45) and
    (.peer.control_socket | type == "string" and length > 0 and length <= 4096) and
    (.packet_control_socket | type == "string" and length > 0 and length <= 4096) and
    (if $path_mode == "owned-quic" then
      (.owned_quic.android_listen | type == "string") and
      (.owned_quic.android_external_endpoint | type == "string") and
      (.owned_quic.host_forward_port | type == "number" and . >= 1 and . <= 65535) and
      (.owned_quic.guest_listen_port | type == "number" and . >= 1 and . <= 65535) and
      .owned_quic.android_listen ==
        ("0.0.0.0:" + (.owned_quic.guest_listen_port | tostring)) and
      .owned_quic.android_external_endpoint ==
        ("127.0.0.1:" + (.owned_quic.host_forward_port | tostring))
    else
      (.owned_quic // null) == null
    end) and
    (if ($path_mode == "relay-only" or $path_mode == "relay-to-direct") then
      (.relay.android_reservation | type == "string" and
        test("^/[^[:space:]]+/p2p-circuit$") and length <= 1024)
    else
      (.relay // null) == null
    end) and
    (if $path_mode == "relay-to-direct" then
      .promotion.direct_path == "tcp_stream"
    else
      (.promotion // null) == null
    end)
  ' "$fixture_metadata" >/dev/null 2>&1; then
    outcome=failed
    outcome_detail="Linux E2E fixture metadata is unavailable or invalid"
    record_step fixture_start failed "$outcome_detail"
    exit 1
  fi
  fixture_bootstrap_peer="$(jq -r '.bootstrap.peer_id' "$fixture_metadata")"
  fixture_bootstrap_address="$(jq -r '.bootstrap.android_address' "$fixture_metadata")"
  fixture_kademlia_protocol="$(jq -r '.bootstrap.kademlia_protocol' "$fixture_metadata")"
  fixture_peer_id="$(jq -r '.peer.peer_id' "$fixture_metadata")"
  fixture_pairing_android_address="$(
    jq -r '.peer.pairing_android_address // empty' "$fixture_metadata"
  )"
  fixture_ipv4="$(jq -r '.peer.ipv4' "$fixture_metadata")"
  fixture_ipv6="$(jq -r '.peer.ipv6' "$fixture_metadata")"
  fixture_control_socket="$(jq -r '.peer.control_socket' "$fixture_metadata")"
  fixture_packet_socket="$(jq -r '.packet_control_socket' "$fixture_metadata")"
  if [[ "$path_mode" == owned-quic ]]; then
    fixture_owned_quic_listen="$(jq -r '.owned_quic.android_listen' "$fixture_metadata")"
    fixture_owned_quic_external_endpoint="$(jq -r '.owned_quic.android_external_endpoint' "$fixture_metadata")"
    fixture_owned_quic_host_port="$(jq -r '.owned_quic.host_forward_port' "$fixture_metadata")"
    fixture_owned_quic_guest_port="$(jq -r '.owned_quic.guest_listen_port' "$fixture_metadata")"
  fi
  if [[ "$path_mode" == relay-only || "$path_mode" == relay-to-direct ]]; then
    fixture_relay_reservation="$(jq -r '.relay.android_reservation' "$fixture_metadata")"
  fi
  if [[ "$path_mode" == relay-to-direct ]]; then
    fixture_promotion_path="$(jq -r '.promotion.direct_path' "$fixture_metadata")"
  fi
  case "$fixture_control_socket:$fixture_packet_socket" in
    "$fixture_state_dir"/*:"$fixture_state_dir"/*) ;;
    *)
      outcome=failed
      outcome_detail="Linux E2E fixture returned sockets outside private state"
      record_step fixture_start failed "$outcome_detail"
      exit 1
      ;;
  esac
  record_step fixture_start passed "Private discovery and rootless Linux peer are ready"
fi

if [[ "$scenario" == multi-network ]]; then
  fixture_secondary_state_dir="$state_dir/fixture-secondary"
  if ! start_fixture_instance \
    secondary_fixture_start \
    "secondary private discovery and rootless Linux peer" \
    android-e2e-beta \
    "$fixture_secondary_state_dir" \
    "$fixture_secondary_log" \
    fixture_secondary_pid \
    fixture_secondary_metadata; then
    exit 1
  fi
  fixture_secondary_bootstrap_peer="$(
    jq -r '.bootstrap.peer_id' "$fixture_secondary_metadata"
  )"
  fixture_secondary_bootstrap_address="$(
    jq -r '.bootstrap.android_address' "$fixture_secondary_metadata"
  )"
  fixture_secondary_kademlia_protocol="$(
    jq -r '.bootstrap.kademlia_protocol' "$fixture_secondary_metadata"
  )"
  fixture_secondary_ipv4="$(jq -r '.peer.ipv4' "$fixture_secondary_metadata")"
  fixture_secondary_ipv6="$(jq -r '.peer.ipv6' "$fixture_secondary_metadata")"
  fixture_secondary_control_socket="$(
    jq -r '.peer.control_socket' "$fixture_secondary_metadata"
  )"
  fixture_secondary_packet_socket="$(
    jq -r '.packet_control_socket' "$fixture_secondary_metadata"
  )"
fi

P2P_VPN_ANDROID_EMULATOR_READY_FILE="$ready_file" \
  "$emulator_command" > "$emulator_log" 2>&1 &
emulator_pid=$!
record_step emulator_start started "Waiting for a clean API 35 emulator"

for _ in $(seq 1 240); do
  if [[ -s "$ready_file" ]]; then
    break
  fi
  if ! kill -0 "$emulator_pid" 2>/dev/null; then
    outcome=failed
    outcome_detail="Emulator launcher exited before readiness"
    record_step emulator_start failed "$outcome_detail"
    exit 1
  fi
  sleep 1
done

if [[ ! -s "$ready_file" ]]; then
  outcome=failed
  outcome_detail="Emulator did not become ready within 240 seconds"
  record_step emulator_start failed "$outcome_detail"
  exit 1
fi

IFS= read -r emulator_serial < "$ready_file"
if [[ ! "$emulator_serial" =~ ^[A-Za-z0-9._:-]+$ || ${#emulator_serial} -gt 128 ]]; then
  outcome=failed
  outcome_detail="Emulator launcher returned an invalid ADB serial"
  record_step emulator_start failed "$outcome_detail"
  exit 1
fi
record_step emulator_start passed "Emulator reported an ADB serial"

adb=("$adb_command" -s "$emulator_serial")
if [[ "$(adb_run get-state)" != device ]]; then
  outcome=failed
  outcome_detail="ADB did not report the emulator as a device"
  record_step adb failed "$outcome_detail"
  exit 1
fi

if [[ "$path_mode" == owned-quic ]]; then
  if ! adb_run emu redir add \
    "udp:$fixture_owned_quic_host_port:$fixture_owned_quic_guest_port" >/dev/null; then
    outcome=failed
    outcome_detail="Emulator could not install the owned-QUIC UDP redirection"
    record_step owned_quic_redirection failed "$outcome_detail"
    exit 1
  fi
  record_step owned_quic_redirection passed \
    "Linux and Android owned-QUIC listeners are mutually reachable"
fi

api_level="$(adb_run shell getprop ro.build.version.sdk | tr -d '\r')"
device_abi="$(adb_run shell getprop ro.product.cpu.abi | tr -d '\r')"
package_path="$(adb_run shell pm path org.hermeticfoundation.p2pvpn.debug | tr -d '\r')"
activity_state="$(adb_run shell dumpsys activity activities)"

jq -n \
  --arg api_level "$api_level" \
  --arg abi "$device_abi" \
  --arg package_path "$package_path" \
  '{api_level: $api_level, abi: $abi, package_installed: ($package_path | startswith("package:"))}' \
  > "$device_file"

if [[ "$api_level" != 35 || "$device_abi" != x86_64 ]]; then
  outcome=failed
  outcome_detail="Emulator API or ABI does not match the reproducible target"
  record_step device_contract failed "$outcome_detail"
  exit 1
fi
if [[ "$package_path" != package:* ]]; then
  outcome=failed
  outcome_detail="p2p-vpn debug package is not installed"
  record_step package failed "$outcome_detail"
  exit 1
fi
if ! grep -Fq org.hermeticfoundation.p2pvpn.MainActivity <<< "$activity_state"; then
  outcome=failed
  outcome_detail="p2p-vpn main activity is not running"
  record_step activity failed "$outcome_detail"
  exit 1
fi

automation_status_file="$state_dir/automation-status.json"
if ! wait_for_automation_status \
  "$automation_status_file" \
  '(.value.snapshot.has_profile | not)'; then
  outcome=failed
  outcome_detail="Protected debug automation status did not become ready"
  record_step debug_automation failed "$outcome_detail"
  exit 1
fi
if ! jq -e '
  .schema_version == 1 and
  .ok and
  .value.service_ready and
  (.value.snapshot.has_profile | not) and
  (.value.snapshot.profile_stored | not) and
  (.value.snapshot.addresses | length == 0) and
  .value.snapshot.paths.connected_peers == 0
' "$automation_status_file" >/dev/null 2>&1; then
  outcome=failed
  outcome_detail="Protected debug automation status is unavailable or invalid"
  record_step debug_automation failed "$outcome_detail"
  exit 1
fi

jq '. + {debug_automation: true}' "$device_file" > "$device_file.updated"
mv -f "$device_file.updated" "$device_file"

record_step device_contract passed "API 35 x86_64 device contract verified"
record_step package passed "Debug package is installed"
record_step activity passed "Main activity is running"
record_step debug_automation passed "ADB-authorized structured status is available"

if [[ "$scenario" == boot-smoke ]]; then
  outcome=passed
  outcome_detail="Clean emulator boot and application smoke test passed"
  exit 0
fi

if [[ "$scenario" == network-workflow ]]; then
  if run_network_workflow_scenario; then
    exit 0
  fi
  exit 1
fi

if [[ "$scenario" == multi-network ]]; then
  if run_multi_network_scenario; then
    exit 0
  fi
  exit 1
fi

if [[ "$pairing_scenario" -eq 1 ]]; then
  pairing_profile="$state_dir/pairing-profile.json"
  command_response="$state_dir/automation-command.json"
  profile_arguments=(
    --es network android-e2e
    --es bootstrap_peer_id "$fixture_bootstrap_peer"
    --es bootstrap_address "$fixture_bootstrap_address"
    --es kademlia_protocol "$fixture_kademlia_protocol"
  )
  if [[ "$path_mode" == owned-quic ]]; then
    profile_arguments+=(
      --es packet_quic_listen "$fixture_owned_quic_listen"
      --es packet_quic_external_endpoint "$fixture_owned_quic_external_endpoint"
    )
  fi
  if [[ "$path_mode" == relay-only || "$path_mode" == relay-to-direct ]]; then
    profile_arguments+=(--es relay_reservation "$fixture_relay_reservation")
  fi
  if ! android_automation create-profile "${profile_arguments[@]}" \
    > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "create-profile"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept isolated profile creation"
    record_step profile_creation failed "$outcome_detail"
    exit 1
  fi
  if ! wait_for_automation_status \
    "$pairing_profile" \
    '.value.snapshot.has_profile and .value.snapshot.profile_stored and (.value.snapshot.busy | not)'; then
    outcome=failed
    outcome_detail="Isolated encrypted profile creation did not complete"
    record_step profile_creation failed "$outcome_detail"
    exit 1
  fi
  if ! jq -e '
    (.value.snapshot.peer_id | type == "string" and length > 0 and length <= 256) and
    (.value.snapshot.addresses | any(contains("."))) and
    (.value.snapshot.addresses | any(contains(":")))
  ' "$pairing_profile" >/dev/null; then
    outcome=failed
    outcome_detail="Isolated profile does not expose valid dual-stack identity metadata"
    record_step profile_creation failed "$outcome_detail"
    exit 1
  fi
  android_peer_id="$(jq -r '.value.snapshot.peer_id' "$pairing_profile")"
  android_hostname="$(jq -r '.value.snapshot.hostname' "$pairing_profile")"
  if [[ ! "$android_hostname" =~ ^android-[0-9a-f]{16}$ ]]; then
    outcome=failed
    outcome_detail="Android profile did not generate a stable p2p-vpn hostname"
    record_step profile_creation failed "$outcome_detail"
    exit 1
  fi
  record_step profile_creation passed \
    "Encrypted profile configured with discovery bootstrap only"

  if ! adb_run shell appops set \
    org.hermeticfoundation.p2pvpn.debug ACTIVATE_VPN allow >/dev/null; then
    outcome=failed
    outcome_detail="ADB could not grant emulator VPN consent"
    record_step vpn_consent failed "$outcome_detail"
    exit 1
  fi
  if ! android_automation connect > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "connect"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not start the VPN"
    record_step vpn_connect failed "$outcome_detail"
    exit 1
  fi
  connected_status="$state_dir/status-connected.json"
  if ! wait_for_automation_status \
    "$connected_status" \
    '.value.snapshot.connected and (.value.snapshot.busy | not)' \
    90; then
    outcome=failed
    outcome_detail="Android VPN runtime did not connect"
    record_step vpn_connect failed "$outcome_detail"
    exit 1
  fi
  record_step vpn_consent passed "ADB authorized the normal VpnService consent gate"
  record_step vpn_connect passed "Android VPN runtime connected"

  pair_open="$state_dir/pair-open.json"
  if ! "$p2p_vpn_command" pair open \
    --socket "$fixture_control_socket" \
    --expires-in-seconds 300 \
    --format json > "$pair_open" \
    || ! jq -e '
      (.operation_id | type == "string" and length > 0 and length <= 128) and
      (.code | type == "string" and length > 0 and length <= 64)
    ' "$pair_open" >/dev/null; then
    outcome=failed
    outcome_detail="Linux fixture could not open a bounded pairing operation"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi
  pair_operation="$(jq -r '.operation_id' "$pair_open")"
  pair_code="$(jq -r '.code' "$pair_open")"

  if ! android_automation join-pairing --es code "$pair_code" > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "join-pairing"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Android did not accept the pairing code"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi

  inviter_status="$state_dir/inviter-status.json"
  if ! wait_for_pair_status \
    "$inviter_status" \
    "$pair_operation" \
    '.phase == "awaiting_approval" and (.candidate.approval_id | type == "string" and length > 0 and length <= 128)' \
    180; then
    outcome=failed
    outcome_detail="Code pairing did not discover the Android candidate"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi
  if [[ "$(jq -r '.candidate.peer_id' "$inviter_status")" != "$android_peer_id" ]]; then
    outcome=failed
    outcome_detail="Pairing candidate identity did not match the Android profile"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi
  if [[ "$(jq -r '.candidate.requested_hostname // empty' "$inviter_status")" != "$android_hostname" ]]; then
    outcome=failed
    outcome_detail="Android pairing request did not authenticate its generated hostname"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi
  approval_id="$(jq -r '.candidate.approval_id' "$inviter_status")"
  pair_approved="$state_dir/pair-approved.json"
  if ! "$p2p_vpn_command" pair approve \
    "$pair_operation" \
    "$approval_id" \
    --socket "$fixture_control_socket" \
    --format json > "$pair_approved" \
    || ! jq -e '.phase == "completed"' "$pair_approved" >/dev/null; then
    outcome=failed
    outcome_detail="Linux fixture could not approve the Android candidate"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi

  paired_status="$state_dir/status-paired.json"
  if ! wait_for_automation_status \
    "$paired_status" \
    '.value.snapshot.connected and (.value.snapshot.busy | not) and (.value.snapshot.pairing_detail | startswith("Paired with ")) and .value.snapshot.paths.connected_peers >= 1' \
    180; then
    outcome=failed
    outcome_detail="Android did not apply pairing artifacts and reconnect automatically"
    record_step code_pairing failed "$outcome_detail"
    exit 1
  fi
  record_step code_pairing passed \
    "Android enrolled from a code without a configured overlay peer address" \
    '{"hostname_assigned":true}'

  android_ipv4="$(
    jq -r '[.value.snapshot.addresses[] | select(contains("."))][0] | split("/")[0]' \
      "$paired_status"
  )"
  android_ipv6="$(
    jq -r '[.value.snapshot.addresses[] | select(contains(":"))][0] | split("/")[0]' \
      "$paired_status"
  )"
  if [[ ! "$android_ipv4" =~ ^[0-9.]{7,15}$ \
    || ! "$android_ipv6" =~ ^[0-9a-fA-F:]{2,45}$ ]]; then
    outcome=failed
    outcome_detail="Paired Android overlay addresses are invalid"
    record_step overlay_addresses failed "$outcome_detail"
    exit 1
  fi
  record_step overlay_addresses passed "Paired profile exposes IPv4 and IPv6 overlay addresses"

  owned_quic_packets_before=0
  if [[ "$path_mode" == owned-quic ]]; then
    owned_quic_ready="$state_dir/status-owned-quic-ready.json"
    if ! wait_for_automation_status \
      "$owned_quic_ready" \
      '(.value.snapshot.paths.direct_quic_datagram >= 1) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.relay == 0) and (.value.snapshot.paths.packet_plane_quic_sessions >= 1)' \
      90; then
      outcome=failed
      outcome_detail="Android owned-QUIC packet plane did not become ready"
      record_step owned_quic_ready failed "$outcome_detail"
      exit 1
    fi
    owned_quic_packets_before="$(
      jq -r '.value.snapshot.paths.outbound_quic_datagram_packets' "$owned_quic_ready"
    )"
    if [[ ! "$owned_quic_packets_before" =~ ^[0-9]{1,12}$ ]]; then
      outcome=failed
      outcome_detail="Android owned-QUIC packet counter is invalid"
      record_step owned_quic_ready failed "$outcome_detail"
      exit 1
    fi
    record_step owned_quic_ready passed \
      "Android established the owned-QUIC packet session before measurement"
  fi

  fixture_ipv4_ready_attempts=0
  fixture_ipv6_ready_attempts=0
  android_ipv4_ready_attempts=0
  android_ipv6_ready_attempts=0
  if ! wait_for_fixture_probe_ready \
    "$fixture_ipv4" \
    "$android_ipv4" \
    ipv4 \
    "$state_dir/linux-ipv4-readiness.json" \
    fixture_ipv4_ready_attempts \
    || ! wait_for_fixture_probe_ready \
      "$fixture_ipv6" \
      "$android_ipv6" \
      ipv6 \
      "$state_dir/linux-ipv6-readiness.json" \
      fixture_ipv6_ready_attempts \
    || ! wait_for_android_ping_ready \
      ipv4 \
      "$fixture_ipv4" \
      "$state_dir/android-ipv4-readiness.txt" \
      android_ipv4_ready_attempts \
    || ! wait_for_android_ping_ready \
      ipv6 \
      "$fixture_ipv6" \
      "$state_dir/android-ipv6-readiness.txt" \
      android_ipv6_ready_attempts; then
    outcome=failed
    outcome_detail="Bidirectional dual-stack packet forwarding did not become ready"
    record_step traffic_readiness failed "$outcome_detail"
    exit 1
  fi
  record_step traffic_readiness passed \
    "Bidirectional IPv4 and IPv6 forwarding converged before measurement"

  promotion_runtime_generation_before=0
  promotion_runtime_generation_after=0
  promotion_relay_packets_before=0
  promotion_relay_packets_after=0
  promotion_relay_packet_delta=0
  promotion_linux_direct_packets_before=0
  promotion_linux_direct_packets_after=0
  promotion_linux_direct_packet_delta=0
  promotion_android_direct_packets_before=0
  promotion_android_direct_packets_after=0
  promotion_android_direct_packet_delta=0
  promotion_convergence_millis=0
  promotion_fixture_process_continuous=false
  if [[ "$path_mode" == relay-to-direct ]]; then
    promotion_android_relay_status="$state_dir/status-promotion-relay.json"
    if ! wait_for_automation_status \
      "$promotion_android_relay_status" \
      '(.value.snapshot.paths.relay >= 1) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.direct_quic_datagram == 0) and (.value.snapshot.paths.direct_quic_stream == 0) and (.value.snapshot.paths.direct_tcp_stream == 0) and (.value.snapshot.paths.promotions_to_direct == 0) and (.value.snapshot.runtime_generation >= 1)' \
      60; then
      outcome=failed
      outcome_detail="Android did not establish the initial isolated relay path"
      record_step promotion_relay_baseline failed "$outcome_detail"
      exit 1
    fi
    promotion_runtime_generation_before="$(
      jq -r '.value.snapshot.runtime_generation' "$promotion_android_relay_status"
    )"

    promotion_linux_relay_before="$state_dir/fixture-promotion-relay-before.txt"
    if ! "$p2p_vpn_command" daemon-state \
      --socket "$fixture_control_socket" > "$promotion_linux_relay_before"; then
      outcome=failed
      outcome_detail="Linux fixture promotion baseline could not be queried"
      record_step promotion_relay_baseline failed "$outcome_detail"
      exit 1
    fi
    promotion_selected_relay_paths="$(
      awk -v peer="$android_peer_id" '
        /^peer state:/ && index($0, peer) &&
          /selected_path circuit_relay/ && /direct_paths 0/ &&
          /relay_paths [1-9][0-9]*/ { count += 1 }
        END { print count + 0 }
      ' "$promotion_linux_relay_before"
    )"
    promotion_relay_packets_before="$(
      awk '$1 == "outbound_stream_fallback_packets" { value = $2 }
        END { print value + 0 }' "$promotion_linux_relay_before"
    )"
    promotion_linux_direct_packets_before="$(
      awk '$1 == "outbound_direct_tcp_stream_fallback_packets" { value = $2 }
        END { print value + 0 }' "$promotion_linux_relay_before"
    )"
    promotion_linux_events_before="$(
      awk '$1 == "path_promotions_to_direct" { value = $2 }
        END { print value + 0 }' "$promotion_linux_relay_before"
    )"
    if [[ "$promotion_selected_relay_paths" -lt 1 \
      || "$promotion_linux_direct_packets_before" -ne 0 \
      || "$promotion_linux_events_before" -ne 0 ]]; then
      outcome=failed
      outcome_detail="Linux fixture did not begin on an isolated relay path"
      record_step promotion_relay_baseline failed "$outcome_detail"
      exit 1
    fi
    record_step promotion_relay_baseline passed \
      "Both runtimes selected relay before the direct endpoint was enabled"
  fi

  if ! measure_bidirectional_traffic "" ""; then
    exit 1
  fi

  underlay_initial_kind=""
  underlay_fallback_kind=""
  underlay_recovered_kind=""
  underlay_restored_kind=""
  underlay_selection_changes_before=0
  underlay_selection_changes_after=0
  underlay_selected_losses_before=0
  underlay_selected_losses_after=0
  underlay_recoveries_before=0
  underlay_recoveries_after=0
  underlay_runtime_recovery_requests_before=0
  underlay_runtime_recovery_requests_after=0
  underlay_runtime_recovery_failures_before=0
  underlay_runtime_recovery_failures_after=0
  underlay_runtime_generation_before=0
  underlay_runtime_generation_after=0
  underlay_fallback_convergence_millis=0
  underlay_loss_detection_millis=0
  underlay_recovery_convergence_millis=0
  underlay_restore_convergence_millis=0
  underlay_outage_hold_millis=0
  underlay_android_process_continuous=false
  underlay_fixture_process_continuous=false
  if [[ "$scenario" == underlay-recovery ]]; then
    underlay_baseline="$state_dir/status-underlay-baseline.json"
    if ! wait_for_automation_status \
      "$underlay_baseline" \
      '.value.snapshot.connected and (.value.snapshot.busy | not) and (.value.snapshot.paths.connected_peers >= 1) and (.value.snapshot.runtime_generation >= 1) and (.value.snapshot.underlay.kind == "wifi") and .value.snapshot.underlay.validated' \
      60; then
      outcome=failed
      outcome_detail="Android did not report a validated Wi-Fi underlay baseline"
      record_step underlay_baseline failed "$outcome_detail"
      exit 1
    fi
    underlay_initial_kind="$(jq -r '.value.snapshot.underlay.kind' "$underlay_baseline")"
    underlay_selection_changes_before="$(
      jq -r '.value.snapshot.underlay.selection_changes' "$underlay_baseline"
    )"
    underlay_selected_losses_before="$(
      jq -r '.value.snapshot.underlay.selected_losses' "$underlay_baseline"
    )"
    underlay_recoveries_before="$(
      jq -r '.value.snapshot.underlay.recoveries' "$underlay_baseline"
    )"
    underlay_runtime_recovery_requests_before="$(
      jq -r '.value.snapshot.underlay.runtime_recovery_requests' "$underlay_baseline"
    )"
    underlay_runtime_recovery_failures_before="$(
      jq -r '.value.snapshot.underlay.runtime_recovery_failures' "$underlay_baseline"
    )"
    underlay_runtime_generation_before="$(
      jq -r '.value.snapshot.runtime_generation' "$underlay_baseline"
    )"
    android_process_before="$(
      adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r'
    )"
    if [[ ! "$android_process_before" =~ ^[0-9]+$ ]]; then
      outcome=failed
      outcome_detail="Android application process identity is unavailable"
      record_step underlay_baseline failed "$outcome_detail"
      exit 1
    fi
    record_step underlay_baseline passed \
      "Validated Wi-Fi carried the measured baseline without configured peer addresses"

    underlay_transition_started_millis="$(monotonic_millis)"
    if ! adb_run shell svc wifi disable >/dev/null; then
      outcome=failed
      outcome_detail="Emulator Wi-Fi underlay could not be disabled"
      record_step underlay_cellular_fallback failed "$outcome_detail"
      exit 1
    fi
    underlay_cellular="$state_dir/status-underlay-cellular.json"
    if ! wait_for_automation_status \
      "$underlay_cellular" \
      ".value.snapshot.connected and (.value.snapshot.paths.connected_peers >= 1) and (.value.snapshot.runtime_generation == $underlay_runtime_generation_before) and (.value.snapshot.underlay.kind == \"cellular\") and .value.snapshot.underlay.validated and (.value.snapshot.underlay.selection_changes >= ($underlay_selection_changes_before + 1)) and (.value.snapshot.underlay.selected_losses >= ($underlay_selected_losses_before + 1)) and (.value.snapshot.underlay.runtime_recovery_requests >= ($underlay_runtime_recovery_requests_before + 1)) and (.value.snapshot.underlay.runtime_recovery_failures == $underlay_runtime_recovery_failures_before)" \
      120; then
      outcome=failed
      outcome_detail="Android did not recover over cellular after Wi-Fi loss"
      record_step underlay_cellular_fallback failed "$outcome_detail"
      exit 1
    fi
    underlay_fallback_kind="$(jq -r '.value.snapshot.underlay.kind' "$underlay_cellular")"
    if ! wait_for_transition_traffic_ready \
      cellular_fallback \
      "after cellular fallback"; then
      exit 1
    fi
    underlay_fallback_completed_millis="$(monotonic_millis)"
    underlay_fallback_convergence_millis=$((
      underlay_fallback_completed_millis - underlay_transition_started_millis
    ))
    if ! measure_bidirectional_traffic \
      cellular_fallback \
      "after cellular fallback"; then
      exit 1
    fi
    record_step underlay_cellular_fallback passed \
      "The existing runtime recovered through the cellular fallback"

    underlay_loss_started_millis="$(monotonic_millis)"
    if ! adb_run shell svc data disable >/dev/null; then
      outcome=failed
      outcome_detail="Emulator cellular underlay could not be disabled"
      record_step underlay_total_loss failed "$outcome_detail"
      exit 1
    fi
    underlay_lost="$state_dir/status-underlay-lost.json"
    if ! wait_for_automation_status \
      "$underlay_lost" \
      ".value.snapshot.connected and (.value.snapshot.runtime_generation == $underlay_runtime_generation_before) and (.value.snapshot.underlay.kind == \"none\") and (.value.snapshot.underlay.available_networks == 0) and (.value.snapshot.underlay.selection_changes >= ($underlay_selection_changes_before + 2)) and (.value.snapshot.underlay.selected_losses >= ($underlay_selected_losses_before + 2)) and (.value.snapshot.underlay.runtime_recovery_requests >= ($underlay_runtime_recovery_requests_before + 2)) and (.value.snapshot.underlay.runtime_recovery_failures == $underlay_runtime_recovery_failures_before)" \
      60; then
      outcome=failed
      outcome_detail="Android did not observe complete physical underlay loss"
      record_step underlay_total_loss failed "$outcome_detail"
      exit 1
    fi
    underlay_loss_completed_millis="$(monotonic_millis)"
    underlay_loss_detection_millis=$((
      underlay_loss_completed_millis - underlay_loss_started_millis
    ))
    underlay_outage_hold_millis=5000
    sleep 5
    underlay_lost_held="$state_dir/status-underlay-lost-held.json"
    if ! wait_for_automation_status \
      "$underlay_lost_held" \
      ".value.snapshot.connected and (.value.snapshot.runtime_generation == $underlay_runtime_generation_before) and (.value.snapshot.underlay.kind == \"none\") and (.value.snapshot.underlay.runtime_recovery_requests >= ($underlay_runtime_recovery_requests_before + 2)) and (.value.snapshot.underlay.runtime_recovery_failures == $underlay_runtime_recovery_failures_before)" \
      5; then
      outcome=failed
      outcome_detail="Android runtime did not remain alive during total underlay loss"
      record_step underlay_total_loss failed "$outcome_detail"
      exit 1
    fi
    record_step underlay_total_loss passed \
      "The native runtime remained alive through a five-second physical outage"

    underlay_recovery_started_millis="$(monotonic_millis)"
    if ! adb_run shell svc data enable >/dev/null; then
      outcome=failed
      outcome_detail="Emulator cellular underlay could not be restored"
      record_step underlay_cellular_recovery failed "$outcome_detail"
      exit 1
    fi
    underlay_recovered="$state_dir/status-underlay-recovered.json"
    if ! wait_for_automation_status \
      "$underlay_recovered" \
      ".value.snapshot.connected and (.value.snapshot.paths.connected_peers >= 1) and (.value.snapshot.runtime_generation == $underlay_runtime_generation_before) and (.value.snapshot.underlay.kind == \"cellular\") and .value.snapshot.underlay.validated and (.value.snapshot.underlay.selection_changes >= ($underlay_selection_changes_before + 3)) and (.value.snapshot.underlay.recoveries >= ($underlay_recoveries_before + 1)) and (.value.snapshot.underlay.runtime_recovery_requests >= ($underlay_runtime_recovery_requests_before + 3)) and (.value.snapshot.underlay.runtime_recovery_failures == $underlay_runtime_recovery_failures_before)" \
      180; then
      outcome=failed
      outcome_detail="Android did not recover automatically after physical connectivity returned"
      record_step underlay_cellular_recovery failed "$outcome_detail"
      exit 1
    fi
    underlay_recovered_kind="$(jq -r '.value.snapshot.underlay.kind' "$underlay_recovered")"
    if ! wait_for_transition_traffic_ready \
      cellular_recovery \
      "after total-loss recovery"; then
      exit 1
    fi
    underlay_recovery_completed_millis="$(monotonic_millis)"
    underlay_recovery_convergence_millis=$((
      underlay_recovery_completed_millis - underlay_recovery_started_millis
    ))
    if ! measure_bidirectional_traffic \
      cellular_recovery \
      "after total-loss recovery"; then
      exit 1
    fi
    record_step underlay_cellular_recovery passed \
      "The same runtime recovered automatically after connectivity returned"

    underlay_restore_started_millis="$(monotonic_millis)"
    if ! adb_run shell svc wifi enable >/dev/null; then
      outcome=failed
      outcome_detail="Emulator Wi-Fi underlay could not be restored"
      record_step underlay_wifi_restore failed "$outcome_detail"
      exit 1
    fi
    underlay_restored="$state_dir/status-underlay-restored.json"
    if ! wait_for_automation_status \
      "$underlay_restored" \
      ".value.snapshot.connected and (.value.snapshot.paths.connected_peers >= 1) and (.value.snapshot.runtime_generation == $underlay_runtime_generation_before) and (.value.snapshot.underlay.kind == \"wifi\") and .value.snapshot.underlay.validated and (.value.snapshot.underlay.selection_changes >= ($underlay_selection_changes_before + 4)) and (.value.snapshot.underlay.runtime_recovery_requests >= ($underlay_runtime_recovery_requests_before + 4)) and (.value.snapshot.underlay.runtime_recovery_failures == $underlay_runtime_recovery_failures_before)" \
      120; then
      outcome=failed
      outcome_detail="Android did not return autonomously to the preferred Wi-Fi underlay"
      record_step underlay_wifi_restore failed "$outcome_detail"
      exit 1
    fi
    underlay_restored_kind="$(jq -r '.value.snapshot.underlay.kind' "$underlay_restored")"
    if ! wait_for_transition_traffic_ready \
      wifi_restore \
      "after Wi-Fi restoration"; then
      exit 1
    fi
    underlay_restore_completed_millis="$(monotonic_millis)"
    underlay_restore_convergence_millis=$((
      underlay_restore_completed_millis - underlay_restore_started_millis
    ))
    if ! measure_bidirectional_traffic wifi_restore "after Wi-Fi restoration"; then
      exit 1
    fi
    record_step underlay_wifi_restore passed \
      "The existing runtime returned to the preferred Wi-Fi underlay"

    underlay_final="$state_dir/status-underlay-final.json"
    if ! wait_for_automation_status \
      "$underlay_final" \
      ".value.snapshot.connected and (.value.snapshot.runtime_generation == $underlay_runtime_generation_before) and (.value.snapshot.underlay.kind == \"wifi\") and (.value.snapshot.underlay.runtime_recovery_requests >= ($underlay_runtime_recovery_requests_before + 4)) and (.value.snapshot.underlay.runtime_recovery_failures == $underlay_runtime_recovery_failures_before)" \
      30; then
      outcome=failed
      outcome_detail="Android underlay continuity state could not be finalized"
      record_step underlay_continuity failed "$outcome_detail"
      exit 1
    fi
    underlay_runtime_generation_after="$(
      jq -r '.value.snapshot.runtime_generation' "$underlay_final"
    )"
    underlay_selection_changes_after="$(
      jq -r '.value.snapshot.underlay.selection_changes' "$underlay_final"
    )"
    underlay_selected_losses_after="$(
      jq -r '.value.snapshot.underlay.selected_losses' "$underlay_final"
    )"
    underlay_recoveries_after="$(
      jq -r '.value.snapshot.underlay.recoveries' "$underlay_final"
    )"
    underlay_runtime_recovery_requests_after="$(
      jq -r '.value.snapshot.underlay.runtime_recovery_requests' "$underlay_final"
    )"
    underlay_runtime_recovery_failures_after="$(
      jq -r '.value.snapshot.underlay.runtime_recovery_failures' "$underlay_final"
    )"
    android_process_after="$(
      adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r'
    )"
    if [[ "$android_process_after" != "$android_process_before" \
      || "$underlay_runtime_generation_after" -ne "$underlay_runtime_generation_before" \
      || "$underlay_runtime_recovery_requests_after" -lt $((underlay_runtime_recovery_requests_before + 4)) \
      || "$underlay_runtime_recovery_failures_after" -ne "$underlay_runtime_recovery_failures_before" \
      || ! -d "/proc/$fixture_pid" ]]; then
      outcome=failed
      outcome_detail="An endpoint restarted during autonomous underlay recovery"
      record_step underlay_continuity failed "$outcome_detail"
      exit 1
    fi
    underlay_android_process_continuous=true
    underlay_fixture_process_continuous=true
    record_step underlay_continuity passed \
      "Android process, native runtime, profile, and Linux fixture remained continuous"
  fi

  if [[ "$path_mode" == relay-to-direct ]]; then
    promotion_linux_relay_after="$state_dir/fixture-promotion-relay-after.txt"
    if ! "$p2p_vpn_command" daemon-state \
      --socket "$fixture_control_socket" > "$promotion_linux_relay_after"; then
      outcome=failed
      outcome_detail="Linux fixture relay measurement could not be queried"
      record_step promotion_relay_traffic failed "$outcome_detail"
      exit 1
    fi
    promotion_selected_relay_paths="$(
      awk -v peer="$android_peer_id" '
        /^peer state:/ && index($0, peer) &&
          /selected_path circuit_relay/ && /direct_paths 0/ &&
          /relay_paths [1-9][0-9]*/ { count += 1 }
        END { print count + 0 }
      ' "$promotion_linux_relay_after"
    )"
    promotion_relay_packets_after="$(
      awk '$1 == "outbound_stream_fallback_packets" { value = $2 }
        END { print value + 0 }' "$promotion_linux_relay_after"
    )"
    promotion_relay_packet_delta=$((
      promotion_relay_packets_after - promotion_relay_packets_before
    ))
    if [[ "$promotion_selected_relay_paths" -lt 1 \
      || "$promotion_relay_packet_delta" -lt 20 ]]; then
      outcome=failed
      outcome_detail="Measured baseline traffic did not remain on circuit relay"
      record_step promotion_relay_traffic failed "$outcome_detail"
      exit 1
    fi
    record_step promotion_relay_traffic passed \
      "Bidirectional dual-stack baseline used circuit relay"

    promotion_android_before="$state_dir/status-promotion-before.json"
    if ! wait_for_automation_status \
      "$promotion_android_before" \
      ".value.snapshot.connected and (.value.snapshot.runtime_generation == $promotion_runtime_generation_before) and (.value.snapshot.paths.relay >= 1) and (.value.snapshot.paths.direct_tcp_stream == 0) and (.value.snapshot.paths.promotions_to_direct == 0)" \
      30; then
      outcome=failed
      outcome_detail="Android relay baseline changed before promotion"
      record_step promotion_enable failed "$outcome_detail"
      exit 1
    fi

    promotion_started_millis="$(monotonic_millis)"
    promotion_enable_response="$state_dir/promotion-enable.json"
    if ! "$fixture_command" enable-direct \
      --socket "$fixture_packet_socket" > "$promotion_enable_response" \
      || ! jq -e \
        '.schema_version == 1 and .ok and .enabled and (.already_enabled | not)' \
        "$promotion_enable_response" >/dev/null; then
      outcome=failed
      outcome_detail="Fixture direct endpoint could not be enabled"
      record_step promotion_enable failed "$outcome_detail"
      exit 1
    fi
    record_step promotion_enable passed \
      "Authenticated direct TCP candidate became reachable without runtime restart"

    promotion_android_direct="$state_dir/status-promotion-direct.json"
    if ! wait_for_automation_status \
      "$promotion_android_direct" \
      ".value.snapshot.connected and (.value.snapshot.runtime_generation == $promotion_runtime_generation_before) and (.value.snapshot.paths.direct_tcp_stream >= 1) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.direct_quic_datagram == 0) and (.value.snapshot.paths.direct_quic_stream == 0) and (.value.snapshot.paths.promotions_to_direct >= 1)" \
      120; then
      outcome=failed
      outcome_detail="Android did not promote from relay to the direct TCP candidate"
      record_step promotion_convergence failed "$outcome_detail"
      exit 1
    fi

    promotion_linux_direct="$state_dir/fixture-promotion-direct.txt"
    promotion_linux_selected_direct=0
    promotion_linux_events_after=0
    for _ in $(seq 1 120); do
      if "$p2p_vpn_command" daemon-state \
        --socket "$fixture_control_socket" > "$promotion_linux_direct"; then
        promotion_linux_selected_direct="$(
          awk -v peer="$android_peer_id" '
            /^peer state:/ && index($0, peer) &&
              /selected_path direct_tcp_stream/ &&
              /direct_paths [1-9][0-9]*/ { count += 1 }
            END { print count + 0 }
          ' "$promotion_linux_direct"
        )"
        promotion_linux_events_after="$(
          awk '$1 == "path_promotions_to_direct" { value = $2 }
            END { print value + 0 }' "$promotion_linux_direct"
        )"
        if [[ "$promotion_linux_selected_direct" -ge 1 \
          && "$promotion_linux_events_after" -ge 1 ]]; then
          break
        fi
      fi
      sleep 1
    done
    if [[ "$promotion_linux_selected_direct" -lt 1 \
      || "$promotion_linux_events_after" -lt 1 ]]; then
      outcome=failed
      outcome_detail="Linux fixture did not observe relay-to-direct promotion"
      record_step promotion_convergence failed "$outcome_detail"
      exit 1
    fi
    promotion_completed_millis="$(monotonic_millis)"
    promotion_convergence_millis=$((
      promotion_completed_millis - promotion_started_millis
    ))
    promotion_runtime_generation_after="$(
      jq -r '.value.snapshot.runtime_generation' "$promotion_android_direct"
    )"
    promotion_android_direct_packets_before="$(
      jq -r '.value.snapshot.paths.outbound_direct_tcp_stream_packets' \
        "$promotion_android_direct"
    )"
    promotion_linux_direct_packets_before="$(
      awk '$1 == "outbound_direct_tcp_stream_fallback_packets" { value = $2 }
        END { print value + 0 }' "$promotion_linux_direct"
    )"
    if ! kill -0 "$fixture_pid" 2>/dev/null \
      || [[ "$promotion_runtime_generation_after" -ne "$promotion_runtime_generation_before" ]]; then
      outcome=failed
      outcome_detail="An endpoint restarted during relay-to-direct promotion"
      record_step promotion_convergence failed "$outcome_detail"
      exit 1
    fi
    promotion_fixture_process_continuous=true
    record_step promotion_convergence passed \
      "Both runtimes selected direct TCP without restarting"

    promoted_linux_ipv4_probe="$state_dir/promoted-linux-ipv4-probe.json"
    promoted_linux_ipv6_probe="$state_dir/promoted-linux-ipv6-probe.json"
    if ! "$fixture_command" probe \
      --socket "$fixture_packet_socket" \
      --source "$fixture_ipv4" \
      --destination "$android_ipv4" \
      --count 5 > "$promoted_linux_ipv4_probe" \
      || ! jq -e '.schema_version == 1 and .ok and .family == "ipv4" and .sent == 5 and .received == 5' \
        "$promoted_linux_ipv4_probe" >/dev/null; then
      outcome=failed
      outcome_detail="Promoted Linux-to-Android IPv4 overlay probe failed"
      record_step promoted_linux_to_android_ipv4 failed "$outcome_detail"
      exit 1
    fi
    record_step promoted_linux_to_android_ipv4 passed \
      "Linux received 5 of 5 IPv4 replies over the promoted path"
    if ! "$fixture_command" probe \
      --socket "$fixture_packet_socket" \
      --source "$fixture_ipv6" \
      --destination "$android_ipv6" \
      --count 5 > "$promoted_linux_ipv6_probe" \
      || ! jq -e '.schema_version == 1 and .ok and .family == "ipv6" and .sent == 5 and .received == 5' \
        "$promoted_linux_ipv6_probe" >/dev/null; then
      outcome=failed
      outcome_detail="Promoted Linux-to-Android IPv6 overlay probe failed"
      record_step promoted_linux_to_android_ipv6 failed "$outcome_detail"
      exit 1
    fi
    record_step promoted_linux_to_android_ipv6 passed \
      "Linux received 5 of 5 IPv6 replies over the promoted path"

    promoted_android_ipv4_ping="$state_dir/promoted-android-ipv4-ping.txt"
    promoted_android_ipv6_ping="$state_dir/promoted-android-ipv6-ping.txt"
    if ! adb_run shell ping -c 5 -W 5 "$fixture_ipv4" \
      > "$promoted_android_ipv4_ping" 2>&1 \
      || ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
        "$promoted_android_ipv4_ping"; then
      received="$(ping_received_count "$promoted_android_ipv4_ping")"
      outcome=failed
      outcome_detail="Promoted Android-to-Linux IPv4 ping received $received of 5 replies"
      record_step promoted_android_to_linux_ipv4 failed "$outcome_detail"
      exit 1
    fi
    record_step promoted_android_to_linux_ipv4 passed \
      "Android received 5 of 5 IPv4 replies over the promoted path"
    if ! adb_run shell ping6 -c 5 -W 5 "$fixture_ipv6" \
      > "$promoted_android_ipv6_ping" 2>&1 \
      || ! grep -Eq '5 packets transmitted, 5 (packets )?received' \
        "$promoted_android_ipv6_ping"; then
      received="$(ping_received_count "$promoted_android_ipv6_ping")"
      outcome=failed
      outcome_detail="Promoted Android-to-Linux IPv6 ping received $received of 5 replies"
      record_step promoted_android_to_linux_ipv6 failed "$outcome_detail"
      exit 1
    fi
    record_step promoted_android_to_linux_ipv6 passed \
      "Android received 5 of 5 IPv6 replies over the promoted path"

    promotion_android_after="$state_dir/status-promotion-after.json"
    if ! wait_for_automation_status \
      "$promotion_android_after" \
      ".value.snapshot.connected and (.value.snapshot.runtime_generation == $promotion_runtime_generation_before) and (.value.snapshot.paths.direct_tcp_stream >= 1) and (.value.snapshot.paths.promotions_to_direct >= 1) and (.value.snapshot.paths.outbound_direct_tcp_stream_packets >= ($promotion_android_direct_packets_before + 20))" \
      60; then
      outcome=failed
      outcome_detail="Android direct TCP counters did not cover promoted traffic"
      record_step promotion_direct_traffic failed "$outcome_detail"
      exit 1
    fi
    promotion_android_direct_packets_after="$(
      jq -r '.value.snapshot.paths.outbound_direct_tcp_stream_packets' \
        "$promotion_android_after"
    )"
    promotion_android_direct_packet_delta=$((
      promotion_android_direct_packets_after - promotion_android_direct_packets_before
    ))

    promotion_linux_after="$state_dir/fixture-promotion-after.txt"
    if ! "$p2p_vpn_command" daemon-state \
      --socket "$fixture_control_socket" > "$promotion_linux_after"; then
      outcome=failed
      outcome_detail="Linux fixture direct traffic state could not be queried"
      record_step promotion_direct_traffic failed "$outcome_detail"
      exit 1
    fi
    promotion_linux_direct_packets_after="$(
      awk '$1 == "outbound_direct_tcp_stream_fallback_packets" { value = $2 }
        END { print value + 0 }' "$promotion_linux_after"
    )"
    promotion_linux_direct_packet_delta=$((
      promotion_linux_direct_packets_after - promotion_linux_direct_packets_before
    ))
    promotion_linux_selected_direct="$(
      awk -v peer="$android_peer_id" '
        /^peer state:/ && index($0, peer) &&
          /selected_path direct_tcp_stream/ &&
          /direct_paths [1-9][0-9]*/ { count += 1 }
        END { print count + 0 }
      ' "$promotion_linux_after"
    )"
    if [[ "$promotion_linux_selected_direct" -lt 1 \
      || "$promotion_linux_direct_packet_delta" -lt 20 \
      || "$promotion_android_direct_packet_delta" -lt 20 ]]; then
      outcome=failed
      outcome_detail="Promoted traffic was not carried bidirectionally over direct TCP"
      record_step promotion_direct_traffic failed "$outcome_detail"
      exit 1
    fi
    record_step promotion_direct_traffic passed \
      "Bidirectional dual-stack traffic used the promoted direct TCP path"
  fi

  relay_selected_paths=0
  relay_stream_packets=0
  relay_established_circuits=0
  if [[ "$path_mode" == relay-only ]]; then
    fixture_relay_state="$state_dir/fixture-relay-state.txt"
    if ! "$p2p_vpn_command" daemon-state \
      --socket "$fixture_control_socket" > "$fixture_relay_state"; then
      outcome=failed
      outcome_detail="Linux fixture relay state could not be queried"
      record_step relay_path_isolation failed "$outcome_detail"
      exit 1
    fi
    relay_selected_paths="$(
      awk '
        /^peer state:/ &&
          /selected_path circuit_relay/ &&
          /direct_paths 0/ &&
          /relay_paths [1-9][0-9]*/ { count += 1 }
        END { print count + 0 }
      ' "$fixture_relay_state"
    )"
    relay_stream_packets="$(
      awk '$1 == "outbound_stream_fallback_packets" { value = $2 }
        END { print value + 0 }' "$fixture_relay_state"
    )"
    relay_established_circuits="$(
      awk '$1 ~ /^relay_(inbound|outbound)_circuits_established$/ { total += $2 }
        END { print total + 0 }' "$fixture_relay_state"
    )"
    if [[ ! "$relay_selected_paths" =~ ^[0-9]+$ || "$relay_selected_paths" -lt 1 ]]; then
      outcome=failed
      outcome_detail="Linux fixture did not select an isolated circuit-relay path"
      record_step relay_path_isolation failed "$outcome_detail"
      exit 1
    fi
    if [[ ! "$relay_stream_packets" =~ ^[0-9]+$ || "$relay_stream_packets" -lt 20 \
      || ! "$relay_established_circuits" =~ ^[0-9]+$ \
      || "$relay_established_circuits" -lt 1 ]]; then
      outcome=failed
      outcome_detail="Linux fixture relay counters did not cover measured traffic"
      record_step relay_path_isolation failed "$outcome_detail"
      exit 1
    fi
    record_step relay_path_isolation passed \
      "Linux fixture selected circuit relay with no direct overlay path"
  fi

  minimum_owned_quic_packets=0
  case "$path_mode" in
    automatic)
      path_predicate='.value.snapshot.paths.connected_peers >= 1'
      ;;
    quic-stream)
      path_predicate='(.value.snapshot.paths.direct_quic_stream >= 1) and (.value.snapshot.paths.direct_tcp_stream == 0) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.direct_quic_datagram == 0) and (.value.snapshot.paths.relay == 0)'
      ;;
    tcp-stream)
      path_predicate='(.value.snapshot.paths.direct_tcp_stream >= 1) and (.value.snapshot.paths.direct_quic_stream == 0) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.direct_quic_datagram == 0) and (.value.snapshot.paths.relay == 0)'
      ;;
    owned-quic)
      minimum_owned_quic_packets=$((owned_quic_packets_before + 20))
      path_predicate="(.value.snapshot.paths.direct_quic_datagram >= 1) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.relay == 0) and (.value.snapshot.paths.packet_plane_quic_sessions >= 1) and (.value.snapshot.paths.outbound_quic_datagram_packets >= $minimum_owned_quic_packets)"
      ;;
    relay-only)
      path_predicate='(.value.snapshot.paths.relay >= 1) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.direct_quic_datagram == 0) and (.value.snapshot.paths.direct_quic_stream == 0) and (.value.snapshot.paths.direct_tcp_stream == 0) and (.value.snapshot.paths.packet_plane_quic_sessions == 0)'
      ;;
    relay-to-direct)
      path_predicate="(.value.snapshot.paths.connected_peers >= 1) and (.value.snapshot.runtime_generation == $promotion_runtime_generation_before) and (.value.snapshot.paths.direct_tcp_stream >= 1) and (.value.snapshot.paths.direct_udp_datagram == 0) and (.value.snapshot.paths.direct_quic_datagram == 0) and (.value.snapshot.paths.direct_quic_stream == 0) and (.value.snapshot.paths.promotions_to_direct >= 1)"
      ;;
  esac
  final_status="$state_dir/status-final.json"
  if ! wait_for_automation_status "$final_status" "$path_predicate" 60; then
    if jq -e \
      '.schema_version == 1 and .ok and (.value.snapshot.paths | type == "object")' \
      "$final_status" >/dev/null 2>&1; then
      jq \
        --arg path_mode "$path_mode" \
        --argjson paths "$(jq '.value.snapshot.paths' "$final_status")" \
        --argjson owned_quic_packets_before "$owned_quic_packets_before" \
        --argjson minimum_owned_quic_packets "$minimum_owned_quic_packets" '
        . + {
          pairing_traffic: {
            path_mode: $path_mode,
            path_observation: $paths,
            owned_quic: {
              outbound_packets_before: $owned_quic_packets_before,
              minimum_outbound_packets: $minimum_owned_quic_packets
            }
          }
        }
      ' "$device_file" > "$device_file.updated"
      mv -f "$device_file.updated" "$device_file"
    fi
    outcome=failed
    outcome_detail="Android did not retain the required $path_mode path isolation"
    record_step path_isolation failed "$outcome_detail"
    exit 1
  fi
  record_step path_isolation passed "Final runtime status matched $path_mode requirements"

  owned_quic_packets_after=0
  owned_quic_packet_delta=0
  if [[ "$path_mode" == owned-quic ]]; then
    owned_quic_packets_after="$(
      jq -r '.value.snapshot.paths.outbound_quic_datagram_packets' "$final_status"
    )"
    owned_quic_packet_delta=$((owned_quic_packets_after - owned_quic_packets_before))
  fi

  jq \
    --arg scenario "$scenario" \
    --arg path_mode "$path_mode" \
    --argjson paths "$(jq '.value.snapshot.paths' "$final_status")" \
    --argjson fixture_ipv4_ready_attempts "$fixture_ipv4_ready_attempts" \
    --argjson fixture_ipv6_ready_attempts "$fixture_ipv6_ready_attempts" \
    --argjson android_ipv4_ready_attempts "$android_ipv4_ready_attempts" \
    --argjson android_ipv6_ready_attempts "$android_ipv6_ready_attempts" \
    --argjson owned_quic_packets_before "$owned_quic_packets_before" \
    --argjson owned_quic_packets_after "$owned_quic_packets_after" \
    --argjson owned_quic_packet_delta "$owned_quic_packet_delta" \
    --argjson relay_selected_paths "$relay_selected_paths" \
    --argjson relay_stream_packets "$relay_stream_packets" \
    --argjson relay_established_circuits "$relay_established_circuits" \
    --arg promotion_path "$fixture_promotion_path" \
    --argjson promotion_runtime_generation_before "$promotion_runtime_generation_before" \
    --argjson promotion_runtime_generation_after "$promotion_runtime_generation_after" \
    --argjson promotion_relay_packets_before "$promotion_relay_packets_before" \
    --argjson promotion_relay_packets_after "$promotion_relay_packets_after" \
    --argjson promotion_relay_packet_delta "$promotion_relay_packet_delta" \
    --argjson promotion_linux_direct_packets_before "$promotion_linux_direct_packets_before" \
    --argjson promotion_linux_direct_packets_after "$promotion_linux_direct_packets_after" \
    --argjson promotion_linux_direct_packet_delta "$promotion_linux_direct_packet_delta" \
    --argjson promotion_android_direct_packets_before "$promotion_android_direct_packets_before" \
    --argjson promotion_android_direct_packets_after "$promotion_android_direct_packets_after" \
    --argjson promotion_android_direct_packet_delta "$promotion_android_direct_packet_delta" \
    --argjson promotion_convergence_millis "$promotion_convergence_millis" \
    --argjson promotion_fixture_process_continuous "$promotion_fixture_process_continuous" \
    --arg underlay_initial_kind "$underlay_initial_kind" \
    --arg underlay_fallback_kind "$underlay_fallback_kind" \
    --arg underlay_recovered_kind "$underlay_recovered_kind" \
    --arg underlay_restored_kind "$underlay_restored_kind" \
    --argjson underlay_selection_changes_before "$underlay_selection_changes_before" \
    --argjson underlay_selection_changes_after "$underlay_selection_changes_after" \
    --argjson underlay_selected_losses_before "$underlay_selected_losses_before" \
    --argjson underlay_selected_losses_after "$underlay_selected_losses_after" \
    --argjson underlay_recoveries_before "$underlay_recoveries_before" \
    --argjson underlay_recoveries_after "$underlay_recoveries_after" \
    --argjson underlay_runtime_recovery_requests_before "$underlay_runtime_recovery_requests_before" \
    --argjson underlay_runtime_recovery_requests_after "$underlay_runtime_recovery_requests_after" \
    --argjson underlay_runtime_recovery_failures_before "$underlay_runtime_recovery_failures_before" \
    --argjson underlay_runtime_recovery_failures_after "$underlay_runtime_recovery_failures_after" \
    --argjson underlay_runtime_generation_before "$underlay_runtime_generation_before" \
    --argjson underlay_runtime_generation_after "$underlay_runtime_generation_after" \
    --argjson underlay_fallback_convergence_millis "$underlay_fallback_convergence_millis" \
    --argjson underlay_loss_detection_millis "$underlay_loss_detection_millis" \
    --argjson underlay_recovery_convergence_millis "$underlay_recovery_convergence_millis" \
    --argjson underlay_restore_convergence_millis "$underlay_restore_convergence_millis" \
    --argjson underlay_outage_hold_millis "$underlay_outage_hold_millis" \
    --argjson underlay_android_process_continuous "$underlay_android_process_continuous" \
    --argjson underlay_fixture_process_continuous "$underlay_fixture_process_continuous" '
    . + {
      pairing_traffic: ({
        code_only_enrollment: true,
        configured_overlay_peer_addresses: 0,
        path_mode: $path_mode,
        readiness_attempts: {
          linux_to_android_ipv4: $fixture_ipv4_ready_attempts,
          linux_to_android_ipv6: $fixture_ipv6_ready_attempts,
          android_to_linux_ipv4: $android_ipv4_ready_attempts,
          android_to_linux_ipv6: $android_ipv6_ready_attempts
        },
        linux_to_android: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}},
        android_to_linux: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}},
        paths: $paths
      } + (if $path_mode == "owned-quic" then {
        owned_quic: {
          outbound_packets_before: $owned_quic_packets_before,
          outbound_packets_after: $owned_quic_packets_after,
          measured_packet_delta: $owned_quic_packet_delta
        }
      } elif $path_mode == "relay-only" then {
        relay_only: {
          selected_circuit_paths: $relay_selected_paths,
          outbound_stream_packets: $relay_stream_packets,
          established_circuits: $relay_established_circuits
        }
      } elif $path_mode == "relay-to-direct" then {
        relay_to_direct: {
          initial_path: "circuit_relay",
          promoted_path: $promotion_path,
          convergence_millis: $promotion_convergence_millis,
          runtime_generation_before: $promotion_runtime_generation_before,
          runtime_generation_after: $promotion_runtime_generation_after,
          runtime_restarted: ($promotion_runtime_generation_before != $promotion_runtime_generation_after),
          fixture_process_continuous: $promotion_fixture_process_continuous,
          relay_packets_before: $promotion_relay_packets_before,
          relay_packets_after: $promotion_relay_packets_after,
          measured_relay_packet_delta: $promotion_relay_packet_delta,
          linux_direct_packets_before: $promotion_linux_direct_packets_before,
          linux_direct_packets_after: $promotion_linux_direct_packets_after,
          linux_measured_direct_packet_delta: $promotion_linux_direct_packet_delta,
          android_direct_packets_before: $promotion_android_direct_packets_before,
          android_direct_packets_after: $promotion_android_direct_packets_after,
          android_measured_direct_packet_delta: $promotion_android_direct_packet_delta,
          promoted_linux_to_android: {
            ipv4: {sent: 5, received: 5},
            ipv6: {sent: 5, received: 5}
          },
          promoted_android_to_linux: {
            ipv4: {sent: 5, received: 5},
            ipv6: {sent: 5, received: 5}
          }
        }
      } else {} end))
    } + (if $scenario == "underlay-recovery" then {
      underlay_recovery: {
        underlays: {
          initial: $underlay_initial_kind,
          fallback: $underlay_fallback_kind,
          outage: "none",
          recovered: $underlay_recovered_kind,
          restored: $underlay_restored_kind
        },
        events: {
          selection_changes_before: $underlay_selection_changes_before,
          selection_changes_after: $underlay_selection_changes_after,
          selected_losses_before: $underlay_selected_losses_before,
          selected_losses_after: $underlay_selected_losses_after,
          recoveries_before: $underlay_recoveries_before,
          recoveries_after: $underlay_recoveries_after,
          runtime_recovery_requests_before: $underlay_runtime_recovery_requests_before,
          runtime_recovery_requests_after: $underlay_runtime_recovery_requests_after,
          runtime_recovery_failures_before: $underlay_runtime_recovery_failures_before,
          runtime_recovery_failures_after: $underlay_runtime_recovery_failures_after
        },
        timing_millis: {
          cellular_fallback: $underlay_fallback_convergence_millis,
          total_loss_detection: $underlay_loss_detection_millis,
          outage_hold: $underlay_outage_hold_millis,
          cellular_recovery: $underlay_recovery_convergence_millis,
          wifi_restore: $underlay_restore_convergence_millis
        },
        continuity: {
          runtime_generation_before: $underlay_runtime_generation_before,
          runtime_generation_after: $underlay_runtime_generation_after,
          runtime_restarted: ($underlay_runtime_generation_before != $underlay_runtime_generation_after),
          android_process_continuous: $underlay_android_process_continuous,
          fixture_process_continuous: $underlay_fixture_process_continuous
        },
        traffic: {
          cellular_fallback: {
            linux_to_android: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}},
            android_to_linux: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}}
          },
          cellular_recovery: {
            linux_to_android: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}},
            android_to_linux: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}}
          },
          wifi_restore: {
            linux_to_android: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}},
            android_to_linux: {ipv4: {sent: 5, received: 5}, ipv6: {sent: 5, received: 5}}
          }
        }
      }
    } else {} end)
  ' "$device_file" > "$device_file.updated"
  mv -f "$device_file.updated" "$device_file"

  outcome=passed
  if [[ "$scenario" == underlay-recovery ]]; then
    outcome_detail="Code pairing, autonomous underlay recovery, and dual-stack traffic passed"
  else
    outcome_detail="Code pairing, $path_mode path isolation, and dual-stack traffic passed"
  fi
  exit 0
fi

baseline_profile="$state_dir/profile-baseline.json"
command_response="$state_dir/automation-command.json"
if ! android_automation create-profile --es network android-e2e > "$command_response" \
  || ! jq -e \
    '.schema_version == 1 and .ok and .value.accepted and .value.command == "create-profile"' \
    "$command_response" >/dev/null; then
  outcome=failed
  outcome_detail="Debug automation did not accept profile creation"
  record_step profile_creation failed "$outcome_detail"
  exit 1
fi
if ! wait_for_automation_status \
  "$baseline_profile" \
  '.value.snapshot.has_profile and .value.snapshot.profile_stored and (.value.snapshot.busy | not)'; then
  outcome=failed
  outcome_detail="Encrypted profile creation did not complete"
  record_step profile_creation failed "$outcome_detail"
  exit 1
fi
if ! jq -e '
  (.value.snapshot.peer_id | type == "string" and length > 0 and length <= 256) and
  (.value.snapshot.addresses | any(contains("."))) and
  (.value.snapshot.addresses | any(contains(":")))
' "$baseline_profile" >/dev/null; then
  outcome=failed
  outcome_detail="Created profile does not expose valid dual-stack identity metadata"
  record_step profile_creation failed "$outcome_detail"
  exit 1
fi
record_step profile_creation passed "Encrypted dual-stack profile created through the real app"

assert_profile_unchanged() {
  local current="$1"
  jq -e -s '
    .[0].value.snapshot.peer_id == .[1].value.snapshot.peer_id and
    .[0].value.snapshot.network_name == .[1].value.snapshot.network_name and
    .[0].value.snapshot.addresses == .[1].value.snapshot.addresses
  ' "$baseline_profile" "$current" >/dev/null
}

if [[ "$scenario" == always-on ]]; then
  if ! adb_run shell appops set \
    org.hermeticfoundation.p2pvpn.debug ACTIVATE_VPN allow >/dev/null; then
    outcome=failed
    outcome_detail="Android did not grant VPN consent for always-on validation"
    record_step vpn_consent failed "$outcome_detail"
    exit 1
  fi
  record_step vpn_consent passed "VPN consent granted through emulator automation"

  if ! android_automation connect > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "connect"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept the initial VPN connection"
    record_step manual_connect failed "$outcome_detail"
    exit 1
  fi
  manual_status="$state_dir/status-manual.json"
  if ! wait_for_automation_status \
    "$manual_status" \
    '.value.snapshot.connected and (.value.snapshot.always_on | not) and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not)' \
    90; then
    outcome=failed
    outcome_detail="Android VPN runtime did not establish the initial manual connection"
    record_step manual_connect failed "$outcome_detail"
    exit 1
  fi
  record_step manual_connect passed "Manual split-tunnel VPN connection established"

  if ! set_android_vpn_mode true false; then
    outcome=failed
    outcome_detail="Android did not enable always-on VPN mode"
    record_step always_on_enable failed "$outcome_detail"
    exit 1
  fi
  always_on_status="$state_dir/status-always-on.json"
  if ! wait_for_automation_status \
    "$always_on_status" \
    '.value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not)' \
    30; then
    outcome=failed
    outcome_detail="The service did not observe Android always-on ownership"
    record_step always_on_enable failed "$outcome_detail"
    exit 1
  fi
  record_step always_on_enable passed "Android assumed always-on ownership of the VPN"

  always_on_generation="$(jq -r '.value.snapshot.runtime_generation' "$always_on_status")"
  if ! android_automation disconnect > "$command_response" \
    || ! jq -e \
      '.schema_version == 1 and .ok and .value.accepted and .value.command == "disconnect"' \
      "$command_response" >/dev/null; then
    outcome=failed
    outcome_detail="Debug automation did not accept the disconnect probe"
    record_step disconnect_guard failed "$outcome_detail"
    exit 1
  fi
  guarded_status="$state_dir/status-after-disconnect.json"
  if ! wait_for_automation_status \
    "$guarded_status" \
    ".value.snapshot.connected and .value.snapshot.connection_requested and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.runtime_generation == $always_on_generation)" \
    15; then
    outcome=failed
    outcome_detail="In-app disconnect interrupted Android's always-on VPN"
    record_step disconnect_guard failed "$outcome_detail"
    exit 1
  fi
  record_step disconnect_guard passed "In-app disconnect was ignored while Android owned the VPN"

  process_before_update="$(adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r')"
  if [[ ! "$process_before_update" =~ ^[0-9]+$ ]]; then
    outcome=failed
    outcome_detail="The pre-update Android process ID was unavailable"
    record_step always_on_update_restart failed "$outcome_detail"
    exit 1
  fi
  if ! adb_run install -r "$android_apk" >/dev/null; then
    outcome=failed
    outcome_detail="ADB replacement install failed during always-on validation"
    record_step always_on_update_restart failed "$outcome_detail"
    exit 1
  fi
  updated_status="$state_dir/status-after-always-on-update.json"
  if ! wait_for_automation_status \
    "$updated_status" \
    '.value.snapshot.has_profile and .value.snapshot.profile_stored and .value.snapshot.connected and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not)' \
    90 \
    || ! assert_profile_unchanged "$updated_status"; then
    outcome=failed
    outcome_detail="Always-on VPN did not restore the same profile after an app update"
    record_step always_on_update_restart failed "$outcome_detail"
    exit 1
  fi
  process_after_update="$(adb_run shell pidof org.hermeticfoundation.p2pvpn.debug | tr -d '\r')"
  if [[ ! "$process_after_update" =~ ^[0-9]+$ \
    || "$process_after_update" == "$process_before_update" ]]; then
    outcome=failed
    outcome_detail="The app update did not prove a fresh always-on service process"
    record_step always_on_update_restart failed "$outcome_detail"
    exit 1
  fi
  record_step always_on_update_restart passed \
    "Android restarted the VPN with the same encrypted profile after an app update"

  if ! set_android_vpn_mode true true; then
    outcome=failed
    outcome_detail="Android did not enable the lockdown validation state"
    record_step lockdown_guard failed "$outcome_detail"
    exit 1
  fi
  lockdown_status="$state_dir/status-lockdown.json"
  if ! wait_for_automation_status \
    "$lockdown_status" \
    '.value.snapshot.always_on and .value.snapshot.lockdown and (.value.snapshot.connected | not) and .value.snapshot.connection_requested and (.value.snapshot.connection_detail | contains("Block connections without VPN"))' \
    30; then
    outcome=failed
    outcome_detail="The service did not stop and report unsupported Android lockdown"
    record_step lockdown_guard failed "$outcome_detail"
    exit 1
  fi
  record_step lockdown_guard passed "Unsupported Android lockdown stopped the split tunnel"

  if ! set_android_vpn_mode true false; then
    outcome=failed
    outcome_detail="Android did not disable the lockdown validation state"
    record_step lockdown_recovery failed "$outcome_detail"
    exit 1
  fi
  restored_status="$state_dir/status-after-lockdown.json"
  if ! wait_for_automation_status \
    "$restored_status" \
    '.value.snapshot.connected and .value.snapshot.connection_requested and .value.snapshot.always_on and (.value.snapshot.lockdown | not) and (.value.snapshot.busy | not)' \
    45 \
    || ! assert_profile_unchanged "$restored_status"; then
    outcome=failed
    outcome_detail="Always-on VPN did not recover automatically after lockdown was disabled"
    record_step lockdown_recovery failed "$outcome_detail"
    exit 1
  fi
  record_step lockdown_recovery passed \
    "The split tunnel recovered automatically after lockdown was disabled"

  jq '
    . + {
      always_on: {
        manual_connect: true,
        disconnect_guard: true,
        update_restart: true,
        lockdown_guard: true,
        lockdown_recovery: true,
        profile_identity_preserved: true
      }
    }
  ' "$device_file" > "$device_file.updated"
  mv -f "$device_file.updated" "$device_file"

  outcome=passed
  outcome_detail="Always-on ownership, restart, lockdown guard, and recovery passed"
  exit 0
fi

adb_run shell am force-stop org.hermeticfoundation.p2pvpn.debug
start_main_activity
process_profile="$state_dir/profile-after-process-death.json"
if ! wait_for_automation_status "$process_profile" '.value.snapshot.has_profile' \
  || ! assert_profile_unchanged "$process_profile"; then
  outcome=failed
  outcome_detail="Profile identity changed after process death"
  record_step process_death failed "$outcome_detail"
  exit 1
fi
record_step process_death passed "Encrypted profile restored after process death"

for install_kind in update reinstall; do
  if ! adb_run install -r "$android_apk" >/dev/null; then
    outcome=failed
    outcome_detail="ADB replacement install failed during $install_kind"
    record_step "$install_kind" failed "$outcome_detail"
    exit 1
  fi
  start_main_activity
  installed_profile="$state_dir/profile-after-$install_kind.json"
  if ! wait_for_automation_status "$installed_profile" '.value.snapshot.has_profile' \
    || ! assert_profile_unchanged "$installed_profile"; then
    outcome=failed
    outcome_detail="Profile identity changed after $install_kind"
    record_step "$install_kind" failed "$outcome_detail"
    exit 1
  fi
  record_step "$install_kind" passed "Encrypted profile survived replacement install"
done

jq '
  . + {
    profile_persistence: {
      process_death: true,
      update_install: true,
      replacement_reinstall: true,
      ipv4: true,
      ipv6: true
    }
  }
' "$device_file" > "$device_file.updated"
mv -f "$device_file.updated" "$device_file"

outcome=passed
outcome_detail="Encrypted profile identity survived process death and replacement installs"
exit 0
