#!/usr/bin/env bash
# shellcheck disable=SC2329
set -euo pipefail

umask 077

readonly schema_version=1
readonly package_name=org.hermeticfoundation.p2pvpn.debug
readonly activity_name=org.hermeticfoundation.p2pvpn.MainActivity
readonly receiver_name=org.hermeticfoundation.p2pvpn.DebugAutomationReceiver
readonly default_duration_seconds=1800
readonly minimum_proof_duration_seconds=1800
readonly default_sample_seconds=60
readonly default_doze_seconds=300
readonly minimum_proof_doze_seconds=300
readonly default_transition_timeout_seconds=180
readonly default_adb_timeout_seconds=60
readonly cleanup_adb_timeout_seconds=5
readonly required_transition_confirmations=3
readonly default_minimum_free_bytes=$((4 * 1024 * 1024 * 1024))
readonly maximum_evidence_bytes=$((2 * 1024 * 1024))

serial=""
network=""
peer_ipv4=""
peer_ipv6=""
output_dir=""
apk="${P2P_VPN_ANDROID_APK:-}"
duration_seconds="$default_duration_seconds"
sample_seconds="$default_sample_seconds"
doze_seconds="$default_doze_seconds"
transition_timeout_seconds="$default_transition_timeout_seconds"
adb_timeout_seconds="${P2P_VPN_ANDROID_DEVICE_AUDIT_ADB_TIMEOUT_SECONDS:-$default_adb_timeout_seconds}"
minimum_free_bytes="${P2P_VPN_ANDROID_DEVICE_AUDIT_MIN_FREE_BYTES:-$default_minimum_free_bytes}"
maximum_loss_percent=1
preflight_only=0
pair_fresh_profile=0
allow_short=0
auto_confirm="${P2P_VPN_ANDROID_DEVICE_AUDIT_AUTO_CONFIRM:-0}"
automatic_confirmation=false
interactive_confirmation=true

adb_command="${P2P_VPN_ADB:-adb}"
host_ping="${P2P_VPN_ANDROID_DEVICE_AUDIT_PING:-ping}"
adb=()
state_dir=""
steps_file=""
samples_file=""
outcome=running
outcome_detail="Physical audit exited before recording a result"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
finished_at=""
device_api=0
device_abi=""
proof_eligible=false
pairing_proven=false
installed_during_run=false
doze_forced=false
cleanup_doze_released=false
cleanup_screen_awake=false
cleanup_private_state_removed=false
baseline_identity=""
baseline_pid=""
android_ipv4=""
android_ipv6=""
battery_start_json=null
battery_end_json=null
diagnostics_start_json=null
diagnostics_end_json=null
sustained_summary_json=null
final_status_json=null
evidence_path=""
finalizing=0
transition_confirmation_count=0

