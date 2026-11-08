#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  printf 'Usage: bash scripts/android-power-capture.sh --serial SERIAL --output FILE [--rpc-timeout-seconds 1..30]\n'
}
fail() {
  printf 'Power capture failed: %s\n' "$1" >&2
  exit 1
}
serial=''
output=''
rpc_timeout=10
while (($# > 0)); do
  case "$1" in
    --serial | --output | --rpc-timeout-seconds)
      (($# >= 2)) || {
        usage >&2
        exit 2
      }
      case "$1" in
        --serial) serial=$2 ;;
        --output) output=$2 ;;
        --rpc-timeout-seconds) rpc_timeout=$2 ;;
      esac
      shift 2
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done
[[ -n $serial && -n $output ]] || {
  usage >&2
  exit 2
}
[[ $rpc_timeout =~ ^([1-9]|[12][0-9]|30)$ ]] || {
  usage >&2
  exit 2
}
[[ ! -e $output && ! -L $output ]] || fail 'output already exists'
adb_command=${P2P_VPN_ADB:-adb}
for tool in "$adb_command" base64 date head jq ln mktemp timeout tr; do
  command -v "$tool" >/dev/null || fail 'required tool unavailable'
done

adb_read() {
  local result
  result=$(timeout --kill-after=1s "${rpc_timeout}s" \
    "$adb_command" -s "$serial" "$@" | head -c 65537) || fail 'ADB request failed or exceeded its bound'
  ((${#result} <= 65536)) || fail 'ADB response exceeds byte limit'
  printf '%s\n' "$result" | tr -d '\r'
}

[[ $(adb_read get-state) == device ]] || fail 'selected device is not authorized and ready'
sdk=$(adb_read shell getprop ro.build.version.sdk)
model=$(adb_read shell getprop ro.product.model)
abis=$(adb_read shell getprop ro.product.cpu.abilist)
fingerprint=$(adb_read shell getprop ro.build.fingerprint)
qemu=$(adb_read shell getprop ro.kernel.qemu)
[[ $sdk =~ ^[1-9][0-9]{1,2}$ && $sdk -ge 26 ]] || fail 'unsupported Android SDK'
[[ -n $model && ${#model} -le 128 && ! $model =~ [[:cntrl:]] ]] || fail 'invalid device model'
[[ $abis =~ ^[a-zA-Z0-9_,.-]{1,128}$ ]] || fail 'invalid device ABI list'
[[ -n $fingerprint && ${#fingerprint} -le 512 && ! $fingerprint =~ [[:cntrl:]] ]] || fail 'invalid OS build fingerprint'
[[ $qemu == 0 || $qemu == 1 || -z $qemu ]] || fail 'invalid emulator property'

broadcast=$(adb_read shell am broadcast --receiver-foreground \
  -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION \
  -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver \
  --es command power-status)
pattern='^Broadcast completed: result=-1, data="([A-Za-z0-9+/=]+)"$'
encoded=
while IFS= read -r line; do
  if [[ $line =~ $pattern ]]; then
    [[ -z $encoded ]] || fail 'multiple automation responses'
    encoded=${BASH_REMATCH[1]}
  fi
done <<<"$broadcast"
[[ -n $encoded ]] || fail 'missing successful power-status response'

power=$(printf '%s' "$encoded" | base64 --decode 2>/dev/null | jq -ces '
  def integer($min; $max):
    type == "number" and floor == . and . >= $min and . <= $max;
  def optional_integer($min; $max): . == null or integer($min; $max);
  select(length == 1) | .[0] |
  select(.schema_version == 1 and .ok == true and
    (keys == ["ok", "schema_version", "value"])) | .value |
  select(keys == (["schema_version", "scope", "started_elapsed_realtime_ms",
    "finished_elapsed_realtime_ms", "uptime_ms", "app_cpu_time_ms",
    "energy_remaining_nwh", "charge_remaining_uah", "current_now_ua",
    "current_average_ua", "capacity_percent", "battery_status", "plugged_sources",
    "voltage_mv", "temperature_tenths_c", "screen_interactive", "device_idle"] | sort)) |
  select(.schema_version == 1 and .scope == "device_battery") |
  select(all(.started_elapsed_realtime_ms, .finished_elapsed_realtime_ms,
    .uptime_ms, .app_cpu_time_ms; integer(0; 9007199254740991))) |
  select(.finished_elapsed_realtime_ms >= .started_elapsed_realtime_ms and
    .uptime_ms <= .finished_elapsed_realtime_ms) |
  select(.energy_remaining_nwh | optional_integer(0; 9007199254740991)) |
  select(all(.charge_remaining_uah, .plugged_sources, .voltage_mv;
    optional_integer(0; 2147483647))) |
  select(all(.current_now_ua, .current_average_ua, .temperature_tenths_c;
    optional_integer(-2147483647; 2147483647))) |
  select(.capacity_percent | optional_integer(0; 100)) |
  select(.battery_status | optional_integer(1; 5)) |
  select(all(.screen_interactive, .device_idle; . == null or type == "boolean"))
' 2>/dev/null) || fail 'invalid or unsupported power-status schema'

staged=$(mktemp -- "${output}.part.XXXXXXXX")
trap 'rm -f -- "$staged"' EXIT
jq -n --arg sdk "$sdk" --arg model "$model" --arg abis "$abis" \
  --arg fingerprint "$fingerprint" --arg qemu "$qemu" \
  --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson power "$power" '{
    schema_version: 1, kind: "android_power_endpoint", captured_at_utc: $captured_at,
    device: {sdk: ($sdk | tonumber), model: $model, abis: ($abis | split(",")),
      build_fingerprint: $fingerprint, emulator_detected: ($qemu == "1")},
    capabilities: {energy_counter_reported: ($power.energy_remaining_nwh != null)},
    energy_window_eligible: false, energy_window_ineligible_reason: "single_endpoint_only",
    power: $power
  }' >"$staged"
# Publish only a new file, including when another process races the initial check.
ln --no-target-directory -- "$staged" "$output" || fail 'output publication would overwrite an existing path'
printf 'Power endpoint captured: %s (not an energy window)\n' "$output"
