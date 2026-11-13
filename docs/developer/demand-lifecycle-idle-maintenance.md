# Idle Maintenance

## Status

Implementation contract. The demand lifecycle stays unadvertised until its
control-delivery, wake, roaming, and energy gates pass.

## Transport Policy

| Setting | Policy |
| --- | --- |
| Automatic reservations | One by default in Rust and Nix; preserve explicit limits and reservations |
| Native ping | 60-second interval; existing 20-second timeout |
| Native QUIC | Retain its compatible 5-second keepalive and 10-second idle timeout |
| Application paths | Prefer active QUIC; probes and legacy behavior change only through authenticated bilateral dormancy |
| Incoming anchor | Prefer TCP; retain QUIC-only fallback |

Pinned libp2p-quic 0.13.1 passes its keepalive directly to Quinn. Quinn-proto
0.11.16 negotiates the smaller finite idle timeout, but does not shorten the
configured keepalive to fit that timeout.

- Setting native QUIC keepalive to 60 seconds would break quiet connections to short-timeout peers.
- Raising only the local idle timeout does not override the remote timeout.
- Remove unused QUIC connections instead of weakening compatibility.
- A necessary legacy QUIC fallback can still incur five-second maintenance.
- Native ping streams do not themselves opt into Swarm keepalive accounting; retain leases through their native owner.

Source audit: `libp2p-quic/src/config.rs`, Quinn's `reset_keep_alive`, and the
native ping handler in the versions pinned by `Cargo.lock`.

## Infrastructure Retirement

| Gate | Requirement |
| --- | --- |
| Reachability | At least one verified incoming anchor |
| Discovery | Ordinary public discovery is quiet; do not provoke bootstrap redial churn |
| Identity | Current, established, non-retiring connection ID and authenticated peer |
| Initial grace | At least 60 seconds since establishment |
| Scope | Automatic infrastructure, routing-only peers, and cached bootstrap peers; not overlay members |
| Shared work | No queued/in-flight packets, pending native dials, request/reply codec work, or DHT work |

Protect:

- Native confirmed reservation connection IDs and all automatic reservation setup in progress.
- Explicitly configured relay peers, including overrides requesting redundant reservations.
- Relays used by active circuits, code pairing, or membership/file-pairing probes.
- Nodes serving circuit-relay clients; do not infer absence of server-side circuits from overlay paths.
- Primary and public-pairing DHT queries, handler streams, RPCs, negotiations, and queued events.

## Ownership

- Inspect native work owners; do not add parallel request or connection registries.
- Include disconnected requests, queued terminal notifications, and pre-decode codec work.
- Protect shared relay transports while any protocol work or native dial might still be selecting a circuit owner.
- Mark only the chosen connection ID retiring, then ask Swarm to close it.
- Preserve addresses, configured seeds, memberships, and identity records.
- Reuse the existing recovery tick; add no polling timer or maintenance packet.
- Deliberate infrastructure retirement must not clear another connection's lease or start overlay recovery.

## Validation

| Scenario | Evidence Required |
| --- | --- |
| Defaults | Minimal JSON/Nix, partial override, explicit disable, explicit redundancy, one-slot failover |
| Sparse TCP | Retained native TCP anchor accepts fresh inbound work beyond the 60-second ping interval |
| QUIC compatibility | Native defaults unchanged; demanded QUIC and QUIC-only relay fallback remain usable |
| No work | Mature surplus infrastructure closes once, with no new dial/query or metadata eviction |
| Busy | Queues, dials, queries, codec streams, and queued replies prevent pruning |
| Dependencies | Reservations, explicit relays, pairing, circuits, serving roles, and overlay peers survive |
| Exact owner | Prune spare QUIC while the TCP reservation remains ready and accepts fresh incoming control |
| Epochs | Young, stale, unestablished, already-retiring, and mismatched connections are rejected |

Connection counts and wire reductions are not battery measurements. Final
acceptance still requires matched, replicated idle cellular mAh/hour results.