usage() {
  cat <<'EOF'
Usage: p2p-vpn-android-device-audit [OPTIONS]

Required for a full audit:
  --network NAME           Expected Android overlay network.
  --peer-ipv4 ADDRESS      Reachable Linux overlay IPv4 address.
  --peer-ipv6 ADDRESS      Reachable Linux overlay IPv6 address.
  --output DIRECTORY       Destination for bounded evidence.json.

Options:
  --serial SERIAL          Authorized ADB device serial.
  --apk PATH               Debug APK used for the replacement update.
  --pair                   Create a profile and join using a code read from the TTY.
  --duration-seconds N     Sustained-run duration; default and proof minimum: 1800.
  --sample-seconds N       Sustained traffic interval; default: 60.
  --doze-seconds N         Forced-Doze hold; default and proof minimum: 300.
  --transition-timeout N   Recovery deadline per transition; default: 180.
  --max-loss-percent N     Sustained packet-loss ceiling; default: 1.
  --allow-short            Permit a smoke run that is marked proof-ineligible.
  --preflight              Validate the host and device without changing either.
  -h, --help               Show this help.

The audit never uses adb forward/reverse, clears app data, uninstalls the app,
or removes the saved profile. ADB is only the management channel.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --serial)
      [[ $# -ge 2 ]] || { echo "--serial requires a value" >&2; exit 2; }
      serial="$2"
      shift 2
      ;;
    --network)
      [[ $# -ge 2 ]] || { echo "--network requires a value" >&2; exit 2; }
      network="$2"
      shift 2
      ;;
    --peer-ipv4)
      [[ $# -ge 2 ]] || { echo "--peer-ipv4 requires a value" >&2; exit 2; }
      peer_ipv4="$2"
      shift 2
      ;;
    --peer-ipv6)
      [[ $# -ge 2 ]] || { echo "--peer-ipv6 requires a value" >&2; exit 2; }
      peer_ipv6="$2"
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || { echo "--output requires a value" >&2; exit 2; }
      output_dir="$2"
      shift 2
      ;;
    --apk)
      [[ $# -ge 2 ]] || { echo "--apk requires a value" >&2; exit 2; }
      apk="$2"
      shift 2
      ;;
    --pair)
      pair_fresh_profile=1
      shift
      ;;
    --duration-seconds)
      [[ $# -ge 2 ]] || { echo "--duration-seconds requires a value" >&2; exit 2; }
      duration_seconds="$2"
      shift 2
      ;;
    --sample-seconds)
      [[ $# -ge 2 ]] || { echo "--sample-seconds requires a value" >&2; exit 2; }
      sample_seconds="$2"
      shift 2
      ;;
    --doze-seconds)
      [[ $# -ge 2 ]] || { echo "--doze-seconds requires a value" >&2; exit 2; }
      doze_seconds="$2"
      shift 2
      ;;
    --transition-timeout)
      [[ $# -ge 2 ]] || { echo "--transition-timeout requires a value" >&2; exit 2; }
      transition_timeout_seconds="$2"
      shift 2
      ;;
    --max-loss-percent)
      [[ $# -ge 2 ]] || { echo "--max-loss-percent requires a value" >&2; exit 2; }
      maximum_loss_percent="$2"
      shift 2
      ;;
    --allow-short)
      allow_short=1
      shift
      ;;
    --preflight)
      preflight_only=1
      shift
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

unsigned_integer() {
  [[ "$1" =~ ^[0-9]{1,18}$ ]]
}

safe_network_name() {
  [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]]
}

valid_ipv4_literal() {
  local address="$1"
  local octet
  local -a octets
  [[ "$address" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || return 1
  IFS=. read -r -a octets <<< "$address"
  for octet in "${octets[@]}"; do
    ((10#$octet <= 255)) || return 1
  done
}

safe_ipv6_literal() {
  [[ ${#1} -le 45 && "$1" == *:* && "$1" =~ ^[0-9A-Fa-f:]+$ ]]
}

for numeric in \
  "$duration_seconds" \
  "$sample_seconds" \
  "$doze_seconds" \
  "$transition_timeout_seconds" \
  "$adb_timeout_seconds" \
  "$minimum_free_bytes" \
  "$maximum_loss_percent"; do
  if ! unsigned_integer "$numeric"; then
    echo "numeric options must be unsigned integers" >&2
    exit 2
  fi
done

duration_seconds=$((10#$duration_seconds))
sample_seconds=$((10#$sample_seconds))
doze_seconds=$((10#$doze_seconds))
transition_timeout_seconds=$((10#$transition_timeout_seconds))
adb_timeout_seconds=$((10#$adb_timeout_seconds))
minimum_free_bytes=$((10#$minimum_free_bytes))
maximum_loss_percent=$((10#$maximum_loss_percent))

if [[ "$auto_confirm" != 0 && "$auto_confirm" != 1 ]]; then
  echo "P2P_VPN_ANDROID_DEVICE_AUDIT_AUTO_CONFIRM must be 0 or 1" >&2
  exit 2
fi
if ((auto_confirm == 1)); then
  automatic_confirmation=true
  interactive_confirmation=false
fi
if ((auto_confirm == 1 && allow_short == 0 && preflight_only == 0)); then
  echo "automatic physical-audit confirmations require --allow-short" >&2
  exit 2
fi

if ((duration_seconds < 1 || duration_seconds > 43200)); then
  echo "--duration-seconds must be between 1 and 43200" >&2
  exit 2
fi
if ((sample_seconds < 1 || sample_seconds > 600)); then
  echo "--sample-seconds must be between 1 and 600" >&2
  exit 2
fi
maximum_samples=$(((duration_seconds + sample_seconds - 1) / sample_seconds))
if ((maximum_samples > 1440)); then
  echo "sustained sampling must produce at most 1440 evidence records" >&2
  exit 2
fi
if ((doze_seconds < 1 || doze_seconds > 3600)); then
  echo "--doze-seconds must be between 1 and 3600" >&2
  exit 2
fi
if ((transition_timeout_seconds < 10 || transition_timeout_seconds > 1800)); then
  echo "--transition-timeout must be between 10 and 1800" >&2
  exit 2
fi
if ((adb_timeout_seconds < 1 || adb_timeout_seconds > 300)); then
  echo "ADB timeout must be between 1 and 300 seconds" >&2
  exit 2
fi
if ((maximum_loss_percent > 100)); then
  echo "--max-loss-percent must be between 0 and 100" >&2
  exit 2
fi
if ((allow_short == 0)) \
  && ((duration_seconds < minimum_proof_duration_seconds \
    || doze_seconds < minimum_proof_doze_seconds)); then
  echo "shortened duration or Doze checks require --allow-short" >&2
  exit 2
fi

for tool in "$adb_command" "$host_ping" base64 date df jq sed sha256sum timeout; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "required command is unavailable: $tool" >&2
    exit 2
  fi
done
if [[ -z "$apk" || ! -s "$apk" ]]; then
  echo "a non-empty debug APK is required through --apk or P2P_VPN_ANDROID_APK" >&2
  exit 2
fi

mapfile -t attached_devices < <(
  timeout --signal=TERM --kill-after=2s "${adb_timeout_seconds}s" \
    "$adb_command" devices \
    | awk 'NR > 1 && $2 == "device" { print $1 }'
)
if [[ -z "$serial" ]]; then
  if ((${#attached_devices[@]} != 1)); then
    echo "exactly one authorized ADB device is required, or pass --serial" >&2
    exit 2
  fi
  serial="${attached_devices[0]}"
elif ! printf '%s\n' "${attached_devices[@]}" | grep -Fxq -- "$serial"; then
  echo "the selected ADB serial is not authorized and online" >&2
  exit 2
fi
adb=("$adb_command" -s "$serial")

adb_run() {
  timeout --signal=TERM --kill-after=2s "${adb_timeout_seconds}s" "${adb[@]}" "$@"
}

start_app() {
  adb_run shell am start -W -n "$package_name/$activity_name" >/dev/null
}

device_abi="$(adb_run shell getprop ro.product.cpu.abilist | tr -d '\r')"
device_api="$(adb_run shell getprop ro.build.version.sdk | tr -d '\r')"
if [[ ",$device_abi," != *,arm64-v8a,* ]]; then
  echo "the selected device does not advertise arm64-v8a" >&2
  exit 2
fi
if ! unsigned_integer "$device_api" || ((10#$device_api < 26)); then
  echo "the selected device must run Android API 26 or newer" >&2
  exit 2
fi
device_api=$((10#$device_api))

if [[ "$preflight_only" -eq 1 ]]; then
  printf 'Android physical audit preflight passed: ABI arm64-v8a, API %s\n' "$device_api"
  exit 0
fi

if ! safe_network_name "$network"; then
  echo "--network must be a 1-128 character ASCII name using letters, digits, dot, dash, or underscore" >&2
  exit 2
fi
if ! valid_ipv4_literal "$peer_ipv4" || ! safe_ipv6_literal "$peer_ipv6"; then
  echo "--peer-ipv4 and --peer-ipv6 must be safe numeric address literals" >&2
  exit 2
fi
if [[ -z "$output_dir" ]]; then
  echo "--output is required for a full physical audit" >&2
  exit 2
fi

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd -P)"
evidence_path="$output_dir/evidence.json"
if [[ -e "$evidence_path" ]]; then
  echo "refusing to replace existing evidence: $evidence_path" >&2
  exit 2
fi
tmp_available="$(df -B1 --output=avail "${TMPDIR:-/tmp}" | awk 'NR == 2 { print $1 }')"
output_available="$(df -B1 --output=avail "$output_dir" | awk 'NR == 2 { print $1 }')"
if ! unsigned_integer "$tmp_available" || ! unsigned_integer "$output_available"; then
  echo "could not determine free space for physical audit state" >&2
  exit 2
fi
if ((tmp_available < minimum_free_bytes || output_available < minimum_free_bytes)); then
  echo "physical audit free space is below the configured reserve" >&2
  exit 75
fi

state_dir="$(mktemp -d -t p2p-vpn-android-device-audit.XXXXXXXX)"
steps_file="$state_dir/steps.ndjson"
samples_file="$state_dir/samples.ndjson"
: > "$steps_file"
: > "$samples_file"

record_step() {
  local name="$1"
  local state="$2"
  local detail="$3"
  local data="${4:-null}"
  jq -nc \
    --arg name "$name" \
    --arg state "$state" \
    --arg detail "$detail" \
    --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson data "$data" \
    '{name: $name, state: $state, detail: $detail, at: $at, data: $data}' \
    >> "$steps_file"
}

fail_audit() {
  outcome=failed
  outcome_detail="$1"
  echo "$outcome_detail" >&2
  exit 1
}

android_automation() {
  local command="$1"
  shift
  local broadcast encoded
  broadcast="$(
    adb_run shell am broadcast \
      --receiver-foreground \
      -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION \
      -n "$package_name/$receiver_name" \
      --es command "$command" \
      "$@"
  )" || return 1
  encoded="$(
    sed -nE \
      's/^Broadcast completed: result=[^,]+, data="([A-Za-z0-9+\/=]+)".*$/\1/p' \
      <<< "$broadcast"
  )"
  [[ -n "$encoded" ]] || return 1
  printf '%s' "$encoded" | base64 --decode
}

get_status() {
  local status
  status="$(android_automation status)" || return 1
  jq -ce 'select(
    .schema_version == 1 and .ok and .value.service_ready and
    (.value.snapshot | type) == "object"
  )' \
    <<< "$status"
}

get_diagnostics() {
  local response
  response="$(android_automation diagnostics)" || return 1
  jq -ce '
    select(
      .schema_version == 1 and .ok and .value.service_ready and
      (.value.report.schema_version == 1) and
      (.value.report.kind == "p2p-vpn-android-diagnostics")
    ) | .value.report
  ' <<< "$response"
}

wait_for_service_status() {
  local deadline=$((SECONDS + 30))
  local status
  while ((SECONDS <= deadline)); do
    if status="$(get_status)"; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 1
  done
  return 1
}

wait_for_profile_status() {
  local deadline=$((SECONDS + 60))
  local status
  while ((SECONDS <= deadline)); do
    if status="$(get_status)" \
      && jq -e --arg network "$network" '
        .value.snapshot.has_profile and
        (.value.snapshot.profile_unreadable | not) and
        .value.snapshot.network_name == $network and
        (.value.snapshot.hostname | type == "string" and test("^android-[0-9a-f]{16}$"))
      ' <<< "$status" >/dev/null; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 1
  done
  return 1
}

sanitize_status() {
  jq -c --arg network "$network" '
    .value.snapshot | {
      connected,
      connection_requested,
      busy,
      profile_stored,
      profile_readable: (.has_profile and (.profile_unreadable | not)),
      network_matches: (.network_name == $network),
      hostname_assigned: (.hostname | type == "string" and test("^android-[0-9a-f]{16}$")),
      address_families: ([.addresses[]? | if contains(":") then "ipv6" else "ipv4" end] | unique),
      runtime_generation,
      underlay,
      paths
    }
  '
}

wait_for_connected_status() {
  local deadline=$((SECONDS + transition_timeout_seconds))
  local status
  while ((SECONDS <= deadline)); do
    if status="$(get_status)" \
      && jq -e --arg network "$network" '
        .value.snapshot.connected and
        .value.snapshot.has_profile and
        (.value.snapshot.profile_unreadable | not) and
        .value.snapshot.network_name == $network and
        (.value.snapshot.addresses | length >= 2)
      ' <<< "$status" >/dev/null; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 2
  done
  return 1
}

ping_received_count() {
  local output="$1"
  local match
  match="$(grep -Eo '[0-9]+ (packets )?received' <<< "$output" | tail -n 1 || true)"
  if [[ "$match" =~ ^([0-9]+) ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
  else
    printf '0\n'
  fi
}

probe_matrix() {
  local count="$1"
  local linux_v4 linux_v6 android_v4 android_v6
  local linux_v4_received linux_v6_received android_v4_received android_v6_received
  linux_v4="$("$host_ping" -4 -c "$count" -W 5 "$android_ipv4" 2>&1 || true)"
  linux_v6="$("$host_ping" -6 -c "$count" -W 5 "$android_ipv6" 2>&1 || true)"
  android_v4="$(adb_run shell ping -c "$count" -W 5 "$peer_ipv4" 2>&1 || true)"
  android_v6="$(adb_run shell ping6 -c "$count" -W 5 "$peer_ipv6" 2>&1 || true)"
  linux_v4_received="$(ping_received_count "$linux_v4")"
  linux_v6_received="$(ping_received_count "$linux_v6")"
  android_v4_received="$(ping_received_count "$android_v4")"
  android_v6_received="$(ping_received_count "$android_v6")"
  jq -nc \
    --argjson sent "$count" \
    --argjson linux_v4 "$linux_v4_received" \
    --argjson linux_v6 "$linux_v6_received" \
    --argjson android_v4 "$android_v4_received" \
    --argjson android_v6 "$android_v6_received" '
    {
      ok: ($linux_v4 == $sent and $linux_v6 == $sent and
        $android_v4 == $sent and $android_v6 == $sent),
      sent: ($sent * 4),
      received: ($linux_v4 + $linux_v6 + $android_v4 + $android_v6),
      directions: {
        linux_to_android_ipv4: {sent: $sent, received: $linux_v4},
        linux_to_android_ipv6: {sent: $sent, received: $linux_v6},
        android_to_linux_ipv4: {sent: $sent, received: $android_v4},
        android_to_linux_ipv6: {sent: $sent, received: $android_v6}
      }
    }
  '
}

wait_for_traffic() {
  local started_millis
  started_millis="$(date +%s%3N)"
  local deadline=$((SECONDS + transition_timeout_seconds))
  local readiness strict convergence
  while ((SECONDS <= deadline)); do
    readiness="$(probe_matrix 1)"
    if jq -e '.ok' <<< "$readiness" >/dev/null; then
      strict="$(probe_matrix 5)"
      if jq -e '.ok' <<< "$strict" >/dev/null; then
        convergence=$(($(date +%s%3N) - started_millis))
        jq -c --argjson convergence "$convergence" \
          '. + {convergence_millis: $convergence}' <<< "$strict"
        return 0
      fi
    fi
    sleep 2
  done
  return 1
}

prompt_operator() {
  local prompt="$1"
  local confirmation_kind="${2:-other}"
  if [[ "$auto_confirm" == 1 ]]; then
    printf 'AUTO: %s\n' "$prompt" >&2
    return 0
  fi
  if [[ ! -r /dev/tty ]]; then
    fail_audit "an interactive TTY is required for physical network checkpoints"
  fi
  printf '\n%s\n' "$prompt" >/dev/tty
  read -r -p "Press Enter when the transition is complete: " _ </dev/tty
  if [[ "$confirmation_kind" == transition ]]; then
    ((transition_confirmation_count += 1))
  fi
}

wait_for_device() {
  local deadline=$((SECONDS + transition_timeout_seconds))
  while ((SECONDS <= deadline)); do
    if adb_run get-state 2>/dev/null | grep -Fxq device; then
      return 0
    fi
    sleep 2
  done
  return 1
}

storage_budget_ok() {
  local current_tmp current_output
  current_tmp="$(df -B1 --output=avail "${TMPDIR:-/tmp}" | awk 'NR == 2 { print $1 }')"
  current_output="$(df -B1 --output=avail "$output_dir" | awk 'NR == 2 { print $1 }')"
  unsigned_integer "$current_tmp" \
    && unsigned_integer "$current_output" \
    && ((current_tmp >= minimum_free_bytes && current_output >= minimum_free_bytes))
}

battery_snapshot() {
  local raw level scale status plugged temperature charge_counter
  raw="$(adb_run shell dumpsys battery | tr -d '\r')" || return 1
  level="$(sed -nE 's/^[[:space:]]*level: ([0-9-]+).*$/\1/p' <<< "$raw" | head -n 1)"
  scale="$(sed -nE 's/^[[:space:]]*scale: ([0-9-]+).*$/\1/p' <<< "$raw" | head -n 1)"
  status="$(sed -nE 's/^[[:space:]]*status: ([0-9-]+).*$/\1/p' <<< "$raw" | head -n 1)"
  plugged="$(sed -nE 's/^[[:space:]]*plugged: ([0-9-]+).*$/\1/p' <<< "$raw" | head -n 1)"
  temperature="$(sed -nE 's/^[[:space:]]*temperature: ([0-9-]+).*$/\1/p' <<< "$raw" | head -n 1)"
  charge_counter="$(sed -nE 's/^[[:space:]]*[Cc]harge counter: ([0-9-]+).*$/\1/p' <<< "$raw" | head -n 1)"
  jq -nc \
    --arg level "$level" \
    --arg scale "$scale" \
    --arg status "$status" \
    --arg plugged "$plugged" \
    --arg temperature "$temperature" \
    --arg charge_counter "$charge_counter" '
    def number_or_null: if test("^-?[0-9]+$") then tonumber else null end;
    {
      level: ($level | number_or_null),
      scale: ($scale | number_or_null),
      status: ($status | number_or_null),
      plugged: ($plugged | number_or_null),
      temperature_tenths_c: ($temperature | number_or_null),
      charge_counter_microamp_hours: ($charge_counter | number_or_null)
    }
  '
}

extract_profile_state() {
  local status="$1"
  baseline_identity="$(jq -r '.value.snapshot.peer_id // empty' <<< "$status")"
  android_ipv4="$(jq -r '.value.snapshot.addresses[] | select(contains(":") | not)' <<< "$status" | head -n 1)"
  android_ipv6="$(jq -r '.value.snapshot.addresses[] | select(contains(":"))' <<< "$status" | head -n 1)"
  android_ipv4="${android_ipv4%%/*}"
  android_ipv6="${android_ipv6%%/*}"
  [[ -n "$baseline_identity" && -n "$android_ipv4" && -n "$android_ipv6" ]]
}

connect_with_permission() {
  local response
  response="$(android_automation connect)" || return 1
  if jq -e '.ok' <<< "$response" >/dev/null; then
    return 0
  fi
  if ! jq -e '.error == "vpn_permission_required"' <<< "$response" >/dev/null; then
    return 1
  fi
  start_app
  prompt_operator "In p2p-vpn, tap Connect and approve the Android VPN permission dialog."
  response="$(android_automation connect)" || return 1
  jq -e '.ok' <<< "$response" >/dev/null
}

pair_new_profile() {
  local status response pairing_code diagnostics deadline
  status="$(wait_for_service_status)" || return 1
  if jq -e '.value.snapshot.has_profile' <<< "$status" >/dev/null; then
    echo "--pair requires an app installation with no saved profile" >&2
    return 1
  fi
  response="$(android_automation create-profile --es network "$network")" || return 1
  jq -e '.ok' <<< "$response" >/dev/null || return 1
  status="$(wait_for_profile_status)" || return 1
  connect_with_permission || return 1
  status="$(wait_for_connected_status)" || return 1

  prompt_operator "On an existing Linux member, run: sudo p2p-vpn pair open --instance INSTANCE"
  if [[ "$auto_confirm" == 1 ]]; then
    pairing_code="TEST-PAIRING-CODE"
  else
    printf 'Enter the one-time pairing code (input is hidden): ' >/dev/tty
    read -r -s pairing_code </dev/tty
    printf '\n' >/dev/tty
  fi
  [[ "$pairing_code" =~ ^[A-Za-z0-9-]{16,32}$ ]] || return 1
  response="$(android_automation join-pairing --es code "$pairing_code")" || return 1
  pairing_code=""
  jq -e '.ok' <<< "$response" >/dev/null || return 1
  prompt_operator "Verify the Android candidate on Linux and approve that pairing operation."

  deadline=$((SECONDS + 600))
  while ((SECONDS <= deadline)); do
    diagnostics="$(get_diagnostics)" || true
    status="$(get_status)" || true
    if [[ -n "$diagnostics" && -n "$status" ]] \
      && jq -e 'any(.events.items[]?; .name == "pairing_completed")' \
        <<< "$diagnostics" >/dev/null \
      && jq -e '.value.snapshot.connected and (.value.snapshot.pairing.code == null)' \
        <<< "$status" >/dev/null; then
      pairing_proven=true
      record_step pairing passed \
        "Code pairing completed without a configured overlay peer address" \
        '{"hostname_assigned":true}'
      return 0
    fi
    sleep 2
  done
  return 1
}

run_transition_checkpoint() {
  local name="$1"
  local instruction="$2"
  local require_selection_change="$3"
  local before="$4"
  local before_generation before_selection after probe after_generation after_selection data
  before_generation="$(jq -r '.value.snapshot.runtime_generation' <<< "$before")"
  before_selection="$(jq -r '.value.snapshot.underlay.selection_changes' <<< "$before")"
  prompt_operator "$instruction" transition
  wait_for_device || fail_audit "$name: ADB management path did not return"
  after="$(wait_for_connected_status)" || fail_audit "$name: Android runtime did not reconnect"
  probe="$(wait_for_traffic)" || fail_audit "$name: bidirectional dual-stack traffic did not recover"
  after_generation="$(jq -r '.value.snapshot.runtime_generation' <<< "$after")"
  after_selection="$(jq -r '.value.snapshot.underlay.selection_changes' <<< "$after")"
  if [[ "$after_generation" != "$before_generation" ]]; then
    fail_audit "$name: native runtime restarted during an underlay transition"
  fi
  if [[ "$require_selection_change" == 1 ]] && ((after_selection <= before_selection)); then
    fail_audit "$name: Android did not record a physical underlay selection change"
  fi
  data="$(jq -nc \
    --argjson status "$(sanitize_status <<< "$after")" \
    --argjson traffic "$probe" \
    --argjson operator_confirmed "$interactive_confirmation" \
    '{status: $status, traffic: $traffic, runtime_generation_preserved: true,
      operator_confirmed: $operator_confirmed}')"
  record_step "$name" passed \
    "Traffic recovered automatically without a native runtime restart" "$data"
  printf '%s\n' "$after"
}

run_doze_checkpoint() {
  local before="$1"
  local before_generation doze_state after probe data
  before_generation="$(jq -r '.value.snapshot.runtime_generation' <<< "$before")"
  adb_run shell input keyevent 223 >/dev/null
  doze_forced=true
  if ! adb_run shell dumpsys deviceidle force-idle >/dev/null; then
    fail_audit "Android could not enter forced Doze"
  fi
  sleep "$doze_seconds"
  doze_state="$(adb_run shell dumpsys deviceidle get deep | tr -d '\r[:space:]')" \
    || fail_audit "Android could not report its deep-idle state"
  if [[ "$doze_state" != IDLE ]]; then
    fail_audit "Android left forced Doze before the hold completed"
  fi
  after="$(wait_for_connected_status)" || fail_audit "runtime was unavailable during Doze"
  probe="$(wait_for_traffic)" || fail_audit "dual-stack traffic did not pass during Doze"
  if [[ "$(jq -r '.value.snapshot.runtime_generation' <<< "$after")" != "$before_generation" ]]; then
    fail_audit "native runtime restarted during screen-off/Doze"
  fi
  adb_run shell dumpsys deviceidle unforce >/dev/null
  doze_forced=false
  cleanup_doze_released=true
  adb_run shell input keyevent 224 >/dev/null
  cleanup_screen_awake=true
  data="$(jq -nc \
    --argjson seconds "$doze_seconds" \
    --argjson status "$(sanitize_status <<< "$after")" \
    --argjson traffic "$probe" \
    '{hold_seconds: $seconds, deep_idle_observed: true, status: $status,
      traffic: $traffic, runtime_generation_preserved: true}')"
  record_step screen_off_doze passed \
    "Foreground VPN traffic and runtime survived forced Doze" "$data"
  printf '%s\n' "$after"
}

run_sustained_checkpoint() {
  local before="$1"
  local before_pid before_generation before_identity started_millis deadline now elapsed
  local matrix status sanitized totals sent received lost loss_basis_points
  before_pid="$(adb_run shell pidof "$package_name" | tr -d '[:space:]')"
  before_generation="$(jq -r '.value.snapshot.runtime_generation' <<< "$before")"
  before_identity="$(jq -r '.value.snapshot.peer_id' <<< "$before")"
  battery_start_json="$(battery_snapshot)" || battery_start_json=null
  diagnostics_start_json="$(get_diagnostics)" || diagnostics_start_json=null
  started_millis="$(date +%s%3N)"
  deadline=$((SECONDS + duration_seconds))
  while ((SECONDS < deadline)); do
    matrix="$(probe_matrix 5)"
    status="$(get_status)" || status=""
    if [[ -n "$status" ]]; then
      sanitized="$(sanitize_status <<< "$status")"
    else
      sanitized=null
    fi
    now="$(date +%s%3N)"
    elapsed=$((now - started_millis))
    jq -nc \
      --argjson elapsed_millis "$elapsed" \
      --argjson traffic "$matrix" \
      --argjson status "$sanitized" \
      '{elapsed_millis: $elapsed_millis, traffic: $traffic, status: $status}' \
      >> "$samples_file"
    storage_budget_ok || fail_audit "physical audit crossed its free-space reserve"
    if ((SECONDS < deadline)); then
      sleep "$sample_seconds"
    fi
  done
  battery_end_json="$(battery_snapshot)" || battery_end_json=null
  diagnostics_end_json="$(get_diagnostics)" || diagnostics_end_json=null
  status="$(wait_for_connected_status)" || fail_audit "runtime was unavailable after sustained traffic"
  totals="$(jq -sc '
    {
      samples: length,
      sent: (map(.traffic.sent) | add // 0),
      received: (map(.traffic.received) | add // 0),
      failed_samples: (map(select(.traffic.ok | not)) | length)
    }
  ' "$samples_file")"
  sent="$(jq -r '.sent' <<< "$totals")"
  received="$(jq -r '.received' <<< "$totals")"
  ((sent > 0)) || fail_audit "sustained run produced no packets"
  lost=$((sent - received))
  loss_basis_points=$((lost * 10000 / sent))
  if ((loss_basis_points > maximum_loss_percent * 100)); then
    fail_audit "sustained packet loss exceeded the configured ceiling"
  fi
  if [[ "$(jq -r '.value.snapshot.peer_id' <<< "$status")" != "$before_identity" \
    || "$(jq -r '.value.snapshot.runtime_generation' <<< "$status")" != "$before_generation" \
    || "$(adb_run shell pidof "$package_name" | tr -d '[:space:]')" != "$before_pid" ]]; then
    fail_audit "identity, process, or runtime generation changed during sustained traffic"
  fi
  sustained_summary_json="$(jq -nc \
    --argjson totals "$totals" \
    --argjson duration "$duration_seconds" \
    --argjson loss_basis_points "$loss_basis_points" \
    --argjson battery_start "$battery_start_json" \
    --argjson battery_end "$battery_end_json" \
    --argjson diagnostics_start "$diagnostics_start_json" \
    --argjson diagnostics_end "$diagnostics_end_json" '
    def metric_delta($section; $name):
      if ($diagnostics_start | type) == "object" and ($diagnostics_end | type) == "object" then
        (($diagnostics_end[$section][$name] // 0) - ($diagnostics_start[$section][$name] // 0))
      else null end;
    $totals + {
      duration_seconds: $duration,
      packet_loss_basis_points: $loss_basis_points,
      process_cpu_millis_delta: metric_delta("resources"; "process_cpu_millis"),
      final_total_pss_kib: ($diagnostics_end.resources.total_pss_kib // null),
      final_private_dirty_kib: ($diagnostics_end.resources.private_dirty_kib // null),
      final_active_threads: ($diagnostics_end.resources.active_threads // null),
      battery: {
        start: $battery_start,
        end: $battery_end,
        level_delta: (if ($battery_start.level != null and $battery_end.level != null)
          then ($battery_end.level - $battery_start.level) else null end),
        charge_counter_delta_microamp_hours:
          (if ($battery_start.charge_counter_microamp_hours != null and
              $battery_end.charge_counter_microamp_hours != null)
           then ($battery_end.charge_counter_microamp_hours -
             $battery_start.charge_counter_microamp_hours) else null end),
        unplugged_measurement: (($battery_start.plugged // -1) == 0 and
          ($battery_end.plugged // -1) == 0)
      }
    }
  ')"
  record_step sustained_connection passed \
    "Sustained bidirectional traffic stayed within the packet-loss ceiling" \
    "$sustained_summary_json"
  printf '%s\n' "$status"
}

run_process_recreation_checkpoint() {
  local before="$1"
  local identity before_pid after after_pid probe data
  identity="$(jq -r '.value.snapshot.peer_id' <<< "$before")"
  before_pid="$(adb_run shell pidof "$package_name" | tr -d '[:space:]')"
  adb_run shell am force-stop "$package_name" >/dev/null
  sleep 2
  start_app
  connect_with_permission || fail_audit "service recreation could not reconnect"
  after="$(wait_for_connected_status)" || fail_audit "service recreation did not restore runtime"
  after_pid="$(adb_run shell pidof "$package_name" | tr -d '[:space:]')"
  if [[ -z "$before_pid" || -z "$after_pid" || "$before_pid" == "$after_pid" ]]; then
    fail_audit "Android process recreation was not observed"
  fi
  if [[ "$(jq -r '.value.snapshot.peer_id' <<< "$after")" != "$identity" ]]; then
    fail_audit "identity changed after Android process recreation"
  fi
  probe="$(wait_for_traffic)" || fail_audit "traffic did not recover after process recreation"
  data="$(jq -nc \
    --argjson status "$(sanitize_status <<< "$after")" \
    --argjson traffic "$probe" \
    '{process_recreated: true, identity_preserved: true, status: $status, traffic: $traffic}')"
  record_step process_service_recreation passed \
    "Encrypted profile and traffic survived process/service recreation" "$data"
  printf '%s\n' "$after"
}

run_update_checkpoint() {
  local before="$1"
  local identity after probe data
  identity="$(jq -r '.value.snapshot.peer_id' <<< "$before")"
  adb_run install -r "$apk" >/dev/null || fail_audit "ADB replacement install failed"
  start_app
  connect_with_permission || fail_audit "updated app could not reconnect"
  after="$(wait_for_connected_status)" || fail_audit "updated app did not restore runtime"
  if [[ "$(jq -r '.value.snapshot.peer_id' <<< "$after")" != "$identity" ]]; then
    fail_audit "identity changed after the in-place APK update"
  fi
  probe="$(wait_for_traffic)" || fail_audit "traffic did not recover after the APK update"
  data="$(jq -nc \
    --arg apk_sha256 "$(sha256sum "$apk" | awk '{ print $1 }')" \
    --argjson status "$(sanitize_status <<< "$after")" \
    --argjson traffic "$probe" \
    '{replacement_install: true, identity_preserved: true, apk_sha256: $apk_sha256,
      status: $status, traffic: $traffic}')"
  record_step in_place_apk_update passed \
    "Encrypted profile and traffic survived adb install -r" "$data"
  printf '%s\n' "$after"
}

finish_audit() {
  local exit_status=$?
  local provisional="$output_dir/.evidence.json.tmp"
  local final_tmp="$output_dir/.evidence.json.final"
  local evidence_size
  local evidence_rendered=true
  if ((finalizing == 1)); then
    exit "$exit_status"
  fi
  finalizing=1
  trap - EXIT INT TERM
  set +e

  if [[ "$doze_forced" == true ]]; then
    adb_timeout_seconds=$cleanup_adb_timeout_seconds
    if adb_run shell dumpsys deviceidle unforce >/dev/null 2>&1; then
      cleanup_doze_released=true
    fi
    doze_forced=false
  elif [[ "$cleanup_doze_released" != true ]]; then
    cleanup_doze_released=true
  fi
  adb_timeout_seconds=$cleanup_adb_timeout_seconds
  if adb_run shell input keyevent 224 >/dev/null 2>&1; then
    cleanup_screen_awake=true
  fi
  diagnostics_end_json="$(get_diagnostics 2>/dev/null)" || diagnostics_end_json=null
  status_for_evidence="$(get_status 2>/dev/null)" || status_for_evidence=""
  if [[ -n "$status_for_evidence" ]]; then
    final_status_json="$(sanitize_status <<< "$status_for_evidence")"
  fi

  if [[ "$outcome" == running ]]; then
    if ((exit_status == 0)); then
      outcome=passed
      outcome_detail="Physical arm64 audit passed"
    else
      outcome=failed
    fi
  fi
  finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "$outcome" == passed ]] && ((allow_short == 0 \
    && auto_confirm == 0 \
    && transition_confirmation_count >= required_transition_confirmations \
    && duration_seconds >= minimum_proof_duration_seconds \
    && doze_seconds >= minimum_proof_doze_seconds)); then
    proof_eligible=true
  fi

  if ! jq -n \
    --argjson schema_version "$schema_version" \
    --arg outcome "$outcome" \
    --arg detail "$outcome_detail" \
    --arg started_at "$started_at" \
    --arg finished_at "$finished_at" \
    --argjson api "$device_api" \
    --argjson proof_eligible "$proof_eligible" \
    --argjson pairing_proven "$pairing_proven" \
    --argjson installed_during_run "$installed_during_run" \
    --argjson duration "$duration_seconds" \
    --argjson sample_seconds "$sample_seconds" \
    --argjson doze_seconds "$doze_seconds" \
    --argjson maximum_loss_percent "$maximum_loss_percent" \
    --argjson automatic_confirmation "$automatic_confirmation" \
    --argjson transition_confirmations "$transition_confirmation_count" \
    --argjson required_confirmations "$required_transition_confirmations" \
    --argjson sustained "$sustained_summary_json" \
    --argjson final_status "$final_status_json" \
    --argjson final_diagnostics "$diagnostics_end_json" \
    --argjson doze_released "$cleanup_doze_released" \
    --argjson screen_awake "$cleanup_screen_awake" \
    --slurpfile steps "$steps_file" \
    --slurpfile samples "$samples_file" '
    {
      schema_version: $schema_version,
      kind: "p2p-vpn-android-physical-audit",
      outcome: $outcome,
      detail: $detail,
      started_at: $started_at,
      finished_at: $finished_at,
      contract: {
        arm64_required: true,
        minimum_api: 26,
        duration_seconds: $duration,
        sample_seconds: $sample_seconds,
        doze_seconds: $doze_seconds,
        maximum_loss_percent: $maximum_loss_percent,
        proof_eligible: $proof_eligible,
        operator: {
          automatic_confirmation: $automatic_confirmation,
          interactive_transition_confirmations: $transition_confirmations,
          required_transition_confirmations: $required_confirmations
        }
      },
      device: {abi: "arm64-v8a", android_api: $api, serial: "excluded", model: "excluded"},
      app: {package: "org.hermeticfoundation.p2pvpn.debug", installed_during_run: $installed_during_run},
      pairing: {performed_during_run: $pairing_proven},
      steps: $steps,
      sustained: $sustained,
      samples: $samples,
      final_status: $final_status,
      final_diagnostics: $final_diagnostics,
      management_path: {adb_only: true, adb_forward: false, adb_reverse: false},
      privacy: {
        device_serial: "excluded",
        device_model: "excluded",
        peer_ids: "excluded",
        overlay_addresses: "excluded",
        pairing_codes: "excluded",
        identity_material: "excluded",
        underlay_addresses: "excluded"
      },
      cleanup: {
        doze_released: $doze_released,
        screen_awake: $screen_awake,
        private_state_removed: false,
        profile_preserved: true
      }
    }
  ' > "$provisional"; then
    evidence_rendered=false
  fi

  if [[ -n "$state_dir" ]] && find "$state_dir" -depth -delete 2>/dev/null; then
    cleanup_private_state_removed=true
  fi
  if [[ "$evidence_rendered" != true ]]; then
    find "$provisional" "$final_tmp" -delete 2>/dev/null || true
    echo "failed to render physical audit evidence" >&2
    exit 1
  fi
  if ! jq --argjson removed "$cleanup_private_state_removed" \
    '.cleanup.private_state_removed = $removed' "$provisional" > "$final_tmp" \
    || ! mv "$final_tmp" "$evidence_path"; then
    find "$provisional" "$final_tmp" -delete 2>/dev/null || true
    echo "failed to finalize physical audit evidence" >&2
    exit 1
  fi
  find "$provisional" -delete 2>/dev/null || true
  evidence_size="$(wc -c < "$evidence_path")"
  if ((evidence_size > maximum_evidence_bytes)); then
    find "$evidence_path" -delete 2>/dev/null || true
    echo "physical audit evidence exceeded its 2 MiB limit" >&2
    exit_status=1
  fi
  printf 'Android physical audit evidence: %s\n' "$evidence_path"
  exit "$exit_status"
}

trap finish_audit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

adb_run install -r "$apk" >/dev/null || fail_audit "failed to install the debug APK"
installed_during_run=true
start_app

if [[ "$pair_fresh_profile" -eq 1 ]]; then
  pair_new_profile || fail_audit "fresh-profile code pairing did not complete"
else
  initial_status="$(wait_for_service_status)" \
    || fail_audit "the Android debug service did not become ready"
  if ! jq -e '.value.snapshot.has_profile and (.value.snapshot.profile_unreadable | not)' \
    <<< "$initial_status" >/dev/null; then
    fail_audit "no readable Android profile exists; pair first or use --pair"
  fi
  if [[ "$(jq -r '.value.snapshot.network_name' <<< "$initial_status")" != "$network" ]]; then
    fail_audit "the saved Android profile belongs to a different network"
  fi
  connect_with_permission || fail_audit "the existing Android profile could not connect"
fi

current_status="$(wait_for_connected_status)" \
  || fail_audit "the Android profile is not connected to the expected network"
extract_profile_state "$current_status" \
  || fail_audit "the Android profile lacks a stable dual-stack identity"
baseline_pid="$(adb_run shell pidof "$package_name" | tr -d '[:space:]')"
[[ -n "$baseline_pid" ]] || fail_audit "the Android app process is unavailable"
baseline_probe="$(wait_for_traffic)" \
  || fail_audit "baseline LAN bidirectional dual-stack traffic did not converge"
record_step lan_baseline passed \
  "Baseline LAN traffic passed 5/5 in every direction and address family" \
  "$(jq -nc \
    --argjson status "$(sanitize_status <<< "$current_status")" \
    --argjson traffic "$baseline_probe" \
    '{status: $status, traffic: $traffic}')"

checkpoint_status_file="$state_dir/checkpoint-status.json"
run_transition_checkpoint \
  hotspot_or_cellular \
  "Move the Android device from LAN to cellular or a separate hotspot. Do not add port forwarding." \
  1 \
  "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"
run_transition_checkpoint \
  hotspot_upstream_vpn \
  "Route that hotspot's upstream connection through a VPN. Do not start a second Android VPN app." \
  0 \
  "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"
run_transition_checkpoint \
  lan_return \
  "Disable the upstream VPN and return the Android device to the original LAN." \
  1 \
  "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"
run_doze_checkpoint "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"
run_sustained_checkpoint "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"
run_process_recreation_checkpoint "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"
run_update_checkpoint "$current_status" > "$checkpoint_status_file"
current_status="$(< "$checkpoint_status_file")"

final_identity="$(jq -r '.value.snapshot.peer_id' <<< "$current_status")"
if [[ "$final_identity" != "$baseline_identity" ]]; then
  fail_audit "identity changed across the physical audit"
fi
final_status_json="$(sanitize_status <<< "$current_status")"
outcome=passed
outcome_detail="Physical arm64 audit passed"
exit 0
