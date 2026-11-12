# Public Relay Idle Evidence

## Result

**Transport prerequisite passed. Battery acceptance remains unmeasured.**

| Measurement | Result |
| --- | --- |
| Date | 2026-11-12 |
| Infrastructure | Automatically discovered public libp2p relay; plain TCP, Noise/Yamux |
| Receiver idle interval | 1,800.004 seconds |
| Idle TCP bytes received / sent | 3,849 / 3,752 |
| Successful read / write operations | 124 / 124; these are not packet or modem-wakeup counts |
| Fresh incoming exchange | Matching 16-byte nonce response in 3.178 seconds |
| Path verification | Both authenticated endpoints reported `/p2p-circuit` |
| Client maintenance | Ping and identify intervals one day; no application probes, DHT, mDNS, or AutoNAT on the receiver |
| Public reservation renewal | None occurred during this window |
| Local renewal regression | Native renewal of an 8-second lease preserved fresh incoming access |

See the [test protocol](relay-idle-validation.md) and
[raw successful report](relay-idle-2026-11-12/public-tcp-30m.json).

## What This Establishes

- A real public TCP reservation survived 30 minutes without client application probes.
- A sender created after the idle interval reached the receiver through that reservation.
- No direct receiver addresses, mDNS discovery, or direct-path upgrade were available to the sender.
- Low-volume transport maintenance still occurred; this was not a zero-traffic connection.
- Essential native relay renewal remains enabled and has separate local regression coverage.

## Limits

- Both test clients ran on this host. This is not an Android, cellular, carrier-NAT, or roaming test.
- The payload was a test echo, not the production VPN's lifecycle wake or TUN traffic.
- One accepted public relay does not establish availability or quietness across all public relays.
- The run did not reach the public reservation's renewal deadline or establish indefinite reachability.
- Counters are below Noise/Yamux but above TCP ACKs/retransmissions; they do not measure radio energy.
- No app-attributed mAh/hour, modem power, or 10x savings was measured in this test.

## Preserved Attempts

| Artifact | Classification |
| --- | --- |
| [Port conflict](relay-idle-2026-11-12/setup-port-conflict.json) | Invalid setup: scanner default port was occupied; no idle test occurred |
| [Transport selection](relay-idle-2026-11-12/invalid-transport-selection.json) | Invalid harness: selected an advertised QUIC circuit for a TCP-only sender |
| [Interruption note](relay-idle-2026-11-12/interruption.json) | Explains why the partial transport-selection run was stopped |
| [Completed TCP run](relay-idle-2026-11-12/public-tcp-30m.json) | Valid transport-only observation; includes refused/unreachable candidates |
| [Manifest](relay-idle-2026-11-12/manifest.json) | Raw artifact digests and measured executable digest |

The measured executable was a development snapshot before local-only renewal
coverage and lint cleanup were added. Its digest is retained; do not describe
it as a byte-identical build of the later published harness commit.

## Runtime Milestone

| Item | Status |
| --- | --- |
| Harness | Published as `e4552411` |
| Owned datagram retirement | Published as `8521b35b` |
| Full workspace/all-target regressions | 1,936 passed; zero failures; 49 ignored |
| Strict Clippy groups / changed-file formatting | Passed; unrelated advisory warnings remain |
| Nix package source | Evaluates; staged runtime source digests match |
| Android / installed Linux services | Unchanged by this milestone |
| Production lifecycle advertisement | Still disabled |

## Remaining Gates

1. Implement real incoming-anchor readiness and safe outstanding-control/stream retirement.
2. Preserve validated dormant metadata, essential relay dependencies, and bounded automatic anchor recovery.
3. Activate the lifecycle and verify first inbound/outbound VPN payloads, compatibility, and network changes.
4. Run fresh replicated paired/reversed-order cellular energy comparisons against the original build.
5. Require at least 90% lower idle app-attributed mAh/hour, corroborated by modem energy, without lost features.
