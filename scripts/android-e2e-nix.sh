#!/usr/bin/env bash
set -euo pipefail

readonly official_cache="https://cache.nixos.org"
readonly community_cache="https://nix-community.cachix.org"
readonly community_cache_key="nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
readonly default_min_free_bytes=$((16 * 1024 * 1024 * 1024))
readonly default_max_store_growth_bytes=$((24 * 1024 * 1024 * 1024))
readonly hard_max_store_growth_bytes=$((64 * 1024 * 1024 * 1024))
readonly default_max_local_derivations=256
readonly default_max_planned_derivations=512
readonly default_max_jobs=2

nix_command="${P2P_VPN_ANDROID_E2E_NIX:-nix}"
df_command="${P2P_VPN_ANDROID_E2E_DF:-df}"
flake_ref="${P2P_VPN_ANDROID_E2E_FLAKE:-}"
runtime_target_name="${P2P_VPN_ANDROID_E2E_RUNTIME_TARGET:-android-e2e-runtime}"
min_free_bytes="${P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES:-$default_min_free_bytes}"
max_store_growth_bytes="${P2P_VPN_ANDROID_E2E_MAX_STORE_GROWTH_BYTES:-$default_max_store_growth_bytes}"
max_local_derivations="${P2P_VPN_ANDROID_E2E_MAX_LOCAL_DERIVATIONS:-$default_max_local_derivations}"
max_planned_derivations="${P2P_VPN_ANDROID_E2E_MAX_PLANNED_DERIVATIONS:-$default_max_planned_derivations}"
max_jobs="${P2P_VPN_ANDROID_E2E_MAX_JOBS:-$default_max_jobs}"

case "$runtime_target_name" in
  android-e2e-runtime)
    runtime_binary_name=p2p-vpn-android-e2e
    runner_label="Android E2E"
    ;;
  android-device-audit-runtime)
    runtime_binary_name=p2p-vpn-android-device-audit
    runner_label="Android device audit"
    ;;
  *)
    echo "P2P_VPN_ANDROID_E2E_RUNTIME_TARGET is not an approved Android runtime" >&2
    exit 2
    ;;
esac

if [[ -z "$flake_ref" ]]; then
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
  flake_ref="path:$(cd -- "$script_dir/.." && pwd -P)"
fi

case "$min_free_bytes" in
  ''|*[!0-9]*)
    echo "P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES must be an unsigned integer" >&2
    exit 2
    ;;
esac

case "$max_local_derivations" in
  ''|*[!0-9]*)
    echo "P2P_VPN_ANDROID_E2E_MAX_LOCAL_DERIVATIONS must be an unsigned integer" >&2
    exit 2
    ;;
esac
if ((max_local_derivations < 1 || max_local_derivations > 256)); then
  echo "P2P_VPN_ANDROID_E2E_MAX_LOCAL_DERIVATIONS must be between 1 and 256" >&2
  exit 2
fi

case "$max_store_growth_bytes" in
  ''|*[!0-9]*)
    echo "P2P_VPN_ANDROID_E2E_MAX_STORE_GROWTH_BYTES must be an unsigned integer" >&2
    exit 2
    ;;
