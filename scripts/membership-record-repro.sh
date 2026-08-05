#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f Cargo.toml || ! -d src ]]; then
  echo "membership-record-repro must be run from the p2p-vpn repository root" >&2
  exit 2
fi

artifact_dir="${P2P_VPN_MEMBERSHIP_REPRO_DIR:-}"
if [[ -z "$artifact_dir" ]]; then
  artifact_dir="$(mktemp -d -t p2p-vpn-membership-record-repro.XXXXXXXX)"
fi
mkdir -p "$artifact_dir"

network="${P2P_VPN_MEMBERSHIP_REPRO_NETWORK:-lab-record-repro}"
route_grant="${P2P_VPN_MEMBERSHIP_REPRO_ROUTE_GRANT:-10.77.0.0/24,100}"
membership_epoch="${P2P_VPN_MEMBERSHIP_REPRO_EPOCH:-1}"
sequence="${P2P_VPN_MEMBERSHIP_REPRO_SEQUENCE:-1}"
expires_at="${P2P_VPN_MEMBERSHIP_REPRO_EXPIRES_AT_UNIX_SECONDS:-}"

issuer_config="$artifact_dir/issuer.json"
member_config="$artifact_dir/member.json"
member_identity="$artifact_dir/member.identity.json"
member_record="$artifact_dir/member.record.json"
issuer_installed_config="$artifact_dir/issuer.with-member-record.json"
commands="$artifact_dir/repro-commands.sh"
metadata="$artifact_dir/repro-metadata.txt"
summary="$artifact_dir/repro-summary.txt"
summary_json="$artifact_dir/repro-summary.json"

command_metadata() {
  local label="$1"
  shift
  {
    echo
    echo "[$label]"
    if output="$("$@" 2>&1)"; then
      echo "status: 0"
      echo "stdout:"
      printf '%s\n' "$output"
      echo "stderr:"
    else
      local status="$?"
      echo "status: $status"
      echo "stdout:"
      echo "stderr:"
      printf '%s\n' "$output"
    fi
  } >>"$metadata"
}

run_p2p() {
  if [[ -n "${P2P_VPN_BIN:-}" ]]; then
    "$P2P_VPN_BIN" "$@"
  else
    nix develop -c cargo run --quiet -- "$@"
  fi
}

record_issue_args=(
  membership-record-issue
  --issuer-config "$issuer_config"
  --member-identity "$member_identity"
  --output "$member_record"
  --membership-epoch "$membership_epoch"
  --sequence "$sequence"
  --route-grant "$route_grant"
  --force
)

if [[ -n "$expires_at" ]]; then
  record_issue_args+=(--expires-at-unix-seconds "$expires_at")
fi

{
  echo "#!/usr/bin/env bash"
  echo "set -euo pipefail"
  echo
  printf 'export P2P_VPN_MEMBERSHIP_REPRO_DIR=%q\n' "$artifact_dir"
  printf 'export P2P_VPN_MEMBERSHIP_REPRO_NETWORK=%q\n' "$network"
  printf 'export P2P_VPN_MEMBERSHIP_REPRO_ROUTE_GRANT=%q\n' "$route_grant"
  printf 'export P2P_VPN_MEMBERSHIP_REPRO_EPOCH=%q\n' "$membership_epoch"
  printf 'export P2P_VPN_MEMBERSHIP_REPRO_SEQUENCE=%q\n' "$sequence"
  printf 'export P2P_VPN_MEMBERSHIP_REPRO_EXPIRES_AT_UNIX_SECONDS=%q\n' "$expires_at"
  echo
  echo "scripts/membership-record-repro.sh"
} >"$commands"
chmod +x "$commands"

{
  echo "membership_record_repro_metadata_version=1"
  echo "artifact_dir=$artifact_dir"
  echo "network=$network"
  echo "route_grant=$route_grant"
  echo "membership_epoch=$membership_epoch"
  echo "sequence=$sequence"
  echo "expires_at_unix_seconds=${expires_at:-none}"
  echo "p2p_vpn_bin=${P2P_VPN_BIN:-nix develop -c cargo run --quiet --}"
} >"$metadata"
command_metadata "git rev-parse HEAD" git rev-parse HEAD
command_metadata "git status --short" git status --short
command_metadata "uname -a" uname -a

run_p2p init-config \
  --network "$network" \
  --output "$issuer_config" \
  --listen-address /ip4/127.0.0.1/tcp/0 \
  --disable-mdns \
  --disable-kademlia \
  --disable-dcutr \
  --disable-autonat \
  --force

run_p2p init-config \
  --network "$network" \
  --output "$member_config" \
  --listen-address /ip4/127.0.0.1/tcp/0 \
  --local-route "$route_grant" \
  --disable-mdns \
  --disable-kademlia \
  --disable-dcutr \
  --disable-autonat \
  --force

run_p2p identity-public \
  --config "$member_config" \
  --output "$member_identity" \
  --force

run_p2p "${record_issue_args[@]}"

verify_output="$(run_p2p membership-record-verify --input "$member_record" --network "$network")"
printf '%s\n' "$verify_output" >"$artifact_dir/membership-record-verify.txt"

install_output="$(run_p2p membership-record-install \
  --config "$issuer_config" \
  --record "$member_record" \
  --output "$issuer_installed_config" \
  --force)"
printf '%s\n' "$install_output" >"$artifact_dir/membership-record-install.txt"

{
  echo "membership record repro: ok"
  echo "artifact_dir=$artifact_dir"
  echo "network=$network"
  echo "route_grant=$route_grant"
  echo "membership_epoch=$membership_epoch"
  echo "sequence=$sequence"
  echo "issuer_config=$issuer_config"
  echo "issuer_installed_config=$issuer_installed_config"
  echo "member_config=$member_config"
  echo "member_identity=$member_identity"
  echo "member_record=$member_record"
  echo "verify_output=$artifact_dir/membership-record-verify.txt"
  echo "install_output=$artifact_dir/membership-record-install.txt"
  echo "replay_commands=$commands"
  echo "metadata=$metadata"
  echo "summary_json=$summary_json"
} >"$summary"

jq -n \
  --arg artifact_dir "$artifact_dir" \
  --arg network "$network" \
  --arg route_grant "$route_grant" \
  --arg membership_epoch "$membership_epoch" \
  --arg sequence "$sequence" \
  --arg expires_at_unix_seconds "$expires_at" \
  --arg issuer_config "$issuer_config" \
  --arg issuer_installed_config "$issuer_installed_config" \
  --arg member_config "$member_config" \
  --arg member_identity "$member_identity" \
  --arg member_record "$member_record" \
  --arg verify_output "$artifact_dir/membership-record-verify.txt" \
  --arg install_output "$artifact_dir/membership-record-install.txt" \
  --arg replay_commands "$commands" \
  --arg metadata "$metadata" \
  '{
    schema_version: 1,
    artifact_dir: $artifact_dir,
    network: $network,
    route_grant: $route_grant,
    membership_epoch: ($membership_epoch | tonumber),
    sequence: ($sequence | tonumber),
    expires_at_unix_seconds: (if $expires_at_unix_seconds == "" then null else ($expires_at_unix_seconds | tonumber) end),
    artifacts: {
      issuer_config: $issuer_config,
      issuer_installed_config: $issuer_installed_config,
      member_config: $member_config,
      member_identity: $member_identity,
      member_record: $member_record,
      verify_output: $verify_output,
      install_output: $install_output,
      replay_commands: $replay_commands,
      metadata: $metadata
    }
  }' >"$summary_json"

cat "$summary"
