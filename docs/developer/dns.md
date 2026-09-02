# DNS Architecture

This document covers the authenticated overlay DNS implementation.

User commands and configuration live in [Overlay DNS](../user/dns.md).

## Components

```text
signed membership + peer-signed hostname records + static config
  -> effective membership graph
  -> immutable DnsZone snapshot
  -> watch-channel publication
  -> bounded UDP and TCP authority
  -> systemd-resolved per-link split DNS
```

| Component | Source | Responsibility |
| --- | --- | --- |
| Naming model | `src/dns.rs` | Canonical labels, records, conflicts, PTR map |
| DNS runtime | `src/runtime/dns.rs` | UDP/TCP protocol, limits, metrics, refresh |
| Membership | `src/membership.rs` | Effective peer and route authority |
| Hostname records | `src/hostname.rs` | Member-signed mutable names and merge rules |
| Pairing | `src/pairing.rs` | Authenticated requested and assigned hostnames |
| Daemon integration | `src/runtime/runner.rs` | Lifecycle, refresh, control-socket views |
| NixOS integration | `nix/nixos-module.nix` | Split DNS registration and cleanup |

## Zone Contract

The zone is:

```text
<canonical-network-name>.p2p-vpn.internal.
```

Forward records are:

```text
<canonical-hostname>.<zone>
```

Both labels use one ASCII DNS label and reject leading or trailing hyphens.

## Name Sources

| Source | Trust Boundary |
| --- | --- |
| Local `dns.hostname` | Local configuration |
| Static `peers[].name` | Local static authorization |
| Hostname record | Self-signature from an effective member |
| Membership `hostname` | Initial and legacy delegated name |
| Peer-ID fallback | Derived from each effective member identity |

Multiple sources for the same peer and label collapse into one record set.

Multiple peers for one label create a conflict and no forward record.

A hostname record overrides static and membership names for the same Peer ID.
It has no effect unless that Peer ID is already an effective member.

## Mutable Hostname Records

The payload binds these fields under a versioned signature domain:

| Field | Purpose |
| --- | --- |
| `network_name` | Prevents use in another overlay. |
| `peer`, `public_key` | Binds the name to one libp2p identity. |
| `sequence` | Selects the latest record for that peer. |
| `hostname` | Carries one canonical DNS label. |
| `issued_at_unix_seconds` | Supports diagnosis without deciding precedence. |

The member signs its own record with its existing libp2p identity.

Merge behavior is monotonic. A higher sequence replaces the prior record;
equal-sequence unequal records fail closed as equivocation.

Records are bounded to 2 KiB each and 256 per network. Capabilities and the
network-scoped Kademlia bundle carry bounded subsets with the local record first.

The daemon reconciles `dns.hostname` at startup. A changed value issues sequence
`n + 1`; an unchanged value preserves the existing record and sequence.

## Membership Extension

`MembershipRecordPayload.hostname` is optional.

Omission preserves the byte shape and validation behavior of older records.

Validation order:

1. Verify record version, network, signature, and issuer authority.
2. Validate the optional canonical hostname label.
3. Resolve the latest issuer/member version.
4. Remove revoked or expired authority from the effective graph.
5. Build DNS names only from effective members.

Revocation tombstones remain persisted.

Expiry removes active records when `now >= expires_at_unix_seconds`.

## Pairing Binding

Code pairing authenticates `requested_hostname` inside the signed join request.

The inviter may accept it or supply `pair approve --hostname`.

The final response carries signed membership records for:

| Record | Hostname Source |
| --- | --- |
| Joiner grant | Assigned or requested hostname |
| Inviter self/grant path | Inviter's configured local hostname |

`dns.enabled` controls the local resolver only. A configured hostname remains
an authenticated identity claim when that listener is disabled.

Pending `Submit` requests are persisted with their exact signed transcript.

After a disconnect or restart, the joiner retries that request instead of
starting a new one-time-code exchange.

## Address Selection

Every effective peer starts with derived IPv4 and IPv6 overlay addresses.

Additional records come from:

| Input | Accepted Address |
| --- | --- |
| `vpn_ip` | IPv4 or IPv6 host address |
| Route grant | IPv4 `/32` or IPv6 `/128` only |
| Prefix route | Not a DNS address |

PTR chooses a non-fallback friendly record when available.

Otherwise it points to the peer-ID fallback name.

## Snapshot Refresh

`DnsZone` is immutable after construction.

The runtime publishes replacements through `tokio::sync::watch`.

Refresh triggers:

| Trigger | Result |
| --- | --- |
| Membership merge | Rebuild immediately |
| Pairing enrollment | Rebuild immediately |
| Revocation | Rebuild immediately |
| Next claim expiry | Timed rebuild at the deadline |
| Invalid replacement | Publish an empty fail-closed zone and retry after 5 seconds |

UDP and TCP tasks read the current `Arc<DnsZone>` without locking per record.

