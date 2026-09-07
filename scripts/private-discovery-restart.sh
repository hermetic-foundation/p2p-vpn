#!/usr/bin/env bash
set -euo pipefail
readonly test_name=tests::private_bootstrap_restores_address_only_discovery_after_restart

if [[ "${1:-}" != --inside ]]; then
  if [[ "$#" != 1 || ! -x "$1" ]]; then
    printf 'Usage: bash scripts/private-discovery-restart.sh <fixture-test-binary>\n' >&2
    exit 2
  fi
  if ! "$1" --list --exact "$test_name" | grep -Fxq "$test_name: test"; then
    printf 'The supplied binary does not contain the restart test.\n' >&2
    exit 2
  fi
  exec unshare --user --map-root-user --net bash "$0" --inside "$1"
fi

ip link set lo up
ip address add 192.168.250.1/32 dev lo
export P2P_VPN_PRIVATE_RESTART_NAMESPACE=1
exec "$2" --ignored --nocapture --exact "$test_name"
