#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f Cargo.toml || ! -d src ]]; then
  echo "debug-bundle must be run from the p2p-vpn repository root" >&2
  exit 2
fi

umask 077

artifact_dir="${P2P_VPN_DEBUG_BUNDLE_DIR:-}"
if [[ -z "$artifact_dir" ]]; then
  artifact_dir="$(mktemp -d -t p2p-vpn-debug-bundle.XXXXXXXX)"
fi
mkdir -p "$artifact_dir"

metadata="$artifact_dir/debug-metadata.txt"
host="$artifact_dir/debug-host.txt"
toolchain="$artifact_dir/debug-toolchain.txt"
flake_show="$artifact_dir/debug-flake-show.txt"
commands="$artifact_dir/debug-commands.sh"
summary="$artifact_dir/debug-summary.txt"
summary_json="$artifact_dir/debug-summary.json"
check_stdout="$artifact_dir/check-fast.stdout"
check_stderr="$artifact_dir/check-fast.stderr"
control_socket="${P2P_VPN_DEBUG_BUNDLE_CONTROL_SOCKET:-}"
daemon_health="$artifact_dir/daemon-health.txt"
daemon_status="$artifact_dir/daemon-status.txt"
daemon_status_prometheus="$artifact_dir/daemon-status-prometheus.txt"
daemon_state="$artifact_dir/daemon-state.txt"
daemon_state_json="$artifact_dir/daemon-state.json"
daemon_paths="$artifact_dir/daemon-paths.txt"
daemon_paths_json="$artifact_dir/daemon-paths.json"
daemon_mtu="$artifact_dir/daemon-mtu.txt"
daemon_mtu_json="$artifact_dir/daemon-mtu.json"
daemon_capabilities="$artifact_dir/daemon-capabilities.txt"
daemon_capabilities_json="$artifact_dir/daemon-capabilities.json"
daemon_control_summary="$artifact_dir/daemon-control-summary.txt"

command_metadata() {
  local label="$1"
  shift
  {
    echo
    echo "[$label]"
    if "$@" >"$artifact_dir/.command.stdout" 2>"$artifact_dir/.command.stderr"; then
      echo "status: 0"
    else
      local status="$?"
      echo "status: $status"
    fi
    echo "stdout:"
    cat "$artifact_dir/.command.stdout"
    echo "stderr:"
    cat "$artifact_dir/.command.stderr"
  } >>"$metadata"
  rm -f "$artifact_dir/.command.stdout" "$artifact_dir/.command.stderr"
}

capture_host() {
  {
    echo "captured_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "system=$(uname -a)"
    echo
    if [[ -r /etc/os-release ]]; then
      echo "[/etc/os-release]"
      sed -n '1,80p' /etc/os-release
      echo
    fi
    echo "[ip -br addr]"
    ip -br addr || true
    echo
    echo "[ip route show]"
    ip route show || true
    echo
    echo "[ip -6 route show]"
    ip -6 route show || true
    echo
    echo "[ss -lunpt]"
    ss -lunpt || true
    echo
    echo "[ps -o pid,ppid,stat,comm,args -C p2p-vpn]"
    ps -o pid,ppid,stat,comm,args -C p2p-vpn || true
  } >"$host" 2>&1
}

capture_toolchain() {
  {
    echo "captured_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo
    for tool in nix cargo rustc rustfmt clippy-driver jq jj git ip ss unshare; do
      if command -v "$tool" >/dev/null 2>&1; then
        echo "[$tool]"
        command -v "$tool"
        case "$tool" in
          nix) nix --version || true ;;
          cargo) cargo --version || true ;;
          rustc) rustc --version --verbose || true ;;
          rustfmt) rustfmt --version || true ;;
          clippy-driver) clippy-driver --version || true ;;
          jq) jq --version || true ;;
          jj) jj --version || true ;;
          git) git --version || true ;;
          ip) ip -Version || true ;;
          ss) ss --version || true ;;
          unshare) unshare --version || true ;;
        esac
      else
        echo "[$tool]"
        echo "missing"
      fi
      echo
    done
  } >"$toolchain" 2>&1
}

