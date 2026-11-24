#!/usr/bin/env bash
set -euo pipefail

stage="$(dirname -- "${BASH_SOURCE[0]}")/stage-browser-sdk.sh"
if (( $# == 2 )) && [[ $1 == --sdk-package ]]; then
    args=("$2")
    if [[ -n ${P2P_VPN_BROWSER_OUT_DIR:-} ]]; then
        args+=("$P2P_VPN_BROWSER_OUT_DIR")
    fi
    exec bash "$stage" "${args[@]}"
fi
if (( $# > 1 )) || { (( $# == 1 )) && [[ $1 != --test-hooks ]]; }; then
    printf 'usage: bash scripts/build-browser.sh [--test-hooks | --sdk-package SDK_PKG]\n' >&2
    exit 2
fi

cd "$(dirname -- "${BASH_SOURCE[0]}")/.."
output=${P2P_VPN_BROWSER_OUT_DIR:-crates/p2p-vpn-browser/web/pkg}
if (( $# == 0 )); then
    package=$(nix build --no-link --no-write-lock-file --print-out-paths .#browser-sdk)
    exec bash scripts/stage-browser-sdk.sh "$package" "$output"
fi

sdk=$(nix flake archive --no-write-lock-file --json | jq -er .inputs.sdk.path)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
target=${P2P_VPN_WASM_TARGET_DIR:-${CARGO_TARGET_DIR:-target/sdk-wasm}}
CARGO_TARGET_DIR=$(realpath -m -- "$target") \
    bash "$sdk/scripts/build-browser-sdk.sh" "$scratch" --test-hooks
bash scripts/stage-browser-sdk.sh "$scratch" "$output"
