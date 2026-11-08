#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == -s ]]; then
  [[ $2 == test-device ]]
  shift 2
  printf '%s\n' "$*" >>"$P2P_VPN_POWER_TEST_STATE/calls"
  mode=${P2P_VPN_POWER_TEST_MODE:-valid}
  case "$*" in
    get-state)
      if [[ $mode == unauthorized ]]; then echo unauthorized; else echo device; fi
      ;;
    'shell getprop ro.build.version.sdk')
      if [[ $mode == sdk ]]; then echo 25; else echo 37; fi
      ;;
    'shell getprop ro.product.model')
      if [[ $mode == model ]]; then printf 'Test\nModel\n'; else echo 'Test Model'; fi
      ;;
    'shell getprop ro.product.cpu.abilist') echo 'arm64-v8a,armeabi-v7a' ;;
    'shell getprop ro.build.fingerprint') echo 'test/vendor/build:37/test/release-keys' ;;
    'shell getprop ro.kernel.qemu')
      if [[ $mode == emulator ]]; then echo 1; else echo 0; fi
      ;;
    'shell am broadcast --receiver-foreground -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver --es command power-status')
      case "$mode" in
        timeout) sleep 3 ;;
        oversized)
          head -c 70000 /dev/zero | tr '\0' A
          exit 0
          ;;
        malformed)
          echo 'Broadcast completed: result=-1, data="aW52YWxpZA=="'
          exit 0
          ;;
        race) printf 'keep-race\n' >"$P2P_VPN_POWER_TEST_STATE/race.json" ;;
      esac
      encoded=$(jq -nc --arg mode "$mode" '{schema_version:1,ok:true,value:{
        schema_version:1,scope:"device_battery",started_elapsed_realtime_ms:1000,
        finished_elapsed_realtime_ms:1001,uptime_ms:900,app_cpu_time_ms:15,
        energy_remaining_nwh:18000000000,charge_remaining_uah:4000000,
        current_now_ua:-1234,current_average_ua:-1200,capacity_percent:81,
        battery_status:3,plugged_sources:0,voltage_mv:4120,temperature_tenths_c:264,
        screen_interactive:false,device_idle:true}} |
        if $mode == "unsupported" then .value.energy_remaining_nwh=null
        elif $mode == "zero" then .value.energy_remaining_nwh=0
        elif $mode == "backwards" then .value.finished_elapsed_realtime_ms=999
        elif $mode == "string" then .value.energy_remaining_nwh="18000000000"
        elif $mode == "sentinel" then .value.current_now_ua=-2147483648
        elif $mode == "unsafe" then .value.energy_remaining_nwh=9007199254740992
        elif $mode == "charging" then .value.plugged_sources=2 | .value.battery_status=2
        elif $mode == "secret" then .value.private_key="must-not-appear"
        elif $mode == "outer_secret" then .private_key="must-not-appear"
        elif $mode == "schema" then .schema_version=2
        elif $mode == "error" then .ok=false
        elif $mode == "missing" then del(.value.uptime_ms)
        elif $mode == "stream" then ., .
        elif $mode == "value_string" then .value="must-not-appear"
        else . end' | base64 -w 0)
      printf 'Broadcast completed: result=-1, data="%s"\n' "$encoded"
      if [[ $mode == duplicate ]]; then
        printf 'Broadcast completed: result=-1, data="%s"\n' "$encoded"
      fi
      ;;
    *)
      echo 'Unexpected or mutating ADB command' >&2
      exit 1
      ;;
  esac
  exit 0
fi

capture=${P2P_VPN_ANDROID_POWER_CAPTURE:-$(dirname "$0")/../scripts/android-power-capture.sh}
state=$(mktemp -d)
trap 'rm -rf -- "$state"' EXIT
export P2P_VPN_POWER_TEST_STATE=$state
# Source scripts' /usr/bin/env shebang is unavailable inside a Nix sandbox.
printf '#!%s\nexec %q %q "$@"\n' "$(command -v bash)" "$(command -v bash)" "$(realpath "$0")" >"$state/adb"
chmod +x "$state/adb"
export P2P_VPN_ADB="$state/adb"
checks=0
for mode in valid unsupported zero charging emulator; do
  export P2P_VPN_POWER_TEST_MODE=$mode
  bash "$capture" --serial test-device --output "$state/$mode.json" >/dev/null
  jq -e '.kind == "android_power_endpoint" and .device.sdk == 37 and
    .device.model == "Test Model" and .energy_window_eligible == false and
    .energy_window_ineligible_reason == "single_endpoint_only" and
    (.device | has("serial") | not)' "$state/$mode.json" >/dev/null
  [[ $(stat -c %a "$state/$mode.json") == 600 ]]
  checks=$((checks + 1))
done
jq -e '.capabilities.energy_counter_reported == false and .power.energy_remaining_nwh == null' "$state/unsupported.json" >/dev/null
jq -e '.capabilities.energy_counter_reported and .power.energy_remaining_nwh == 0' "$state/zero.json" >/dev/null
jq -e '.power.plugged_sources == 2 and .energy_window_eligible == false' "$state/charging.json" >/dev/null
jq -e '.device.emulator_detected and .energy_window_eligible == false' "$state/emulator.json" >/dev/null
for mode in backwards string sentinel unsafe secret outer_secret schema error missing stream value_string malformed duplicate unauthorized oversized timeout sdk model; do
  export P2P_VPN_POWER_TEST_MODE=$mode
  if bash "$capture" --serial test-device --output "$state/$mode.json" --rpc-timeout-seconds 1 >"$state/stdout" 2>"$state/stderr"; then
    echo "Invalid $mode capture accepted" >&2
    exit 1
  fi
  [[ ! -e "$state/$mode.json" ]]
  if grep -q must-not-appear "$state/stdout" "$state/stderr"; then
    echo 'Private response field leaked into diagnostics' >&2
    exit 1
  fi
  checks=$((checks + 1))
done
printf 'keep-original\n' >"$state/existing.json"
export P2P_VPN_POWER_TEST_MODE=valid
if bash "$capture" --serial test-device --output "$state/existing.json" >/dev/null 2>&1; then
  echo 'Existing output was overwritten' >&2
  exit 1
fi
[[ $(<"$state/existing.json") == keep-original ]]
checks=$((checks + 1))
ln -s "$state/absent-target" "$state/symlink.json"
if bash "$capture" --serial test-device --output "$state/symlink.json" >/dev/null 2>&1; then
  echo 'Existing symlink was replaced' >&2
  exit 1
fi
[[ -L "$state/symlink.json" && ! -e "$state/absent-target" ]]
checks=$((checks + 1))
export P2P_VPN_POWER_TEST_MODE=race
if bash "$capture" --serial test-device --output "$state/race.json" >/dev/null 2>&1; then
  echo 'Racing output was overwritten' >&2
  exit 1
fi
[[ $(<"$state/race.json") == keep-race ]]
checks=$((checks + 1))
[[ -z $(find "$state" -name '*.part.*' -print -quit) ]]
for args in '' '--output file' '--serial test-device' '--unknown' '--serial test-device --output file --rpc-timeout-seconds 0'; do
  read -r -a arguments <<<"$args"
  if bash "$capture" "${arguments[@]}" >/dev/null 2>&1; then
    echo 'Invalid arguments accepted' >&2
    exit 1
  fi
  checks=$((checks + 1))
done
printf 'Power endpoint capture checks passed: %s cases (valid, negative, privacy, bounds and publication).\n' "$checks"