capture_flake_show() {
  if command -v nix >/dev/null 2>&1; then
    nix flake show --allow-import-from-derivation >"$flake_show" 2>&1 || true
  else
    echo "nix missing" >"$flake_show"
  fi
}

capture_daemon_control() {
  {
    echo "captured_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "control_socket=$control_socket"
    if [[ -z "$control_socket" ]]; then
      echo "enabled=false"
      echo "reason=P2P_VPN_DEBUG_BUNDLE_CONTROL_SOCKET unset"
      return 0
    fi
    echo "enabled=true"
    if [[ ! -S "$control_socket" ]]; then
      echo "socket_present=false"
      echo "reason=control socket is missing or is not a socket"
      {
        echo "daemon_health_ready false"
        echo "check control_socket ok=false value=0 detail=\"control socket is missing or is not a socket\""
      } >"$daemon_health"
      return 0
    fi
    echo "socket_present=true"
  } >"$daemon_control_summary"

  if [[ -z "$control_socket" || ! -S "$control_socket" ]]; then
    return 0
  fi

  local wait_seconds="${P2P_VPN_DEBUG_BUNDLE_HEALTH_WAIT_SECONDS:-1}"
  local health_args=(--socket "$control_socket" --wait-seconds "$wait_seconds")
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_VALIDATED_PEERS:-0}" == 1 ]]; then
    health_args+=(--require-validated-peers)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_SUPPORTED_PATHS:-0}" == 1 ]]; then
    health_args+=(--require-supported-paths)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_PACKET_SESSION:-0}" == 1 ]]; then
    health_args+=(--require-packet-plane-session)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_QUIC_SESSION:-0}" == 1 ]]; then
    health_args+=(--require-packet-plane-quic-session)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_OBSERVED_UDP_ENDPOINT:-0}" == 1 ]]; then
    health_args+=(--require-observed-packet-plane-udp-endpoint)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_OBSERVED_QUIC_ENDPOINT:-0}" == 1 ]]; then
    health_args+=(--require-observed-packet-plane-quic-endpoint)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_INFRASTRUCTURE_PEER:-0}" == 1 ]]; then
    health_args+=(--require-auto-relay-infrastructure-peer)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_CANDIDATE:-0}" == 1 ]]; then
    health_args+=(--require-auto-relay-candidate)
  fi
  if [[ "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_RESERVATION:-0}" == 1 ]]; then
    health_args+=(--require-auto-relay-reservation)
  fi

  {
    printf "health_args:"
    printf " %q" "${health_args[@]}"
    printf "\n"
  } >>"$daemon_control_summary"

  p2p-vpn daemon-health "${health_args[@]}" >"$daemon_health" 2>"$daemon_health.stderr" || true
  p2p-vpn daemon-status --socket "$control_socket" >"$daemon_status" 2>"$daemon_status.stderr" || true
  p2p-vpn daemon-status --socket "$control_socket" --format prometheus >"$daemon_status_prometheus" 2>"$daemon_status_prometheus.stderr" || true
  p2p-vpn daemon-state --socket "$control_socket" >"$daemon_state" 2>"$daemon_state.stderr" || true
  p2p-vpn daemon-state --socket "$control_socket" --format json >"$daemon_state_json" 2>"$daemon_state_json.stderr" || true
  p2p-vpn daemon-paths --socket "$control_socket" >"$daemon_paths" 2>"$daemon_paths.stderr" || true
  p2p-vpn daemon-paths --socket "$control_socket" --format json >"$daemon_paths_json" 2>"$daemon_paths_json.stderr" || true
  p2p-vpn daemon-mtu --socket "$control_socket" >"$daemon_mtu" 2>"$daemon_mtu.stderr" || true
  p2p-vpn daemon-mtu --socket "$control_socket" --format json >"$daemon_mtu_json" 2>"$daemon_mtu_json.stderr" || true
  p2p-vpn daemon-capabilities --socket "$control_socket" >"$daemon_capabilities" 2>"$daemon_capabilities.stderr" || true
  p2p-vpn daemon-capabilities --socket "$control_socket" --format json >"$daemon_capabilities_json" 2>"$daemon_capabilities_json.stderr" || true
}

