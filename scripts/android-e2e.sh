#!/usr/bin/env bash
# shellcheck disable=SC2329
set -euo pipefail

umask 077

readonly evidence_schema_version=1
readonly maximum_log_bytes=$((1024 * 1024))
readonly default_minimum_free_bytes=$((16 * 1024 * 1024 * 1024))
readonly default_maximum_runtime_growth_bytes=$((8 * 1024 * 1024 * 1024))
readonly hard_maximum_runtime_growth_bytes=$((32 * 1024 * 1024 * 1024))

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
runtime_storage_watchdog_pid=""
emulator_serial=""
state_dir=""
runtime_storage_failure_file=""
runtime_tmp_baseline_available_bytes=""
runtime_output_baseline_available_bytes=""
cleanup_emulator_stopped=false
cleanup_fixture_stopped=false
cleanup_private_state_removed=false
readonly harness_pid="$BASHPID"

usage() {
  cat <<'EOF'
Usage: p2p-vpn-android-e2e [OPTIONS]

Options:
  --scenario NAME        Select boot-smoke, profile-persistence, or pairing-traffic.
  --path-mode MODE       Select automatic, quic-stream, tcp-stream, owned-quic, or relay-only.
  --preflight            Check requirements without starting an emulator.
  --allow-skip           Exit 77 instead of 2 when requirements are unavailable.
  --output DIRECTORY     Write bounded evidence to DIRECTORY.
  -h, --help             Show this help.

Environment:
  P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES
                         Required runtime free space; defaults to 16 GiB.
  P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES
                         Runtime growth limit; defaults to 8 GiB and cannot exceed 32 GiB.

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

case "$scenario" in
  boot-smoke|profile-persistence|pairing-traffic) ;;
  *)
    echo "unsupported Android E2E scenario: $scenario" >&2
    exit 2
    ;;
esac

case "$path_mode" in
  automatic|quic-stream|tcp-stream|owned-quic|relay-only) ;;
  *)
    echo "unsupported Android E2E path mode: $path_mode" >&2
    exit 2
    ;;
esac
if [[ "$scenario" != pairing-traffic && "$path_mode" != automatic ]]; then
  echo "--path-mode is supported only by pairing-traffic" >&2
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

: > "$checks_file"
: > "$steps_file"
: > "$emulator_log"
: > "$fixture_log"
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
    "${adb[@]}" shell am broadcast \
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
  for _ in $(seq 1 "$attempts"); do
    if "$p2p_vpn_command" pair status "$operation_id" \
      --socket "$fixture_control_socket" \
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
  local attempt
  for attempt in $(seq 1 30); do
    if "$fixture_command" probe \
      --socket "$fixture_packet_socket" \
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
    if "${adb[@]}" shell "${command[@]}" -c 1 -W 2 "$destination" \
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

start_main_activity() {
  "${adb[@]}" shell am start \
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

sanitize_fixture_log() {
  [[ -f "$fixture_log" ]] || return 0
  sed -E \
    -e 's#/home/[^/[:space:]]+#/home/REDACTED#g' \
    -e 's#/tmp/p2p-vpn-android-e2e-state\.[^/[:space:]]+#/tmp/p2p-vpn-android-e2e-state.REDACTED#g' \
    -e 's/[A-Z2-9]{4}(-[A-Z2-9]{4}){3}/PAIRING-CODE-REDACTED/g' \
    -e '/(membership_key|membership_tag: Some\(|member_public_key|private_key|certificate_der: Some\(\[|signature:)/d' \
    -e 's#/dns(4|6)?/[^/[:space:]"}]+#/dns/UNDERLAY-REDACTED#g' \
    -e 's#/ip6/[^/[:space:]"}]+#/ip6/IPV6-REDACTED#g' \
    -e 's/\[?[[:xdigit:]]{1,4}(:[[:xdigit:]]{0,4}){2,7}\]?(:[0-9]+)?/IPV6-REDACTED/g' \
    -e 's/([0-9]{1,3}\.){3}[0-9]{1,3}/IPV4-REDACTED/g' \
    "$fixture_log" > "$fixture_log.sanitized"
  mv -f "$fixture_log.sanitized" "$fixture_log"
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

stop_fixture() {
  if [[ -z "$fixture_pid" ]]; then
    cleanup_fixture_stopped=true
    return 0
  fi
  if kill -0 "$fixture_pid" 2>/dev/null; then
    kill -INT "$fixture_pid" 2>/dev/null || true
    for _ in $(seq 1 15); do
      if ! kill -0 "$fixture_pid" 2>/dev/null; then
        break
      fi
      sleep 1
    done
  fi
  if kill -0 "$fixture_pid" 2>/dev/null; then
    kill -KILL "$fixture_pid" 2>/dev/null || true
  fi
  wait "$fixture_pid" 2>/dev/null || true
  if kill -0 "$fixture_pid" 2>/dev/null; then
    cleanup_fixture_stopped=false
  else
    cleanup_fixture_stopped=true
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
  sanitize_emulator_log
  sanitize_fixture_log
  bound_file "$emulator_log"
  bound_file "$fixture_log"
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
        private_state_removed: $private_state_removed
      },
      artifacts: {
        emulator_log: "emulator.log",
        fixture_log: "fixture.log"
      }
    }' > "$evidence_file"
  rm -f "$checks_file" "$steps_file" "$device_file"
}

