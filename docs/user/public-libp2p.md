# Public libp2p/IPFS Reachability

Public libp2p/IPFS infrastructure can help find paths.

It must not be treated as VPN membership or route authority.

## What Public Infrastructure Can Do

| Capability | Supported |
| --- | --- |
| Bootstrap into public routing | yes |
| Discover relay-hop candidates | partial |
| Reserve usable public relays | depends on relay policy |
| Carry relayed fallback traffic | yes, when relay policy allows it |
| Attempt DCUtR hole punching | topology-dependent |
| Authorize VPN routes | no |
| Authorize VPN membership | no |

## Connection Roles

Public routing connections stay outside the VPN data plane.

| Role | Admission | Authority |
| --- | --- | --- |
| Public routing | Exact configured Kademlia protocol | DHT requests only |
| Relay infrastructure | Relay-hop protocol or explicit config | Relay service only |
| Pairing probe | Active pairing session | Pairing protocol only |
| Overlay member | Valid static or signed membership | Approved routes and packets |

The daemon retains at most 32 identified public routing peers per instance.

Unknown peers have 30 seconds to identify or present membership. Invalid
overlay capability attempts are disconnected and quarantined with backoff.

### Returning To LAN

After a direct LAN connection returns, packet negotiation prefers endpoints on
that LAN when both peers already advertise them. A public-looking address does
not override that peer-specific preference merely because it is public-looking.

- Existing discovery and dial cooldowns can delay promotion.
- Without a healthy direct LAN connection or mutually advertised endpoints, existing endpoint selection remains in effect.
- Endpoint signatures, peer authorization, and packet-path health checks remain required.

## Public DHT Resource Policy

Public IPFS Kademlia always runs in client mode.

It can issue lookups and publish records. It does not accept public DHT
server duties or advertise itself as a Kademlia server.

| Control | Behavior |
| --- | --- |
| Maintenance cadence | At most once every two minutes. |
| Query overlap | A second cycle cannot start while one is active. |
| AutoNAT-triggered relay lookup | Shares the two-minute cadence; pauses when acquisition is disabled, candidates or reservations are sufficient, or query capacity is full. |
| Query timeout | Stale cycle queries are canceled after 90 seconds. |
| Healthy overlay | Ordinary lookup cycles stop; signed address refresh continues. |
| Address changes | Coalesced into one pending update; at most one event-driven publication every five seconds. |
| Signed address freshness | Re-signed every 15 minutes, including while peers are healthy; the 30-minute signed lifetime is unchanged. |
| Offline peer recovery | Starts at 10 seconds, then backs off to five minutes. |
| Routing addresses | At most 64 per peer, each at most 2,048 encoded bytes. |
| Configured routing seeds | Protected from address churn; count toward the same limit. |
| Query candidates | At most 256 identities per query phase, including failed candidates. |
| Query address storage | At most 256 KiB of encoded addresses per phase, with the same per-peer limits. |
| Retained queries | At most 32 per DHT, including finished queries awaiting retirement. |
| Query input | At most 256 KiB for a key and record value together; oversized input is rejected. |
| Query result bookkeeping | At most 256 stored acknowledgement or cache-candidate peers per query; required quorum is unchanged. |
| Provider publication addresses | At most 64 per query, each at most 2,048 encoded bytes. |
| RPCs awaiting a connection | Per DHT: at most 256 requests and 1 MiB of retained payload; retired with their queries. |
| Library background jobs | One new query per poll, only below two existing queries; provider and record jobs share the allowance. |
| Background job storage | Bounded key batches, not full-record snapshots; at most 4 MiB of retained key payload per DHT. |
| Unsent DHT actions and intermediate results | Per DHT: 512 entries and 4 MiB of charged payload; terminal query results bypass this queue. |
| Waiting DHT requests | Per connection: at most 64 requests and 256 KiB of retained payload data; queued requests expire after ten seconds. |
| Inbound DHT streams | At most 32 per connection; idle or stalled requests expire after ten seconds by default. |
| Aggregate routing storage | Per DHT: 512 retained entry versions and 2 MiB of encoded address buffers, including pending entries and routing snapshots. |

