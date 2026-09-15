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

| Mechanism | Current behavior | Gap |
| --- | --- | --- |
| Startup holdoff | Delays public bootstrap for 60 seconds. | Applies only to public bootstrap. |
| mDNS | Continuously discovers interface-local addresses. | Cannot force an immediate query through the libp2p API. |
| Discovery dial admission | Dials when disconnected or relay-only. | Rejects a LAN address when any healthy direct path exists. |
| Recovery ordering | Sorts known current-subnet addresses before relays and stale direct addresses. | Cannot prefer an address that mDNS has not emitted yet. |
| Direct deduplication | Chooses a stable initiator per transport. | Does not prefer current-subnet connections. |
| Retention | Prefers QUIC control, then TCP, then relay. | Does not distinguish LAN from WAN direct connections. |
| Packet negotiation | Filters signed endpoint candidates when a healthy LAN control connection exists. | Never runs if LAN control promotion is suppressed. |
| Packet selection | Uses negotiated datagram health and RTT. | Path diagnostics do not themselves identify endpoint locality. |

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

## Planned Correction

1. Treat an mDNS address on a current local subnet as a promotion candidate.
2. Suppress the dial only when a healthy current-subnet control connection exists.
3. Rank current-subnet connections before transport and initiator tie-breakers.
4. Let the existing authenticated packet negotiation migrate to matching LAN endpoints.
5. Preserve public direct and relay connections until LAN health is established.
6. Add explicit promotion, stale-LAN, simultaneous-result, and fallback tests.

## Verification Contract

| Claim | Required evidence |
| --- | --- |
| Initial LAN-first | mDNS namespace test reaches payload over a local endpoint. |
| Late promotion | Existing WAN/relay path does not suppress a new LAN dial. |
| Stable selection | Both endpoints retain the same LAN connection. |
| Stale LAN fallback | Failed LAN dial does not remove a healthy WAN/relay path. |
| Payload migration | Negotiated packet endpoint changes to the current subnet. |
| No manual rescue | Tests perform no daemon restart or address injection after start. |
