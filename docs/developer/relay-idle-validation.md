# Quiet Incoming Relay Validation

## Purpose

Test the infrastructure assumption behind [dormancy](demand-driven-lifecycle.md):
an accepted public relay reservation can remain usable without frequent
client-originated probes. This is transport evidence, not battery acceptance.

## Protocol

| Stage | Required Evidence |
| --- | --- |
| Discover | Existing bounded IPFS/libp2p relay scan; no manually chosen public server |
| Reserve | Fresh identity, plain TCP, Noise/Yamux; accepted reservation and circuit listener |
| Settle | Poll the receiver for 10 seconds; exclude setup traffic from idle totals |
| Idle | Real monotonic wait; keep polling; record raw TCP reads/writes every 5 seconds |
| Incoming | Fresh sender with no direct receiver addresses; dial only the reserved circuit |
| Verify | Authenticated sender/receiver IDs, relayed endpoints, matching bounded nonce reply |

## Traffic Policy

- No DHT, mDNS, AutoNAT, direct-path discovery, or periodic application probes on the receiver.
- Identify and ping retain inbound support; outbound intervals are one day.
- The library's initial ping and identify exchange happen during setup/settling.
- Native relay renewal is not disabled; record reservation events throughout the wait.
- Count TCP I/O below encryption/multiplexing, including remote pings and their replies.
- I/O operations are not IP packets; TCP ACKs/retransmissions are outside these counters.

## Bounds

| Resource | Limit |
| --- | --- |
| Discovery | One existing scan, 45 seconds, at most 64 candidate addresses |
| Reservation attempts | At most 8 plain-TCP candidates, serially, 20 seconds each |
| Concurrent public reservations | One |
| Idle duration | Explicit 1–3600 seconds; acceptance evidence requires at least 1800 |
| Incoming deadline | 30 seconds |
| Echo request/reply | Exactly 16 bytes each |
| Report | Small JSON; samples at 5-second intervals; no private identity material |

## Execution

```sh
# Local encrypted relay, real short wait; no external network required.
cargo test --test relay_idle local_relay_accepts_incoming_after_idle

# Controlled 8-second lease: native renewals must preserve incoming access.
cargo test --test relay_idle local_reservation_renewal_preserves_incoming

# Explicit public-network evidence, not part of the ordinary test suite.
P2P_VPN_RELAY_IDLE_SECONDS=1800 \
P2P_VPN_RELAY_IDLE_REPORT=/tmp/p2p-vpn-relay-idle.json \
cargo test --test relay_idle public_relay_accepts_incoming_after_idle \
  -- --ignored --nocapture
```

## Interpretation

- Preserve failed attempts, idle samples, elapsed durations, and connection losses.
- An accepted reservation alone does not pass; a fresh incoming exchange must succeed.
- Server-generated traffic counts against quietness even when the client initiates none.
- A same-host relay-only test does not prove cellular NAT lifetime, roaming, or Android sleep behavior.
- Keep the runtime lifecycle passive until incoming-path ownership and retirement are safe.
- Replicated cellular energy and full VPN reachability tests remain separate acceptance gates.