Routing-address limits apply to both DHTs and standalone code pairing, including internally learned addresses.
At capacity, unprotected addresses rotate while preserving LAN and relay alternatives
where possible. These limits do not authorize peers or change minimal configuration.

Query limits also apply to standalone code pairing. Excess candidates and
addresses are ignored; unusually large searches can return fewer results.
The retained-query cap applies independently to each DHT. Neither it nor the
per-query limits establish a total process-memory or connection limit.

When the query pool is full, discovery and publication retry later. Pending
address updates retain the latest state, and pairing publication deferrals do
not consume the attempt budget. Bootstrap diagnostics report rejected starts.

Oversized input reports `query_input_too_large`, separately from temporary
`query_capacity` exhaustion. An oversized address snapshot waits for a new
address update instead of repeatedly retrying the same invalid publication.
These limits require no additional JSON or Nix settings.
The input ceiling does not increase existing wire-message or local-store limits.

Background publication reads current stored values as capacity becomes available.
Deleted or expired pending work is discarded. A full replication skip-hint map
may cause an extra normal replication; it does not drop stored records.

A full DHT request queue rejects new work without closing the connection used
by VPN traffic. Rejection reporting is bounded too; extreme overload may wait
for the query deadline instead of reporting every rejection immediately.
Query deadlines wake automatically, even when connections are idle. A busy inbound
event queue cannot indefinitely postpone query retirement.

Queue overload can discard intermediate lookup results and inbound responses.
Dropped progress does not count as a delivered record/provider result. Final query
results remain deliverable, and handler-mode changes coalesce to the latest mode.

Inbound saturation replaces reusable idle streams in place or rejects new streams.
Stalled inbound requests expire even if no response was admitted to the DHT queue.
These limits retire individual DHT streams, not the shared VPN connection.

AutoNAT relay lookups retain their own cleanup even when periodic DHT discovery
is disabled. Completion or timeout releases lookup ownership, allowing later
relay discovery without restarting the daemon.

Repeated reachability changes do not bypass the lookup cooldown. A deferred
lookup can start on a later eligible event after capacity is released; it does
not evict unrelated discovery or pairing work to make room.

Automatic relay candidates are removed after two unsuccessful reservation
attempts, including synchronous listener errors. The default retry interval is
30 seconds. This frees candidate capacity; it does not revoke membership or
change explicitly configured relay reservations.

Configured bootstrap seeds are protected from routing-address rotation and
whole-peer bucket replacement. Protection does not increase bucket capacity;
a full bucket of protected seeds rejects additional peers.

Old routing snapshots remain charged until released. At capacity, new entries
are rejected until space is available; updates can replace unprotected alternatives.
Updates that cannot fit leave existing routes intact; no extra setup is needed.
Routing notifications share the limit and may be dropped under overload.

The five-second scheduler does not launch a DHT batch on every tick.
Changed local addresses still receive a bounded publication while ordinary
maintenance rests. New changes replace stale publication work with the latest
address snapshot; network delivery is not guaranteed within five seconds.

## Code Pairing Over Public Paths

The default code workflow can use public routing and relays.

It does not require a public peer to know the pairing code.

| Step | Public infrastructure sees |
| --- | --- |
| Inviter advertisement | A network-scoped derived locator. |
| Joiner lookup | The same derived locator. |
| Relay transport | Encrypted libp2p traffic between peer identities. |
| Approval | Local daemon RPC only. |
| Membership grant | End-to-end authenticated pairing exchange. |

Start with normal defaults on both hosts:

```sh
sudo p2p-vpn pair open --instance lab
sudo p2p-vpn pair join CODE --instance lab --no-wait
```

Inspect the selected discovery and transport path:

