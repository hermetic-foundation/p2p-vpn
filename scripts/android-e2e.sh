#!/usr/bin/env bash
# shellcheck disable=SC2329
set -euo pipefail

umask 077

readonly evidence_schema_version=1
readonly maximum_log_bytes=$((1024 * 1024))
readonly default_minimum_free_bytes=$((16 * 1024 * 1024 * 1024))

scenario=boot-smoke
preflight_only=0
allow_skip=0
output_dir="${P2P_VPN_ANDROID_E2E_DIR:-}"
minimum_free_bytes="${P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES:-$default_minimum_free_bytes}"
started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
outcome=running
outcome_detail="E2E harness exited before recording a result"
evidence_finalized=0
emulator_pid=""
emulator_serial=""
state_dir=""
cleanup_emulator_stopped=false
cleanup_private_state_removed=false

usage() {
  cat <<'EOF'
Usage: p2p-vpn-android-e2e [OPTIONS]

Options:
  --scenario NAME        Select boot-smoke or profile-persistence.
  --preflight            Check requirements without starting an emulator.
  --allow-skip           Exit 77 instead of 2 when requirements are unavailable.
  --output DIRECTORY     Write bounded evidence to DIRECTORY.
  -h, --help             Show this help.

Environment:
  P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES
                         Required runtime free space; defaults to 16 GiB.

Exit codes:
  0   Scenario passed.
  2   Usage error or a required host capability is unavailable.
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
  boot-smoke|profile-persistence) ;;
  *)
    echo "unsupported Android E2E scenario: $scenario" >&2
    exit 2
    ;;
esac

if [[ ! "$minimum_free_bytes" =~ ^[0-9]{1,18}$ ]]; then
  echo "P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES must be an integer from 0 to 999999999999999999" >&2
  exit 2
fi
minimum_free_bytes=$((10#$minimum_free_bytes))
readonly minimum_free_bytes

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

: > "$checks_file"
: > "$steps_file"
: > "$emulator_log"
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
  bound_file "$emulator_log"
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
        private_state_removed: $private_state_removed
      },
      artifacts: {emulator_log: "emulator.log"}
    }' > "$evidence_file"
  rm -f "$checks_file" "$steps_file" "$device_file"
}

exit_handler() {
  local status="$1"
  trap - EXIT INT TERM
  set +e
  if [[ "$outcome" == running ]]; then
    outcome=failed
    outcome_detail="E2E harness terminated unexpectedly"
  fi
  stop_emulator
  remove_private_state
  finalize_evidence
  printf 'Android E2E evidence: %s\n' "$evidence_file" >&2
  exit "$status"
}

trap 'exit_handler $?' EXIT
trap 'outcome=failed; outcome_detail="E2E harness interrupted"; exit 130' INT
trap 'outcome=failed; outcome_detail="E2E harness terminated"; exit 143' TERM

missing_requirements=()
test_mode="${P2P_VPN_ANDROID_E2E_TEST_MODE:-0}"
emulator_command="${P2P_VPN_ANDROID_EMULATOR:-}"
adb_command="${P2P_VPN_ADB:-adb}"
android_apk="${P2P_VPN_ANDROID_APK:-}"

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

available_bytes="$({
  df --output=avail -B1 "${TMPDIR:-/tmp}" 2>/dev/null | tail -n 1 | tr -d '[:space:]'
} || true)"
if [[ "$available_bytes" =~ ^[0-9]{1,18}$ ]]; then
  available_bytes=$((10#$available_bytes))
  if (( available_bytes >= minimum_free_bytes )); then
    record_check disk_space true true \
      "$available_bytes bytes available; $minimum_free_bytes required"
  else
    record_check disk_space true false \
      "$available_bytes bytes available; $minimum_free_bytes required"
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

if [[ "$scenario" == profile-persistence ]]; then
  if [[ -n "$android_apk" && -f "$android_apk" ]]; then
    record_check android_apk true true "Reproducible debug APK is available"
  else
    record_check android_apk true false "P2P_VPN_ANDROID_APK is unavailable"
    missing_requirements+=(android_apk)
  fi
else
  record_check android_apk false false "boot-smoke does not reinstall the APK"
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
