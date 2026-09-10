#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != --inside ]]; then
  if [[ "$#" -lt 2 || "$1" != -- ]]; then
    printf 'Usage: bash scripts/android-resource-isolation.sh -- command [arguments...]\n' >&2
    exit 2
  fi
  shift
  exec unshare --user --map-root-user --net --mount --pid --fork --kill-child \
    --propagation private bash "$0" --inside \
    "$(readlink /proc/self/ns/net)" "$(readlink /proc/self/ns/pid)" \
    "$(readlink /proc/self/ns/mnt)" -- "$@"
fi

if [[ "$#" -lt 6 || "$5" != -- ]]; then
  printf 'Invalid internal isolation invocation.\n' >&2
  exit 2
fi
for kind in net pid mnt; do
  case "$kind" in
    net) parent="$2" ;;
    pid) parent="$3" ;;
    mnt) parent="$4" ;;
  esac
  if [[ "$(readlink "/proc/self/ns/$kind")" == "$parent" ]]; then
    printf 'Required %s namespace isolation is absent.\n' "$kind" >&2
    exit 1
  fi
done
shift 5

# These mounts are private to the new user/mount namespace.
mount -t proc proc /proc
if [[ -d /dev/bus/usb ]]; then
  mount -t tmpfs -o size=64k,nosuid,nodev,noexec,mode=000 tmpfs /dev/bus/usb
  [[ -z "$(find /dev/bus/usb -mindepth 1 -print -quit)" ]]
fi
ip -j link show | jq -e 'length == 1 and .[0].ifname == "lo"' >/dev/null
ip link set lo up
ip -j -4 route show table all | jq -e 'all(.[]; .dev == "lo")' >/dev/null
ip -j -6 route show table all | jq -e 'all(.[]; .dev == "lo")' >/dev/null

# A fresh network namespace cannot reach the host's TCP ADB server.
export ADB_SERVER_SOCKET=tcp:127.0.0.1:5037
export ADB_MDNS_AUTO_CONNECT=0
unset ANDROID_SERIAL
exec "$@"