exit_handler() {
  local status="$1"
  local storage_failure=""
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
  stop_emulator
  stop_fixture
  remove_private_state
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

if [[ "$scenario" == pairing-traffic ]]; then
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
fixture_bootstrap_peer=""
fixture_bootstrap_address=""
fixture_kademlia_protocol=""
fixture_ipv4=""
fixture_ipv6=""
fixture_control_socket=""
fixture_packet_socket=""
fixture_owned_quic_listen=""
fixture_owned_quic_external_endpoint=""
fixture_owned_quic_host_port=""
fixture_owned_quic_guest_port=""
fixture_relay_reservation=""

if [[ "$scenario" == pairing-traffic ]]; then
  fixture_state_dir="$state_dir/fixture"
  mkdir -p "$fixture_state_dir"
  fixture_metadata="$fixture_state_dir/fixture.json"
  "$fixture_command" run \
    --state-dir "$fixture_state_dir" \
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
  if [[ ! -s "$fixture_metadata" ]] || ! jq -e --arg path_mode "$path_mode" '
    .schema_version == 1 and
    (.network | type == "string" and length > 0 and length <= 128) and
    .path_mode == $path_mode and
    (.bootstrap.peer_id | type == "string" and test("^[A-Za-z0-9]+$") and length <= 256) and
    (.bootstrap.android_address | type == "string" and test("^/[^[:space:]]+$") and length <= 1024) and
    (.bootstrap.kademlia_protocol | type == "string" and test("^/[^[:space:]]+$") and length <= 128) and
    (.peer.peer_id | type == "string" and test("^[A-Za-z0-9]+$") and length <= 256) and
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
    (if $path_mode == "relay-only" then
      (.relay.android_reservation | type == "string" and
        test("^/[^[:space:]]+/p2p-circuit$") and length <= 1024)
    else
      (.relay // null) == null
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
  if [[ "$path_mode" == relay-only ]]; then
    fixture_relay_reservation="$(jq -r '.relay.android_reservation' "$fixture_metadata")"
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
if [[ "$("${adb[@]}" get-state)" != device ]]; then
  outcome=failed
  outcome_detail="ADB did not report the emulator as a device"
  record_step adb failed "$outcome_detail"
  exit 1
fi

if [[ "$path_mode" == owned-quic ]]; then
  if ! "${adb[@]}" emu redir add \
    "udp:$fixture_owned_quic_host_port:$fixture_owned_quic_guest_port" >/dev/null; then
    outcome=failed
    outcome_detail="Emulator could not install the owned-QUIC UDP redirection"
    record_step owned_quic_redirection failed "$outcome_detail"
    exit 1
  fi
  record_step owned_quic_redirection passed \
    "Linux and Android owned-QUIC listeners are mutually reachable"
fi

api_level="$("${adb[@]}" shell getprop ro.build.version.sdk | tr -d '\r')"
device_abi="$("${adb[@]}" shell getprop ro.product.cpu.abi | tr -d '\r')"
package_path="$("${adb[@]}" shell pm path org.hermeticfoundation.p2pvpn.debug | tr -d '\r')"
activity_state="$("${adb[@]}" shell dumpsys activity activities)"

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

if [[ "$scenario" == pairing-traffic ]]; then
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
  if [[ "$path_mode" == relay-only ]]; then
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
  record_step profile_creation passed \
    "Encrypted profile configured with discovery bootstrap only"

  if ! "${adb[@]}" shell appops set \
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
    "Android enrolled from a code without a configured overlay peer address"

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

  linux_ipv4_probe="$state_dir/linux-ipv4-probe.json"
  linux_ipv6_probe="$state_dir/linux-ipv6-probe.json"
  if ! "$fixture_command" probe \
    --socket "$fixture_packet_socket" \
    --source "$fixture_ipv4" \
    --destination "$android_ipv4" \
    --count 5 > "$linux_ipv4_probe" \
    || ! jq -e '.schema_version == 1 and .ok and .family == "ipv4" and .sent == 5 and .received == 5' \
      "$linux_ipv4_probe" >/dev/null; then
    outcome=failed
    outcome_detail="Linux-to-Android IPv4 overlay probe failed"
    record_step linux_to_android_ipv4 failed "$outcome_detail"
    exit 1
  fi
  record_step linux_to_android_ipv4 passed "Linux received 5 of 5 IPv4 replies"
  if ! "$fixture_command" probe \
    --socket "$fixture_packet_socket" \
    --source "$fixture_ipv6" \
    --destination "$android_ipv6" \
    --count 5 > "$linux_ipv6_probe" \
    || ! jq -e '.schema_version == 1 and .ok and .family == "ipv6" and .sent == 5 and .received == 5' \
      "$linux_ipv6_probe" >/dev/null; then
    outcome=failed
    outcome_detail="Linux-to-Android IPv6 overlay probe failed"
    record_step linux_to_android_ipv6 failed "$outcome_detail"
    exit 1
  fi
  record_step linux_to_android_ipv6 passed "Linux received 5 of 5 IPv6 replies"

  android_ipv4_ping="$state_dir/android-ipv4-ping.txt"
  android_ipv6_ping="$state_dir/android-ipv6-ping.txt"
  android_ipv4_ping_status=0
  "${adb[@]}" shell ping -c 5 -W 5 "$fixture_ipv4" \
    > "$android_ipv4_ping" 2>&1 || android_ipv4_ping_status=$?
  if [[ "$android_ipv4_ping_status" -ne 0 ]]; then
    outcome=failed
    outcome_detail="Android-to-Linux IPv4 ping exited with status $android_ipv4_ping_status"
    record_step android_to_linux_ipv4 failed "$outcome_detail"
    exit 1
  fi
  if ! grep -Eq '5 packets transmitted, 5 (packets )?received' "$android_ipv4_ping"; then
    outcome=failed
    outcome_detail="Android-to-Linux IPv4 ping did not report 5 replies"
    record_step android_to_linux_ipv4 failed "$outcome_detail"
    exit 1
  fi
  record_step android_to_linux_ipv4 passed "Android received 5 of 5 IPv4 replies"
  android_ipv6_ping_status=0
  "${adb[@]}" shell ping6 -c 5 -W 5 "$fixture_ipv6" \
    > "$android_ipv6_ping" 2>&1 || android_ipv6_ping_status=$?
  if [[ "$android_ipv6_ping_status" -ne 0 ]]; then
    outcome=failed
    outcome_detail="Android-to-Linux IPv6 ping exited with status $android_ipv6_ping_status"
    record_step android_to_linux_ipv6 failed "$outcome_detail"
    exit 1
  fi
  if ! grep -Eq '5 packets transmitted, 5 (packets )?received' "$android_ipv6_ping"; then
    outcome=failed
    outcome_detail="Android-to-Linux IPv6 ping did not report 5 replies"
    record_step android_to_linux_ipv6 failed "$outcome_detail"
    exit 1
  fi
  record_step android_to_linux_ipv6 passed "Android received 5 of 5 IPv6 replies"

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
    --argjson relay_established_circuits "$relay_established_circuits" '
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
      } else {} end))
    }
  ' "$device_file" > "$device_file.updated"
  mv -f "$device_file.updated" "$device_file"

  outcome=passed
  outcome_detail="Code pairing, $path_mode path isolation, and dual-stack traffic passed"
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

"${adb[@]}" shell am force-stop org.hermeticfoundation.p2pvpn.debug
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
  if ! "${adb[@]}" install -r "$android_apk" >/dev/null; then
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
