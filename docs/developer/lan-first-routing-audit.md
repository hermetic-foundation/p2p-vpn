# LAN-First Routing Audit

## Scope

This audit covers authorized overlay peers that are reachable on a current local
subnet while public direct addresses or circuit relays are also available.

The required policy is:

1. Discover on LAN before public lookup during startup and recovery.
2. Promote a verified LAN connection when one appears later.
3. Keep WAN and relay paths as bounded fallbacks.
4. Select payload transport from health evidence, not connection count alone.

## Evidence

The pre-change live capture is retained outside the repository at:

```text
/tmp/p2p-vpn-lan-first-20260915T195727Z
```

The capture shows a healthy baseline, not the reported failure:

| Layer | Evidence |
| --- | --- |
| Host subnet | `wlp1s0` is `192.168.0.180/24`. |
| Discovery/control | Four configured peers have healthy direct control paths. |
| Packet sessions | All four `monarchic-runners` sessions use `192.168.0.x:51820`. |
| Payload selection | All four select `direct_udp_datagram` with confirmed RTT. |

This proves that LAN packet transport can work. It does not prove that an
existing WAN connection is promoted after a LAN address becomes available.

## Failure Mechanism

The policy is split across independent mechanisms:

| Mechanism | Pre-fix behavior | Correction |
| --- | --- | --- |
| Startup holdoff | Delayed public bootstrap for 60 seconds. | All automatic public overlay dials honor the holdoff. |
| mDNS | Queried every five minutes after startup. | Overlay queries run every 10 seconds. |
| Discovery dial admission | Rejected LAN when any direct path existed. | A current-subnet candidate may promote over WAN. |
| Recovery ordering | Sorted only addresses already known. | A 15-second peer recovery window waits for fresh LAN discovery. |
| Direct deduplication | Chose initiator and transport without locality. | Current-subnet connections survive before other tie-breakers. |
| Retention | Preferred QUIC, TCP, then relay. | LAN direct precedes WAN direct and relay. |
| Packet negotiation | Needed a LAN control connection first. | LAN promotion triggers endpoint renegotiation. |
| Packet selection | Used packet health and RTT. | The selected healthy LAN packet path retains normal scoring. |

The primary defect is deterministic:

```text
healthy WAN direct connection
  -> mDNS discovers authorized peer on current subnet
  -> address is authenticated and retained
  -> should_dial_discovered_address returns false
  -> no LAN control connection
  -> no LAN-filtered packet renegotiation
  -> WAN payload session remains selected
```

Even if another event creates a LAN connection, connection deduplication can
discard it because initiator role and transport currently outrank locality.

## Constraints

| Constraint | Required handling |
| --- | --- |
| Stale private address | Dial backoff and health checks must permit WAN fallback. |
| No mDNS result | Existing 60-second startup bound remains the maximum public-bootstrap delay. |
| Explicit address | Explicit direct configuration keeps its recovery precedence. |
| Authentication | LAN discovery never bypasses configured or signed membership. |
| QUIC preference | LAN selection must not disable QUIC or owned packet-plane negotiation. |
| Network movement | Interface-change recovery invalidates stale paths and restarts bounded discovery. |

## Implemented Policy

| Event | LAN phase | Fallback phase |
| --- | --- | --- |
| Process startup | mDNS is active immediately. | Automatic public discovery starts after 60 seconds. |
| Supported path lost | Current-subnet and explicit direct addresses remain dialable for 15 seconds. | Retained WAN, Kademlia, and relay targets become eligible at the deadline. |
| Network changed | Paths and recovery state are invalidated; each authorized peer starts a new LAN phase. | Public recovery resumes after each peer deadline. |
| LAN appears later | mDNS may open an authenticated LAN connection alongside WAN. | WAN remains usable until LAN establishment and negotiation succeed. |

Recovery retries do not extend the 15-second deadline. A successful replacement
connection closes that recovery generation.

Explicit direct peer addresses remain eligible during the LAN phase. Relayed
addresses and automatic public direct addresses do not.

## Verification Contract

| Claim | Required evidence |
| --- | --- |
| Initial LAN-first | mDNS namespace test reaches payload over a local endpoint. |
| Late promotion | Existing WAN/relay path does not suppress a new LAN dial. |
| Stable selection | Both endpoints retain the same LAN connection. |
| Stale LAN fallback | Failed LAN dial does not remove a healthy WAN/relay path. |
| Payload migration | Negotiated packet endpoint changes to the current subnet. |
| No manual rescue | Tests perform no daemon restart or address injection after start. |

## Verification Results

| Claim | Automated evidence |
| --- | --- |
| Initial LAN-first | The namespace movement test starts with no configured direct peer address and selects a discovered LAN datagram path. |
| Late promotion | Unit tests permit current-subnet promotion while a WAN direct connection exists. |
| Stable selection | Direct deduplication and retention tests keep LAN before WAN. |
| Stale LAN fallback | Recovery-window tests reject stale-subnet addresses and release public discovery at 15 seconds. |
| Relay fallback | The namespace movement test removes the LAN link and reaches the peer through circuit relay. |
| LAN return | The same running daemons restore the link, rediscover it through mDNS, and promote back to direct datagrams. |
| Payload proof | Overlay ping and packet-path counters pass in all three phases. |

Command:

```sh
nix develop -c cargo test --test tun_namespace \
  tun_namespace_recovers_relay_and_direct_after_network_move \
  -- --ignored --nocapture
```
