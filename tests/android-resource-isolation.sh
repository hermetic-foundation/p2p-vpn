#!/usr/bin/env bash
set -euo pipefail
wrapper="$(dirname "$0")/../scripts/android-resource-isolation.sh"
readonly wrapper

expect_status() {
  local expected="$1" actual=0
  shift
  "$@" || actual=$?
  [[ "$actual" == "$expected" ]] || {
    printf 'Expected exit %s, received %s\n' "$expected" "$actual" >&2
    exit 1
  }
}

expect_status 2 bash "$wrapper"
expect_status 1 bash "$wrapper" --inside \
  "$(readlink /proc/self/ns/net)" "$(readlink /proc/self/ns/pid)" \
  "$(readlink /proc/self/ns/mnt)" -- false
expect_status 23 bash "$wrapper" -- bash -c 'exit 23'

before_usb="$(find /dev/bus/usb -mindepth 1 -maxdepth 1 -print | sort)"
export ANDROID_SERIAL=must-not-survive
# shellcheck disable=SC2016
bash "$wrapper" -- bash -c '
  set -euo pipefail
  [[ $$ == 1 ]]
  [[ -z ${ANDROID_SERIAL+x} ]]
  [[ $ADB_SERVER_SOCKET == tcp:localhost:5037 ]]
  [[ $ADB_MDNS_AUTO_CONNECT == 0 ]]
  [[ -z $(find /dev/bus/usb -mindepth 1 -print -quit) ]]
  ip -j link show | jq -e "length == 1 and .[0].ifname == \"lo\"" >/dev/null
  ip -j -4 route show table all | jq -e "all(.[]; .dev == \"lo\")" >/dev/null
  ip -j -6 route show table all | jq -e "all(.[]; .dev == \"lo\")" >/dev/null
  [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]]
'
after_usb="$(find /dev/bus/usb -mindepth 1 -maxdepth 1 -print | sort)"
[[ "$before_usb" == "$after_usb" ]]

readonly marker="p2p-vpn-isolation-orphan-$$"
# shellcheck disable=SC2016
bash "$wrapper" -- bash -c 'bash -c "sleep 60 & wait" "$1" & child=$!; sleep 0.1; kill -0 "$child"' sh "$marker"
if pgrep -f " $marker$" >/dev/null; then
  printf 'Isolation left a descendant running.\n' >&2
  exit 1
fi
expect_status 124 timeout --signal=TERM --kill-after=2s 1 \
  bash "$wrapper" -- bash -c 'sleep 60 & wait' "$marker"
if pgrep -f " $marker$" >/dev/null; then
  printf 'Timeout left the isolated command running.\n' >&2
  exit 1
fi
printf 'Isolation checks passed: guards, network, USB, KVM, exit status and descendant cleanup.\n'
if [[ -n "${P2P_VPN_ADB:-}" ]]; then
  # shellcheck disable=SC2016
  bash "$wrapper" -- bash -c '
    set -euo pipefail
    "$1" start-server
    devices=$("$1" devices)
    [[ $(printf "%s\n" "$devices" | sed "/^List of devices attached/d; /^[[:space:]]*$/d") == "" ]]
  ' sh "$P2P_VPN_ADB"
  printf 'Private ADB auto-start and empty device-list checks passed.\n'
fi
