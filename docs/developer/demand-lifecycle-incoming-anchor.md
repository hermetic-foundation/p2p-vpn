# Incoming Anchor Ownership

## Status

Implementation contract. The peer lifecycle remains unadvertised until incoming
ownership, event-driven control delivery, and wake/recovery tests are complete.

The [public relay test](relay-idle-2026-11-12.md) proves one TCP reservation
survived 30 minutes and accepted a fresh sender. It is not cellular energy evidence.

## Requirements

| Item | Contract |
| --- | --- |
| Automatic footprint | One accepted reservation by default; no required new configuration |
| Preferred underlay | Plain TCP to the relay; active direct overlay QUIC remains preferred |
| Explicit overrides | Preserve configured reservations and explicit automatic reservation limits |
| Recovery | Independent of peer payload demand; bounded attempts, deadlines, backoff, alternate candidates |
| Reservation lifetime | Native relay client owns renewal and stop/circuit handling |
| Quiet readiness | A confirmed native reservation, owned live listener, and current non-retiring underlay connection |
| Identity/isolation | Bind actual connection ID to the authenticated relay peer in this runtime/underlay epoch |

## Transport Ambiguity

Libp2p relay 0.21.1 currently reuses the first connection to a relay for a new
reservation. A TCP multiaddr or advertised TCP circuit does not establish that
the reservation itself uses TCP; an existing QUIC connection may be reused.

1. Expose confirmed reservation connection IDs from the native client.
2. Allow an existing authenticated connection to be preferred for new reservations.
3. Select/dial the intended transport before issuing the reservation request.
4. Validate readiness against the actual established endpoint, not advertised addresses.

The preference changes only future native connection selection. It must not
move or reset an existing reservation, circuit, or renewal. No new relay wire
protocol, claimed peer identity, or independent reservation ledger is introduced.

## Acquisition

| State | Action | Deadline/Exit |
| --- | --- | --- |
| Candidate | Rank reachable LAN addresses first, then prefer TCP within locality | Existing bounded candidate pool |
| Connecting | Reuse a usable matching direct connection or explicitly dial it | Existing pending setup deadline and native dial timeout |
| Reserving | Prefer the chosen connection, then install the circuit listener | Native acceptance plus owned live listener |
| Available | Retain actual reservation connection and publish valid circuit addresses | Native renewal; connection/listener loss invalidates readiness |
| Recovering | Release failed ownership, rotate candidates, retain bounded backoff | Retry incoming path even while all data peers are dormant |

- Pending explicit dials retain their connection/underlay epoch ownership.
- Late dialing/establishment events cannot resurrect an expired or old-network attempt.
- QUIC-only candidates remain usable fallbacks; diagnostics must not label them quiet TCP anchors.
- A replacement becomes usable before the old usable anchor is retired where capacity permits.

## Idle Maintenance

- Target a sparse native transport heartbeat, initially 60 seconds with the existing 20-second timeout.
- Continue answering incoming pings and native relay lease refreshes.
- Active overlay path probes remain responsible for demanded data-path liveness.
- A silent idle-anchor failure can therefore take about 80 seconds to detect.
- Underlay-change events invalidate old ownership immediately and start existing LAN-first recovery.
- Keep no extra QUIC/DHT connections merely because an address is cached.
- Never prune a live reservation, active circuit dependency, pairing path, or outstanding wire/query work.

This changes maintenance cadence, not authentication or the libp2p ping protocol.
Older peers retain legacy data-path behavior; no peer is declared dormant without
the bilateral lifecycle exchange.

## Readiness And Activation

- A peer-only reservation event is insufficient evidence of the owning transport.
- A stale listener, closing connection, or old underlay epoch is not ready.
- Cached capabilities, external addresses, and a public AutoNAT result alone are not incoming proof.
- Losing the anchor does not revoke dormant memberships or start a full peer recovery storm.
- Do not enable lifecycle advertisement before control updates can reach dormant disconnected members.

## Verification

| Layer | Required Evidence |
| --- | --- |
| Native client | Existing QUIC plus TCP connections; preference chooses TCP without closing QUIC; confirmed ID matches actual owner |
| Ownership | Unconfirmed, expired, closing, wrong-peer, and stale-epoch connections are rejected |
| Acquisition | Explicit transport dial before reservation, bounded timeout, cancellation, retry and alternate candidates |
| Compatibility | Default native relay behavior unchanged unless preference is requested; explicit user settings preserved |
| Recovery | Reservation/listener loss repairs automatically without application data |
| Functional | Fresh incoming payload, active QUIC, relay fallback, membership/DNS/revocation, and roaming |
| Energy | Fresh paired cellular measurements; connection/byte counts are not a 10x battery result |