## DNS Protocol

The responder is authoritative and non-recursive.

| Query | Result |
| --- | --- |
| In-zone `A`, `AAAA`, `ANY` | Authorized record values |
| Known address `PTR`, `ANY` | Preferred canonical target |
| Missing in-zone name | Authoritative `NXDOMAIN` plus SOA |
| Existing name, wrong type | `NOERROR`/NODATA plus SOA |
| Ambiguous hostname | `NXDOMAIN`; control view reports conflict |
| Outside zone or unknown PTR | `REFUSED` |
| Unsupported class | `REFUSED` |
| Invalid shape | `FORMERR` |
| EDNS version mismatch | `BADVERS` |

Responses copy the request ID, question, recursion-desired bit, and supported
EDNS payload size. Recursion-available is always false.

## Resource Bounds

| Resource | Bound |
| --- | --- |
| Request bytes | `4,096` |
| UDP response bytes | `1,232` |
| TCP response bytes | `65,535` |
| Concurrent TCP connections | `64` |
| Queries per TCP connection | `32` |
| TCP read/write timeout | `5 seconds` |
| Record sets | `1,024` |
| Addresses per peer | `256` |
| Total addresses | `4,096` |
| CLI list page | `256` |
| TTL | `1` through `300 seconds` |

UDP responses over the negotiated limit set truncation and omit the oversized
answer. Clients may retry over TCP.

## Control-Socket Views

| Request | CLI | Data |
| --- | --- | --- |
| DNS status | `dns status` | Listener, zone, counts, counters, refresh |
| DNS list | `dns list` | Records, PTRs, conflicts, bounded pagination |
| DNS resolve | `dns resolve` | Qualification, type, values, failure status |

The local RPC framing and maximum request size already bound these requests.

Control output sanitizes query input and limits peer/address previews.

## NixOS Split DNS

Each instance uses its TUN interface as a `systemd-resolved` DNS link.

Registration sequence:

1. Wait for the daemon control socket and DNS status.
2. Read the actual loopback listener and authoritative zone.
3. Verify the parent-suffix guard is active.
4. Lock `/run/p2p-vpn-resolved.lock`.
5. Reject duplicate zones across active instances.
6. Register DNS server, search/routing domain, and `default-route no`.
7. Disable LLMNR, mDNS, DNSSEC, and DNS-over-TLS for that link.
8. Persist the interface and zone under the instance runtime directory.

The parent guard owns `~p2p-vpn.internal` on a dedicated dummy link.

It refuses unmatched private-suffix queries and prevents resolver leakage.

## Service Lifecycle

| Unit | Relationship |
| --- | --- |
| `p2p-vpn-<instance>.service` | Owns TUN and loopback DNS responder |
| `p2p-vpn-<instance>-resolved.service` | Bound and part-of daemon/resolved/guard |
| `p2p-vpn-dns-guard.service` | Bound and part-of `systemd-resolved` |

Per-instance cleanup uses `resolvectl revert` and removes its state file.

The guard reverts and deletes only the dummy link it owns.

All setup and cleanup operations serialize through one runtime lock.

## Compatibility

| Surface | Rule |
| --- | --- |
| JSON config | Missing `network.dns` keeps DNS disabled |
| NixOS native mode | `dns.enable` defaults false |
| Membership records | Missing `hostname` remains valid |
| Persisted membership | Existing format version remains readable |
| Control capabilities | Missing `hostname_records` decodes as empty |
| Kademlia bundle | Missing `hostname_records` decodes as empty |
| Pairing requests | Missing `requested_hostname` remains valid |
| Native and JSON modes | Remain separate complete modes |

No existing wire version changed. New serialized fields are additive, optional,
and covered by compatibility tests.

## Verification

### Focused Rust Tests

```sh
cargo test dns::tests
cargo test runtime::dns::tests
cargo test membership::tests
cargo test pairing::tests
cargo test --test pair_cli
```

### Nix Evaluation

```sh
nix build .#checks.x86_64-linux.nixos-module
```

### Resolver Lifecycle

```sh
nix build .#checks.x86_64-linux.nixos-vm-module-lifecycle
```

### Transitive Membership Lifecycle

```sh
nix build \
  .#checks.x86_64-linux.nixos-vm-membership-convergence
```

The four-VM test covers:

| Area | Assertion |
| --- | --- |
| Indirect membership | `A` admits `B`; `B` admits `C`; `A` and `C` resolve |
| Search domain | Short names resolve on all three members |
| FQDN | Canonical names resolve on all three members |
| Traffic | Never-paired `A` and `C` ping by name |
| Relay | DNS traffic survives isolated relay fallback |
| Restart | DNS restores with persisted membership |
| Expiry | Names, routes, and fallbacks disappear automatically |
| Conflict | Ambiguous friendly name fails closed everywhere |
| Rename | Newer signed name resolves the conflict |
| Revocation | Names and routes remain absent after restart |
