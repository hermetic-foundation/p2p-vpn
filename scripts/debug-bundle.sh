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

write_commands() {
  {
    echo "#!/usr/bin/env bash"
    echo "set -euo pipefail"
    printf "export P2P_VPN_DEBUG_BUNDLE_DIR=%q\n" "$artifact_dir"
    printf "export P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST=%q\n" "${P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST:-0}"
    echo
    echo "nix run .#debug-bundle"
    echo "sed -n '1,220p' \"$metadata\""
    echo "sed -n '1,220p' \"$toolchain\""
    echo "sed -n '1,220p' \"$host\""
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
} >"$metadata"
command_metadata "git rev-parse HEAD" git rev-parse HEAD
command_metadata "git status --short" git status --short
command_metadata "jj status" jj status
command_metadata "cargo metadata --no-deps --format-version 1" cargo metadata --no-deps --format-version 1

capture_host
capture_toolchain
capture_flake_show
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
  --arg check_stdout "$check_stdout" \
  --arg check_stderr "$check_stderr" \
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
      check_fast_stdout: (if $check_fast_enabled then $check_stdout else null end),
      check_fast_stderr: (if $check_fast_enabled then $check_stderr else null end)
    },
    check_fast: {
      enabled: $check_fast_enabled,
      status: $check_fast_status
    }
  }' >"$summary_json"

cat "$summary"
exit "$check_status"
