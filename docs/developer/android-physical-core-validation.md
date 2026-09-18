# Android Physical Core Validation

## Result

The physical Android core audit passed on 2026-09-17/18.
[Portable results](android-physical-core-results.json) retain the acceptance facts.

| Item | Result |
| --- | --- |
| Source | `29d050179627` |
| Audit reporting fix | `5c90c89d8928` |
| Device | Physical arm64-v8a, Android API 37 |
| Scenario | LAN to cellular to LAN |
| Outcome | Passed; proof eligible |
| Runtime restarts during movement | None |

## Mobility

| Checkpoint | Traffic | Convergence | Runtime |
| --- | --- | --- | --- |
| LAN baseline | 20/20 dual-stack packets | 47.342 seconds | Generation 1 |
| Cellular | 20/20 dual-stack packets | 63.174 seconds | Generation preserved |
| LAN return | 20/20 dual-stack packets | 17.526 seconds | Generation preserved |

The final Linux-side state selected `direct_quic_datagram` to an endpoint on the
phone's current LAN. No healthy relay path was selected.

## Lifecycle

| Check | Result |
| --- | --- |
| Forced Doze | Passed after 300 seconds in deep idle |
| Doze traffic | 20/20 dual-stack packets |
| Sustained runtime | 1,800 seconds |
| Process/service recreation | Passed; identity and traffic restored |
| In-place APK update | Passed; identity and traffic restored |
| Encrypted profile | Byte-identical before and after |
| Membership state | Byte-identical before and after |

## Endurance

| Metric | Result |
| --- | --- |
| Samples | 24 at 60-second intervals |
| Packets | 479/480 |
| Loss | 0.20%; limit 1.00% |
| Failed samples | One isolated IPv4 datagram |
| Final queue | Empty |
| Runtime packet drops | Zero recorded |
| Final active threads | 11 |
| Final PSS | 109,857 KiB |

The isolated loss recovered on the next packet. It did not trigger a route outage,
runtime restart, queue buildup or recorded application drop.

## Artifacts

| Artifact | SHA-256 |
| --- | --- |
| Debug APK | `c6d5db42b79a830ab7e58ed5579034622ecbd8b41f94337d7b8c17def34bfa16` |
| Raw evidence | `39055e09b91a542ceeab7f68b781f01e4f25148866db7583c62eb24204c7e7c7` |
| Encrypted profile, before and after | `ffbfa97f372ce441fddff8a7567ca098955b945744200c00f3ae834afaf4c970` |
| Membership state, before and after | `8d56b15224c89372be8c2bb25a819b3c91e690e0e0f7e9ce2d4dbe1312f6d7bf` |

The raw evidence remains at
`/tmp/p2p-vpn-android-core-20260918-final/evidence.json` on the validation host.
It is 63,725 bytes and excludes device, peer, overlay and underlay identifiers.

## Verification

| Command or check | Result |
| --- | --- |
| `cargo test -p p2p-vpn --lib` | 1,197 passed; 8 ignored |
| Runtime scheduler regressions | 2 passed |
| `cargo check -p p2p-vpn --all-targets` | Passed |
| Android debug APK Nix build | Passed |
| Android JVM unit tests | Passed |
| Android lint | Passed |
| `android-device-audit-structure` | Passed |
| ShellCheck for physical audit | Passed |

## Limits

- The core scenario used cellular directly. It did not add an upstream hotspot VPN.
- USB power remained attached. Battery deltas are not unplugged energy evidence.
- The raw report used the legacy generated-hostname validator and labeled the
  device-derived hostname as unassigned. `5c90c89d8928` corrects that reporting check.
- This validates one physical Android model and one carrier/LAN environment.