esac
if ((${#max_store_growth_bytes} > 18)) \
  || ((max_store_growth_bytes < 1 || max_store_growth_bytes > hard_max_store_growth_bytes)); then
  printf 'P2P_VPN_ANDROID_E2E_MAX_STORE_GROWTH_BYTES must be between 1 and %s\n' \
    "$hard_max_store_growth_bytes" >&2
  exit 2
fi

case "$max_planned_derivations" in
  ''|*[!0-9]*)
    echo "P2P_VPN_ANDROID_E2E_MAX_PLANNED_DERIVATIONS must be an unsigned integer" >&2
    exit 2
    ;;
esac
if ((max_planned_derivations < 1 || max_planned_derivations > 1024)); then
  echo "P2P_VPN_ANDROID_E2E_MAX_PLANNED_DERIVATIONS must be between 1 and 1024" >&2
  exit 2
fi

case "$max_jobs" in
  ''|*[!0-9]*)
    echo "P2P_VPN_ANDROID_E2E_MAX_JOBS must be an unsigned integer" >&2
    exit 2
    ;;
esac
if ((max_jobs < 1 || max_jobs > 4)); then
  echo "P2P_VPN_ANDROID_E2E_MAX_JOBS must be between 1 and 4" >&2
  exit 2
fi

if ! command -v "$nix_command" >/dev/null 2>&1; then
  echo "Nix is required to run the $runner_label harness" >&2
  exit 2
fi
if ! command -v "$df_command" >/dev/null 2>&1; then
  echo "df is required to enforce the $runner_label storage budget" >&2
  exit 2
fi

tmp_available_bytes="$("$df_command" --output=avail -B1 "${TMPDIR:-/tmp}" | tail -n 1 | tr -d '[:space:]')"
store_available_bytes="$("$df_command" --output=avail -B1 /nix/store | tail -n 1 | tr -d '[:space:]')"
if [[ ! "$tmp_available_bytes" =~ ^[0-9]+$ || ! "$store_available_bytes" =~ ^[0-9]+$ ]]; then
  echo "could not determine available space for $runner_label state and Nix store" >&2
  exit 2
fi
if ((tmp_available_bytes < min_free_bytes || store_available_bytes < min_free_bytes)); then
  printf '%s requires at least %s free bytes; tmp has %s and the Nix store has %s\n' \
    "$runner_label" "$min_free_bytes" "$tmp_available_bytes" "$store_available_bytes" >&2
  exit 2
fi

plan_dir="$(mktemp -d -t p2p-vpn-android-e2e-plan.XXXXXXXX)"
build_pid=""
cleanup() {
  if [[ -n "$build_pid" ]] && kill -0 "$build_pid" 2>/dev/null; then
    kill -TERM "$build_pid" 2>/dev/null || true
    wait "$build_pid" 2>/dev/null || true
  fi
  case "$plan_dir" in
    "${TMPDIR:-/tmp}"/p2p-vpn-android-e2e-plan.*)
      chmod -R u+w "$plan_dir" 2>/dev/null || true
      rm -rf -- "$plan_dir"
      ;;
  esac
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if ! "$nix_command" store info \
  --store "$official_cache" \
  --option connect-timeout 30 >/dev/null 2>&1; then
  echo "$runner_label stopped because cache.nixos.org is unavailable" >&2
  exit 69
fi

substituters="$official_cache"
trusted_public_keys="$(
  ("$nix_command" config show trusted-public-keys 2>/dev/null || true) | tr '\n' ' '
)"
if grep -Fq "$community_cache_key" <<<"$trusted_public_keys"; then
  if "$nix_command" store info \
    --store "$community_cache" \
    --option connect-timeout 10 >/dev/null 2>&1; then
    substituters+=" $community_cache"
  else
    echo "$runner_label: trusted nix-community cache is unavailable; using the official cache" >&2
  fi
fi

nix_options=(
  --option fallback false
  --option keep-going false
  --option connect-timeout 30
  --option max-jobs "$max_jobs"
  --option substituters "$substituters"
  --option extra-substituters ""
)

budget_failure=""
last_progress_seconds=0
last_store_growth_bytes=0
check_store_budget() {
  local current_store_available_bytes

  current_store_available_bytes="$({
    "$df_command" --output=avail -B1 /nix/store | tail -n 1 | tr -d '[:space:]'
  } || true)"
  if [[ ! "$current_store_available_bytes" =~ ^[0-9]+$ ]]; then
    budget_failure="could not monitor Nix store free space"
    return 1
  fi

  last_store_growth_bytes=0
  if ((current_store_available_bytes < store_available_bytes)); then
    last_store_growth_bytes=$((store_available_bytes - current_store_available_bytes))
  fi
  if ((current_store_available_bytes < min_free_bytes)); then
    budget_failure="Nix store free space fell below the required reserve"
    return 1
  fi
  if ((last_store_growth_bytes > max_store_growth_bytes)); then
    budget_failure="Nix store growth exceeded the per-run limit"
    return 1
  fi
  return 0
}

