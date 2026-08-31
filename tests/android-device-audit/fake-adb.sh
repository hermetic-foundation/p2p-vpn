#!/usr/bin/env bash
set -euo pipefail

state_dir="${P2P_VPN_ANDROID_DEVICE_AUDIT_FAKE_STATE:?}"
mode="${P2P_VPN_ANDROID_DEVICE_AUDIT_FAKE_MODE:-pass}"
mkdir -p "$state_dir"
printf '%q ' "$@" >> "$state_dir/adb.log"
printf '\n' >> "$state_dir/adb.log"

if [[ "${1:-}" == devices ]]; then
  printf 'List of devices attached\nphysical-test\tdevice product:test transport_id:1\n'
  exit 0
fi

if [[ "${1:-}" == -s && "${2:-}" == physical-test ]]; then
  shift 2
else
  echo "fake adb requires the selected test serial" >&2
  exit 2
fi

if [[ "${1:-}" == get-state ]]; then
  printf 'device\n'
  exit 0
fi

if [[ "${1:-}" == install && "${2:-}" == -r ]]; then
  printf 'Success\n'
  exit 0
fi

[[ "${1:-}" == shell ]] || { echo "unsupported fake adb command: $*" >&2; exit 2; }
shift

case "${1:-} ${2:-} ${3:-}" in
  "getprop ro.product.cpu.abilist ")
    if [[ "$mode" == wrong-abi ]]; then
      printf 'x86_64\n'
    else
      printf 'arm64-v8a,armeabi-v7a\n'
    fi
    ;;
  "getprop ro.build.version.sdk ")
    printf '35\n'
    ;;
  "am start -W")
    printf 'Starting: Intent\n'
    ;;
  "am force-stop org.hermeticfoundation.p2pvpn.debug")
    printf '5252\n' > "$state_dir/pid"
    ;;
  "pidof org.hermeticfoundation.p2pvpn.debug ")
    if [[ -s "$state_dir/pid" ]]; then
      cat "$state_dir/pid"
    else
      printf '4242\n'
    fi
    ;;
  "input keyevent 223"|"input keyevent 224")
    ;;
  "dumpsys deviceidle force-idle")
    if [[ "$mode" == doze-fail ]]; then
      exit 1
    fi
    printf 'Now forced in to deep idle mode\n'
    ;;
  "dumpsys deviceidle unforce")
    printf 'Light state: ACTIVE, deep state: ACTIVE\n'
    ;;
  "dumpsys deviceidle get")
    printf 'IDLE\n'
    ;;
  "dumpsys battery ")
    count=0
    if [[ -s "$state_dir/battery-count" ]]; then
      read -r count < "$state_dir/battery-count"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$state_dir/battery-count"
    cat <<EOF
Current Battery Service state:
  status: 3
  plugged: 0
  level: $((90 - count))
  scale: 100
  temperature: 250
  Charge counter: $((3000000 - count * 1000))
EOF
    ;;
  "ping "*|"ping6 "*)
    count=1
    previous=""
    for argument in "$@"; do
      if [[ "$previous" == -c ]]; then
        count="$argument"
      fi
      previous="$argument"
    done
    printf '%s packets transmitted, %s received, 0%% packet loss\n' "$count" "$count"
    ;;
  "am broadcast --receiver-foreground")
    command=""
    previous=""
    for argument in "$@"; do
      if [[ "$previous" == command ]]; then
        command="$argument"
        break
      fi
      previous="$argument"
    done
    case "$command" in
      status)
        count=0
        if [[ -s "$state_dir/status-count" ]]; then
          read -r count < "$state_dir/status-count"
        fi
        count=$((count + 1))
        printf '%s\n' "$count" > "$state_dir/status-count"
        has_profile=true
        if [[ "$mode" == fresh-pair && ! -f "$state_dir/profile-created" ]]; then
          has_profile=false
        fi
        response="$(jq -nc \
          --argjson changes "$count" \
          --argjson has_profile "$has_profile" '
          {
            schema_version: 1,
            ok: true,
            value: {
              service_ready: true,
              snapshot: {
                has_profile: $has_profile,
                profile_stored: $has_profile,
                profile_unreadable: false,
                connected: $has_profile,
                connection_requested: $has_profile,
                always_on: false,
                lockdown: false,
                busy: false,
                network_name: (if $has_profile then "physical-test" else null end),
                hostname: (if $has_profile then "android-0123456789abcdef" else null end),
                peer_id: (if $has_profile then "12D3KooWFakeAndroidPeer" else null end),
                addresses: (if $has_profile then ["100.64.0.9/32", "fd42::9/128"] else [] end),
                runtime_generation: 2,
                underlay: {
                  kind: "wifi",
                  validated: true,
                  available_networks: 1,
                  selection_changes: $changes,
                  selected_losses: 1,
                  recoveries: 1,
                  runtime_recovery_requests: $changes,
                  runtime_recovery_failures: 0
                },
                paths: {
                  connected_peers: 1,
                  direct_udp_datagram: 0,
                  direct_quic_datagram: 0,
                  direct_quic_stream: 1,
                  direct_tcp_stream: 0,
                  relay: 0,
                  public_routing_peers: 1,
                  packet_plane_quic_sessions: 0,
                  outbound_quic_datagram_packets: 0,
                  outbound_direct_tcp_stream_packets: 20,
                  promotions_to_direct: 1
                },
                pairing: {code: null, candidate_peer: null}
              }
            }
          }
        ')"
        ;;
      diagnostics)
        response='{"schema_version":1,"ok":true,"value":{"service_ready":true,"report":{"schema_version":1,"kind":"p2p-vpn-android-diagnostics","lifecycle":{"connected":true,"always_on":false,"lockdown":false,"runtime_generation":2},"underlay":{"kind":"wifi","runtime_recovery_requests":4,"runtime_recovery_failures":0},"paths":{"connected_peers":1,"direct_quic_stream":1,"relay":0},"queue":{"queued_packets":0},"drops":{"outbound_packets":0,"inbound_packets":0},"resources":{"process_cpu_millis":2500,"total_pss_kib":52000,"private_dirty_kib":27000,"active_threads":12},"pairing":{"operation_active":false,"candidate_pending":false},"events":{"discarded":0,"items":[{"sequence":1,"since_service_start_millis":1,"name":"pairing_completed"}]},"privacy":{"identity_material":"excluded","peers":"excluded","pairing_secrets":"excluded","underlay_addresses":"excluded"}}}}'
        ;;
      create-profile)
        touch "$state_dir/profile-created"
        response="$(jq -nc --arg command "$command" \
          '{schema_version: 1, ok: true, value: {accepted: true, command: $command}}')"
        ;;
      connect|join-pairing)
        response="$(jq -nc --arg command "$command" \
          '{schema_version: 1, ok: true, value: {accepted: true, command: $command}}')"
        ;;
      *)
        echo "unsupported fake automation command: $command" >&2
        exit 2
        ;;
    esac
    encoded="$(printf '%s' "$response" | base64 --wrap=0)"
    printf 'Broadcast completed: result=-1, data="%s"\n' "$encoded"
    ;;
  *)
    echo "unsupported fake adb shell command: $*" >&2
    exit 2
    ;;
esac
