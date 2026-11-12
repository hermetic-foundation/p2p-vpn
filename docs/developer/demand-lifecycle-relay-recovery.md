# Incoming Relay Recovery

## Status

Implementation contract for missing incoming paths and transport alternatives.
The production demand lifecycle remains disabled until all activation gates pass.

Transport alternatives are implemented. Demand-independent discovery remains
pending; the validation contract below is not a claim that every gate has passed.

## Separation

| Work | Owner | Idle Policy |
| --- | --- | --- |
| Ordinary mesh maintenance | Existing Kademlia maintenance batch | Suppressed when data peers are quiet |
| Incoming-path repair | Dedicated query slot within that maintenance owner | Runs only without a verified incoming path |
| Pairing, payload recovery, publication | Existing respective owners | Must not be cancelled or reset by relay repair |

An idle data plane is not evidence that an incoming path exists. Repair must
continue after relay/candidate loss without waking every dormant mesh peer.

## Budget

| Limit | Contract |
| --- | --- |
| Incoming query concurrency | One native Kademlia query |
| Query lifetime | Existing 90-second maintenance deadline |
| Burst | Three starts, at least 120 seconds apart |
| Cooldown | At least 10 minutes after each third start |
| Full native query pool | Wait without cancelling other owners or consuming a round |
| Network changes | Cancel the owned query and reset its budget; retain the existing LAN-first holdoff |
| Healthy incoming path | Cancel only incoming repair and reset for future path loss |
| Candidate capacity | Honour configured limits; explicit relay IDs do not consume automatic capacity |

- Respect disabled discovery, disabled automatic relaying, and bootstrap backoff.
- Use the node's configured/default bootstrap addresses and primary DHT protocol.
- Seed the native address book; let libp2p own bootstrap dials and query RPCs.
- Never use pairing's public DHT to bypass a private primary DHT configuration.
- Query completion, timeout, and cooldown do not reset ordinary maintenance cadence.
- Normal maintenance suppression must not cancel the incoming query.
- Native cancellation must remove queued work, not merely forget the query ID.

## Readiness

- Automatic anchors require their exact owned listener and selected connection ID.
- Native confirmation, current established epoch, and non-retiring direct transport are required.
- Explicit anchors require a current native confirmation and a live configured listener.
- Cached peer acceptance, addresses, or public AutoNAT status alone are insufficient.
- Pending setup consumes a reservation slot but is not incoming readiness proof.

## Transport Alternatives

| Item | Contract |
| --- | --- |
| Per-candidate addresses | At most four validated, deduplicated addresses |
| Initial preference | LAN first; TCP preferred within each locality |
| Fallback order | Include a QUIC alternative before redundant TCP alternatives in the same locality |
| Dial engine | One native libp2p dial with bounded addresses and concurrency one |
| Ownership | One dial connection ID and the existing 30-second setup deadline |
| Actual transport | Inspect the authenticated established endpoint, not the first requested address |

- Retain alternatives from validated discovery/Identify without inventing routing peers.
- Do not add relay-only peers to the DHT routing table just to cache their addresses.
- Public advertisements cannot introduce private, unsupported, relayed, or wrong-peer addresses.
- Retained validated LAN routes remain ahead of public alternatives.
- Native fallback must work without an already-established QUIC connection.
- Late completion after expiry or an underlay change must remain fenced.
- Accepted leases and explicit relay overrides must not be replaced during address refresh.

## Validation

| Scenario | Required Evidence |
| --- | --- |
| Quiet outage | Bounded query starts across 24 simulated hours with no payload demand |
| Suppression | Ordinary maintenance cancellation preserves incoming repair |
| Ownership | Incoming timeout/availability/network reset preserve unrelated native queries |
| Native cancellation | Owned queued completion/RPC work is purged; unrelated work survives |
| LAN first | No incoming public query before the existing holdoff expires |
| Pool pressure | No extra starts, eviction, or busy rejection loop at capacity |
| Alternatives | Address cap, source validation, locality, and transport ordering |
| Cold fallback | A real TCP refusal followed by QUIC reservation and fresh relayed control |
| End-to-end repair | Lost anchor plus empty candidates recovers without mesh payload traffic |

These tests establish recovery behavior, not cellular energy savings. The final
10x acceptance test still requires matched, replicated cellular measurements.