```sh
sudo p2p-vpn pair status OPERATION --instance lab
```

Expected public fields include `discovery: Relay` or public lookup counters.

Relay availability still depends on each public relay's reservation policy.

## Create A Public Profile Config

```sh
nix run .# -- init-config \
  --output public.json \
  --public-ipfs-profile \
  --force
```

This profile:

| Setting | Value |
| --- | --- |
| Kademlia protocol | `/ipfs/kad/1.0.0` |
| Public bootstrap peers | enabled |
| mDNS | disabled |
| Provider advertisement | disabled |
| AutoNAT | enabled |
| DCUtR | enabled |

## Check Bootstrap Reachability

```sh
nix run .# -- bootstrap-check \
  --config public.json \
  --timeout-seconds 45 \
  --require-autonat-status \
  --write-report bootstrap-check.json \
  --force
```

## Scan For Relay Candidates

```sh
nix run .# -- relay-scan \
  --ipfs-bootstrap-peers \
  --timeout-seconds 30 \
  --write-candidates public-relay-candidates.txt
```

## Validate Relay Candidates

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-report public-relay-check.json \
  --timeout-seconds 45
```

Validate reservation acceptance:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --require-relay-reservation \
  --max-validation-candidates 4 \
  --write-report public-relay-reservation.json \
  --timeout-seconds 45
```

Use this before discovery-only public pairing.

It checks whether the inviter can reserve the relay.

Validate DCUtR when the topology should allow hole punching:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --require-dcutr-success \
  --max-validation-candidates 4 \
  --write-report public-relay-dcutr.json \
  --timeout-seconds 45
```

## Use A Validated Relay

Generate a relay-assisted config:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-config public-relay.json \
  --timeout-seconds 45
```

The relay is added as infrastructure.

It is not added to `peers[]`.

## Generate Minimal Two-Host Configs

Use this for a mobile LAN-to-hotspot test:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-host-a-config host-a.json \
  --write-host-b-config host-b.json \
  --timeout-seconds 45 \
  --force
```

The generated configs use:

| Setting | Value |
| --- | --- |
| Interface | `pv0` |
| LAN discovery | default mDNS |
| Public routing | default IPFS-compatible Kademlia |
| Provider ads | default enabled |
| Relay fallback | automatic relay candidates |
| Peer addresses | omitted |
| Relay reservations | omitted |
| Bootstrap peers | omitted from JSON; defaults apply at runtime |

This is the normal mobile profile.

The selected relay remains in the relay-check report.

It is not written into the host configs.

To force relay-only testing, disable direct listeners and mDNS in a copy.

## Move Between Networks

Use the same generated configs for every phase.

Do not add peer addresses, relay routes, or manual OS routes between phases.

| Phase | Host Placement | Required Result |
| --- | --- | --- |
| Baseline | Both hosts on LAN | Overlay ping succeeds. |
| Split | One host on hotspot or VPN | Overlay ping recovers through relay or direct public path. |
| Return | Both hosts back on LAN | Overlay ping succeeds again. |

For each move:

1. Start the Host A and Host B scripts once on LAN.
2. Keep both daemons running.
3. Move one host between LAN, hotspot, and LAN return.
4. Check overlay ping after each move.

Expected result:

| Phase | Expected Result |
| --- | --- |
| LAN baseline | Overlay ping succeeds. |
| Public split | Overlay ping recovers through relay or direct public path. |
| LAN return | Overlay ping succeeds again. |

For reproducible proof capture, use
[developer testing](../developer/testing.md).

## No-Route Backoff

Default public IPFS bootstrap peers are retried with backoff when the OS reports
`network unreachable` or `host unreachable`.

| Item | Behavior |
| --- | --- |
| First delay | 30 seconds |
| Maximum delay | 10 minutes |
| Reset | Successful connection to a default public bootstrap peer |

This only restrains public bootstrap retries.

LAN discovery, configured peers, discovered relay paths, and Kademlia record
lookups continue during the backoff window.

## Address Scope

Public peers sometimes advertise private or unresolved DNS addresses.
p2p-vpn accepts only globally routable literal IPs from those records.

| Source | Private or unresolved DNS behavior |
| --- | --- |
| Public Kademlia and unadmitted Identify | Rejected |
| Static configuration | Accepted |
| Local mDNS | Accepted |
| Established secure endpoint | Accepted |
| Admitted member record or Identify | Accepted |

Check the rejection counter:

```sh
sudo p2p-vpn daemon-status --socket /run/p2p-vpn/control.sock \
  | rg '^public_discovery_unverified_addresses_rejected '
