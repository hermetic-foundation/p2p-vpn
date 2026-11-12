# Application Demand And Quiet Fencing

## Recording Contract

| Event | Demand Record |
| --- | --- |
| New authorized TUN packet | Destination peer, after source/destination/size validation |
| Queue backpressure after valid admission | Still application demand; not a promise of delivery |
| Accepted inbound IP frame | Authenticated origin peer, after source/destination/size and applicable replay checks |
| Explicit application request or accepted wake | Authorized target/origin peer |
| Queued retransmission | No new activity |
| Probe, keepalive, ACK, capability, membership or DNS-cache maintenance | No new activity |
| Malformed, spoofed, unauthorized or replay-rejected packet | No new activity |

Record at the shared forwarding boundary so all packet backends and Android
use the same rule. Preserve the immutable inbound stream validation API;
recording must not require a new global lock or per-packet heap allocation.

## Per-Peer State

| Field | Purpose |
| --- | --- |
| Monotonic timestamp | Last observed application demand; never wall-clock time |
| Generation | Advances on each accepted application event, even at the same clock tick |
| Quiet bit | Permission committed by validated bilateral lifecycle negotiation |
| Ownership | Exactly one record per currently authorized forwarding peer |

The quiet bit and generation share an atomic word. An application event clears
quiet and advances the generation. A quiet grant uses compare-and-swap against
the generation that the dispatcher checked; stale permission cannot win later.

Generation exhaustion fails open for traffic and closed for quiescence: keep
forwarding, but never grant quiet using a reused or wrapped generation.

## Dispatcher And Forwarder

1. Observe actual forwarding activity before evaluating an idle request or acknowledgement.
2. Invalidate semantic idle tickets on new application generations; retain transport slots until terminal events.
3. Commit model permission only after normal authentication, scope, readiness and delivery checks.
4. Grant the forwarding quiet bit only if its observed generation still matches.
5. If activity raced the grant, remain active and preserve the newer demand.

A packet clears quiet immediately at the forwarding boundary. It need not wait
for a five-second maintenance tick or status poll. Control refreshes do not
invent a new application timestamp.

## Authorization Changes

- Preserve demand timestamps for unchanged authorized peers across harmless refreshes.
- Prune removed peers; a removed and re-added peer does not inherit an old quiet grant.
- Invalidate quiet grants on changed forwarding authority or membership incarnations.
- Keep staged/failed authorization transactions from changing live demand state.
- Each instance owns independent records; no global cross-network activity lease.

## Activation Boundary

Use the forwarding quiet bit to suppress peer probes, negotiation retries,
retention and background data-path recovery. Do not report dormant as a healthy
transport; it is a separate policy state.

These gates are not permission to advertise quiet support yet. Incoming-path
readiness, transport retirement, public-relay evidence, feature checks and
matched cellular energy measurements remain required activation gates.