run_guarded_nix_build() {
  local label="$1"
  local stdout_file="$2"
  local stderr_file="$3"
  local current_seconds
  local build_status
  shift 3

  : > "$stdout_file"
  : > "$stderr_file"
  budget_failure=""
  "$nix_command" "$@" >"$stdout_file" 2>"$stderr_file" &
  build_pid=$!

  while kill -0 "$build_pid" 2>/dev/null; do
    check_store_budget || break

    current_seconds="$(date +%s)"
    if ((last_progress_seconds == 0 || current_seconds - last_progress_seconds >= 30)); then
      printf '%s: %s has used %s bytes of the %s-byte limit\n' \
        "$runner_label" "$label" "$last_store_growth_bytes" "$max_store_growth_bytes" >&2
      last_progress_seconds="$current_seconds"
    fi
    sleep 1
  done

  if [[ -n "$budget_failure" ]]; then
    kill -TERM "$build_pid" 2>/dev/null || true
  fi

  set +e
  wait "$build_pid"
  build_status=$?
  set -e
  build_pid=""

  if [[ -z "$budget_failure" ]]; then
    check_store_budget || true
  fi

  if [[ -n "$budget_failure" ]]; then
    return 75
  fi
  return "$build_status"
}

runtime_target="${flake_ref}#$runtime_target_name"
plan_log="$plan_dir/plan.log"
if ! "$nix_command" build "$runtime_target" \
  --no-link \
  --dry-run \
  --log-format raw \
  "${nix_options[@]}" \
  >/dev/null 2>"$plan_log"; then
  tail -n 80 "$plan_log" >&2
  echo "$runner_label stopped because the Nix build plan failed" >&2
  exit 1
fi

mapfile -t planned_derivations < <(
  sed -nE 's#^[[:space:]]+(/nix/store/[^[:space:]]+\.drv)$#\1#p' "$plan_log"
)

if ((${#planned_derivations[@]} > max_planned_derivations)); then
  printf '%s stopped: Nix planned %d builds; limit is %d\n' \
    "$runner_label" "${#planned_derivations[@]}" "$max_planned_derivations" >&2
  exit 75
fi

derivation_json=""

is_fixed_output_derivation() {
  printf '%s' "$derivation_json" \
    | tr -d '\n' \
    | grep -Eq '"outputs":\{"[^"]+":\{"hash":'
}

is_cargo_vendor_unpack_derivation() {
  grep -Fq '"buildCommand"' <<<"$derivation_json" \
    && grep -Fq '.cargo-checksum.json' <<<"$derivation_json"
}

is_approved_local_derivation() {
  local path="$1"
  local name="${path##*/}"
  name="${name#*-}"

  case "$name" in
    p2p-vpn-*.drv | android-sdk-*.drv | androidsdk*.drv | run-test-emulator.drv | \
      rustc-with-android-libsrc.drv | cargo-vendor-dir.drv | \
      remove-references-to.drv | bionic-prebuilt.drv | \
      *-unknown-linux-android-ndk-toolchain*.drv | \
      *-unknown-linux-android-rustc*.drv | \
      *-unknown-linux-android-cargo*.drv | rustc-wrapper-*.drv | \
      cargo-*-hook.sh.drv | androidenv-android-sdk-license.drv | \
      stdenv-linux.drv | ncurses-abi5-compat-6.6.drv | gradle-9.5.1.drv)
      return 0
      ;;
  esac

  is_fixed_output_derivation || is_cargo_vendor_unpack_derivation
}

unexpected_derivations=()
non_fixed_derivations=0
early_fixed_output_derivations=()
late_fixed_output_derivations=()
for derivation in "${planned_derivations[@]}"; do
  if ! derivation_json="$("$nix_command" derivation show "$derivation" 2>/dev/null)"; then
    unexpected_derivations+=("${derivation##*/} (could not inspect)")
    continue
  fi
  if is_fixed_output_derivation; then
    case "${derivation##*/}" in
      *-crate-*.tar.gz.drv | *.jar.drv | *.module.drv | *.pom.drv | *.xml.drv)
        early_fixed_output_derivations+=("$derivation")
        ;;
      *)
        late_fixed_output_derivations+=("$derivation")
        ;;
    esac
  else
    ((non_fixed_derivations += 1))
  fi
  if ! is_approved_local_derivation "$derivation"; then
    unexpected_derivations+=("${derivation##*/}")
  fi