```

## Interpret Results

| Field | Meaning |
| --- | --- |
| `relay_reservation` | Reservation setup failed or timed out. |
| `relayed_peer_circuit` | Relay path did not connect to target. |
| `dcutr_success` | Hole punching did not complete. |
| `none` | Candidate passed requested checks. |

Inspect active public routing transport:

```sh
sudo p2p-vpn daemon-status --socket /run/p2p-vpn/control.sock \
  | rg '^public_routing_peers '
```

This count does not include VPN members or grant route authority.

### Discovery Resource Counters

Both `daemon-status` and `daemon-state` include numeric DHT resource fields:

```sh
sudo p2p-vpn daemon-status --socket /run/p2p-vpn/control.sock \
  | rg '^kad_(primary|pairing)_'
```

| Field Suffix | Interpretation |
| --- | --- |
| `present` | `1` means this DHT exists. `kad_pairing_present 0` means there is no separate pairing DHT. |
| `query_pool_retained` | Includes finished queries still awaiting cleanup, not just active lookups. |
| `query_phases_admitted`, `query_phases_retired` | Compare increments to see whether new work also retires. One operation can have several phases. |
| `dial_intents_*` | Kademlia's queued dialing requests, not all socket attempts or established connections. |
| `handler_pending_requests`, `handler_pending_bytes` | Pending work summed over live Kademlia connection handlers. |
| `handler_peak_*` | Largest reported value on any single handler since this DHT started. |

Counters reset when the DHT restarts. Periodic maintenance is normal; nonzero
counters or retained routing entries alone do not establish a connection storm.
These fields add no configuration requirements or network authority.

### Recovery Owners

The same commands expose `app_*` fields for work retained by the VPN runtime,
separately from the DHT query pool. An empty DHT pool does not necessarily mean
all application work has been cleaned up.

```sh
sudo p2p-vpn daemon-state --socket /run/p2p-vpn/control.sock | rg '^app_'
```

| Field Group | Interpretation |
| --- | --- |
| `maintenance_*`, `address_publication_*` | Ordinary discovery and signed-address publication have independent owners. |
| `recovery_queries`, `recovery_query_*` | Pending targeted queries, their oldest age, and retained retry cooldowns. |
| `discovered_recovery_addresses_*` | Recovery-cache entries and quarantine; not the DHT's routing table. |
| `recovery_dial_*`, `public_discovery_*` | Retained dial targets and remaining retry delays. |
| `connection_attempts_pending`, `connections_retiring` | Transport attempts and connections awaiting retirement. |
| `packet_hello*`, `packet_responder*`, `packet_quic_connection_*` | Packet-handshake and QUIC connection task ownership. |

Times are floored milliseconds. Zero remaining delay means due or absent;
check the associated count or scheduled flag. A retained cooldown is not an
active attempt. These snapshots observe state without starting recovery work.

## More References

Strict checks and deeper debugging guides live in:

| Document | Contents |
| --- | --- |
| [Developer Testing](../developer/testing.md) | VM, namespace, relay, and two-host proof commands. |
| [Public Bootstrap Smoke](../developer/public-bootstrap-smoke.md) | Public reachability notes. |
| [Feature Matrix](../developer/feature-matrix.md) | Current implementation status. |
