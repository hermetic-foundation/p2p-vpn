#!/usr/bin/env bash
set -euo pipefail

if (( $# == 2 )) && [[ $1 == --sdk-package ]]; then
    script_dir=$(dirname -- "${BASH_SOURCE[0]}")
    if [[ -n ${P2P_VPN_BROWSER_OUT_DIR:-} ]]; then
        exec bash "$script_dir/stage-browser-sdk.sh" "$2" "$P2P_VPN_BROWSER_OUT_DIR"
    fi
    exec bash "$script_dir/stage-browser-sdk.sh" "$2"
fi

if (( $# > 1 )) || { (( $# == 1 )) && [[ $1 != --test-hooks ]]; }; then
    printf 'usage: bash scripts/build-browser.sh [--test-hooks | --sdk-package SDK_PKG]\n' >&2
    exit 2
fi

cd "$(dirname -- "${BASH_SOURCE[0]}")/.."
version=$(awk '
    $0 == "name = \"wasm-bindgen\"" { found = 1; next }
    found && /^version = / { gsub(/"/, "", $3); print $3; exit }
' Cargo.lock)
if [[ -z "$version" ]]; then
    printf 'Could not determine the locked wasm-bindgen version.\n' >&2
    exit 1
fi
if ! command -v wasm-bindgen >/dev/null || [[ $(wasm-bindgen --version) != "wasm-bindgen $version" ]]; then
    printf 'Install the matching CLI: cargo install --locked wasm-bindgen-cli --version %s\n' "$version" >&2
    exit 1
fi

args=(--locked --package p2p-vpn-browser --target wasm32-unknown-unknown --lib)
if (( $# )); then
    args+=(--features test-hooks)
fi
target=$(cargo metadata --locked --no-deps --format-version 1 | jq -er .target_directory)
output=${P2P_VPN_BROWSER_OUT_DIR:-crates/p2p-vpn-browser/web/pkg}
cargo build "${args[@]}"
# Keep packages within the companion's asset budget, including test hooks.
wasm-bindgen --target web --out-dir "$output" --remove-name-section \
    "$target/wasm32-unknown-unknown/debug/p2p_vpn_browser.wasm"
cp assets/p2p-vpn-logo.svg "$output/logo.svg"