done

if ((non_fixed_derivations > max_local_derivations)); then
  printf '%s stopped: Nix planned %d non-fixed builds; limit is %d\n' \
    "$runner_label" "$non_fixed_derivations" "$max_local_derivations" >&2
  exit 75
fi

if ((${#unexpected_derivations[@]} > 0)); then
  echo "$runner_label stopped before an unexpected third-party source build:" >&2
  printf '  %s\n' "${unexpected_derivations[@]}" >&2
  echo "Restore binary-cache access and retry; do not bypass this guard casually." >&2
  exit 75
fi

fixed_output_derivations=(
  "${early_fixed_output_derivations[@]}"
  "${late_fixed_output_derivations[@]}"
)
if ((${#fixed_output_derivations[@]} > 0)); then
  printf '%s: prefetching %d fixed-output inputs sequentially\n' \
    "$runner_label" "${#fixed_output_derivations[@]}" >&2
fi

prefetch_stdout="$plan_dir/prefetch.out"
prefetch_log="$plan_dir/prefetch.log"
for derivation in "${fixed_output_derivations[@]}"; do
  if run_guarded_nix_build \
    "fixed-output prefetch" \
    "$prefetch_stdout" \
    "$prefetch_log" \
    build "${derivation}^*" \
    --no-link \
    --log-format raw \
    "${nix_options[@]}"; then
    continue
  else
    prefetch_status=$?
  fi

  tail -n 80 "$prefetch_log" >&2 || true
  if ((prefetch_status == 75)); then
    echo "$runner_label stopped: $budget_failure" >&2
    exit 75
  fi
  printf '%s stopped before runtime realization: fixed-output input failed: %s\n' \
    "$runner_label" "${derivation##*/}" >&2
  exit 1
done

runtime_path_file="$plan_dir/runtime-path"
realize_log="$plan_dir/realize.log"
printf '%s: realizing at most %s bytes with %s build jobs\n' \
  "$runner_label" "$max_store_growth_bytes" "$max_jobs" >&2

if run_guarded_nix_build \
  "runtime realization" \
  "$runtime_path_file" \
  "$realize_log" \
  build "$runtime_target" \
  --no-link \
  --print-out-paths \
  --log-format raw \
  "${nix_options[@]}"; then
  build_status=0
else
  build_status=$?
fi

if ((build_status == 75)); then
  tail -n 80 "$realize_log" >&2 || true
  echo "$runner_label stopped: $budget_failure" >&2
  exit 75
fi

if ((build_status != 0)); then
  tail -n 80 "$realize_log" >&2 || true
  echo "$runner_label runtime realization failed without source fallback" >&2
  exit 1
fi

mapfile -t runtime_paths <"$runtime_path_file"
if ((${#runtime_paths[@]} != 1)) || [[ ! -x "${runtime_paths[0]}/bin/$runtime_binary_name" ]]; then
  echo "$runner_label runtime realization returned an invalid output" >&2
  exit 1
fi

cleanup
trap - EXIT
exec "${runtime_paths[0]}/bin/$runtime_binary_name" "$@"