write_commands() {
  {
    echo "#!/usr/bin/env bash"
    echo "set -euo pipefail"
    printf "export P2P_VPN_DEBUG_BUNDLE_DIR=%q\n" "$artifact_dir"
    printf "export P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST=%q\n" "${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_CONTROL_SOCKET=%q\n" "$control_socket"
    printf "export P2P_VPN_DEBUG_BUNDLE_HEALTH_WAIT_SECONDS=%q\n" "${P2P_VPN_DEBUG_BUNDLE_HEALTH_WAIT_SECONDS:-1}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_VALIDATED_PEERS=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_VALIDATED_PEERS:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_SUPPORTED_PATHS=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_SUPPORTED_PATHS:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_PACKET_SESSION=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_PACKET_SESSION:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_QUIC_SESSION=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_QUIC_SESSION:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_OBSERVED_UDP_ENDPOINT=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_OBSERVED_UDP_ENDPOINT:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_OBSERVED_QUIC_ENDPOINT=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_OBSERVED_QUIC_ENDPOINT:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_INFRASTRUCTURE_PEER=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_INFRASTRUCTURE_PEER:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_CANDIDATE=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_CANDIDATE:-0}"
    printf "export P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_RESERVATION=%q\n" "${P2P_VPN_DEBUG_BUNDLE_REQUIRE_AUTO_RELAY_RESERVATION:-0}"
    echo
    echo "nix run .#debug-bundle"
    echo "sed -n '1,220p' \"$metadata\""
    echo "sed -n '1,220p' \"$toolchain\""
    echo "sed -n '1,220p' \"$host\""
    echo "sed -n '1,220p' \"$daemon_control_summary\""
    echo "sed -n '1,220p' \"$daemon_health\""
    echo "sed -n '1,220p' \"$summary\""
    echo "jq . \"$summary_json\""
  } >"$commands"
  chmod +x "$commands"
}

run_check_fast() {
  if [[ "${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}" != 1 ]]; then
    return 0
  fi

  set +e
  if command -v p2p-vpn-check-fast >/dev/null 2>&1; then
    p2p-vpn-check-fast >"$check_stdout" 2>"$check_stderr"
  else
    nix run .#check-fast >"$check_stdout" 2>"$check_stderr"
  fi
  local status="$?"
  set -e
  return "$status"
}

{
  echo "debug_bundle_metadata_version=1"
  echo "artifact_dir=$artifact_dir"
  echo "started_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "working_directory=$(pwd)"
  echo "run_check_fast=${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}"
  echo "control_socket=$control_socket"
} >"$metadata"
command_metadata "git rev-parse HEAD" git rev-parse HEAD
command_metadata "git status --short" git status --short
command_metadata "jj status" jj status
command_metadata "cargo metadata --no-deps --format-version 1" cargo metadata --no-deps --format-version 1

capture_host
capture_toolchain
capture_flake_show
capture_daemon_control
write_commands

set +e
run_check_fast
check_status="$?"
set -e

{
  echo "p2p-vpn debug bundle"
  echo "artifact_dir=$artifact_dir"
  echo "metadata=$metadata"
  echo "host=$host"
  echo "toolchain=$toolchain"
  echo "flake_show=$flake_show"
  echo "commands=$commands"
  echo "summary_json=$summary_json"
  echo "daemon_control_summary=$daemon_control_summary"
  if [[ -n "$control_socket" ]]; then
    echo "daemon_health=$daemon_health"
    echo "daemon_status=$daemon_status"
    echo "daemon_status_prometheus=$daemon_status_prometheus"
    echo "daemon_state=$daemon_state"
    echo "daemon_state_json=$daemon_state_json"
    echo "daemon_paths=$daemon_paths"
    echo "daemon_paths_json=$daemon_paths_json"
    echo "daemon_mtu=$daemon_mtu"
    echo "daemon_mtu_json=$daemon_mtu_json"
    echo "daemon_capabilities=$daemon_capabilities"
    echo "daemon_capabilities_json=$daemon_capabilities_json"
  fi
  echo "check_fast_enabled=${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}"
  echo "check_fast_status=$check_status"
  if [[ "${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}" == 1 ]]; then
    echo "check_fast_stdout=$check_stdout"
    echo "check_fast_stderr=$check_stderr"
  fi
} >"$summary"

