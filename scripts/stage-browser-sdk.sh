#!/usr/bin/env bash
set -euo pipefail

if (( $# < 1 || $# > 2 )); then
    printf 'usage: bash scripts/stage-browser-sdk.sh SDK_PKG [OUT_DIR]\n' >&2
    exit 2
fi

sdk=$(realpath -e -- "$1")
repo=$(realpath -e -- "$(dirname -- "${BASH_SOURCE[0]}")/..")
output=${2:-"$repo/crates/p2p-vpn-browser/web/pkg"}
artifacts=(
    p2p_vpn_browser.js
    p2p_vpn_browser.d.ts
    p2p_vpn_browser_bg.wasm
    p2p_vpn_browser_bg.wasm.d.ts
)
for file in "${artifacts[@]}"; do
    if [[ ! -s $sdk/$file ]]; then
        printf 'Missing browser SDK artifact: %s\n' "$sdk/$file" >&2
        exit 1
    fi
done
mkdir -p -- "$output"
for file in "${artifacts[@]}"; do
    install -m644 -- "$sdk/$file" "$output/$file"
done
install -m644 -- "$repo/assets/p2p-vpn-logo.svg" "$output/logo.svg"
