#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
root="$(mktemp -d)"
trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root/bin" "$root/repo/src"
touch "$root/repo/Cargo.toml"

cat > "$root/bin/fake-tool" <<'TOOL'
#!/usr/bin/env bash
set -euo pipefail
case "${0##*/}" in
  ps)
    if [[ "$*" == *args* ]]; then
      echo '123 1 S p2p-vpn p2p-vpn pair join PAIRING-CODE-MUST-NOT-BE-CAPTURED'
    else
      echo '123 1 S p2p-vpn'
    fi
    ;;
  cargo) echo '{}' ;;
  *) echo "fixture ${0##*/}" ;;
esac
TOOL
chmod +x "$root/bin/fake-tool"
for tool in ps p2p-vpn nix cargo rustc rustfmt clippy-driver jj git ip ss unshare uname; do
  ln -s fake-tool "$root/bin/$tool"
done

cd "$root/repo"
PATH="$root/bin:$PATH" \
  P2P_VPN_DEBUG_BUNDLE_DIR="$root/artifacts" \
  P2P_VPN_DEBUG_BUNDLE_CONTROL_SOCKET='' \
  P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST=0 \
  bash "$repo/scripts/debug-bundle.sh" > "$root/stdout" 2> "$root/stderr"

test -s "$root/artifacts/debug-summary.json"
grep -Fq '123 1 S p2p-vpn' "$root/artifacts/debug-host.txt"
if grep -RFq 'PAIRING-CODE-MUST-NOT-BE-CAPTURED' "$root/artifacts"; then
  echo 'debug bundle captured a process argument containing a pairing code' >&2
  exit 1
fi
jq -e '.schema_version == 1' "$root/artifacts/debug-summary.json" >/dev/null
echo 'debug bundle process-argument privacy passed'