jq -n \
  --arg artifact_dir "$artifact_dir" \
  --arg metadata "$metadata" \
  --arg host "$host" \
  --arg toolchain "$toolchain" \
  --arg flake_show "$flake_show" \
  --arg commands "$commands" \
  --arg summary "$summary" \
  --arg control_socket "$control_socket" \
  --arg daemon_control_summary "$daemon_control_summary" \
  --arg daemon_health "$daemon_health" \
  --arg daemon_status "$daemon_status" \
  --arg daemon_status_prometheus "$daemon_status_prometheus" \
  --arg daemon_state "$daemon_state" \
  --arg daemon_state_json "$daemon_state_json" \
  --arg daemon_paths "$daemon_paths" \
  --arg daemon_paths_json "$daemon_paths_json" \
  --arg daemon_mtu "$daemon_mtu" \
  --arg daemon_mtu_json "$daemon_mtu_json" \
  --arg daemon_capabilities "$daemon_capabilities" \
  --arg daemon_capabilities_json "$daemon_capabilities_json" \
  --arg check_stdout "$check_stdout" \
  --arg check_stderr "$check_stderr" \
  --argjson daemon_control_enabled "$([[ -n "$control_socket" ]] && echo true || echo false)" \
  --argjson daemon_control_socket_present "$([[ -n "$control_socket" && -S "$control_socket" ]] && echo true || echo false)" \
  --argjson check_fast_enabled "$([[ "${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}" == 1 ]] && echo true || echo false)" \
  --argjson check_fast_status "$check_status" \
  '{
    schema_version: 1,
    artifact_dir: $artifact_dir,
    artifacts: {
      metadata: $metadata,
      host: $host,
      toolchain: $toolchain,
      flake_show: $flake_show,
      replay_commands: $commands,
      summary: $summary,
      daemon_control_summary: $daemon_control_summary,
      daemon_health: (if $daemon_control_enabled then $daemon_health else null end),
      daemon_status: (if $daemon_control_enabled then $daemon_status else null end),
      daemon_status_prometheus: (if $daemon_control_enabled then $daemon_status_prometheus else null end),
      daemon_state: (if $daemon_control_enabled then $daemon_state else null end),
      daemon_state_json: (if $daemon_control_enabled then $daemon_state_json else null end),
      daemon_paths: (if $daemon_control_enabled then $daemon_paths else null end),
      daemon_paths_json: (if $daemon_control_enabled then $daemon_paths_json else null end),
      daemon_mtu: (if $daemon_control_enabled then $daemon_mtu else null end),
      daemon_mtu_json: (if $daemon_control_enabled then $daemon_mtu_json else null end),
      daemon_capabilities: (if $daemon_control_enabled then $daemon_capabilities else null end),
      daemon_capabilities_json: (if $daemon_control_enabled then $daemon_capabilities_json else null end),
      check_fast_stdout: (if $check_fast_enabled then $check_stdout else null end),
      check_fast_stderr: (if $check_fast_enabled then $check_stderr else null end)
    },
    daemon_control: {
      enabled: $daemon_control_enabled,
      socket: (if $daemon_control_enabled then $control_socket else null end),
      socket_present: $daemon_control_socket_present
    },
    check_fast: {
      enabled: $check_fast_enabled,
      status: $check_fast_status
    }
  }' >"$summary_json"

cat "$summary"
exit "$check_status"
