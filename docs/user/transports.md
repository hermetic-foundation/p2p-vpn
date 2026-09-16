# Packet Transports

`p2p-vpn` selects and recovers packet paths automatically.
Minimal configuration does not require listener ports or peer routes.

## Path Order

The daemon uses the highest healthy path supported by both peers.

| Priority | Path | Typical Use |
| ---: | --- | --- |
| 1 | QUIC datagram | Normal direct packet delivery |
| 2 | UDP datagram | Compatible direct fallback |
| 3 | Direct QUIC stream | Reliable direct fallback |
| 4 | Direct TCP stream | Lower direct fallback |
| 5 | Circuit relay stream | Public reachability fallback |

LAN discovery runs before public recovery.
A restored direct QUIC path is promoted automatically.

## Default Configuration

Omit packet-plane settings for normal use.
The daemon opens ephemeral UDP and QUIC listeners.

NixOS needs no transport options:

```nix
{
  services.p2p-vpn.instances.lab.enable = true;
}
```

JSON also uses defaults when `network.packet_plane` is absent.

## Overrides

Use overrides only for controlled networks or known public mappings.

| Override | Effect |
| --- | --- |
| `quicListen = [];` | Disable QUIC datagrams |
| `listen = [];` without a QUIC override | Use streams only |
| Nonempty `quicListen` with empty `listen` | Use QUIC without owned UDP |
| External endpoints | Advertise a known public port mapping |

NixOS example:

```nix
{
  services.p2p-vpn.instances.lab.packetPlane = {
    listen = [ "0.0.0.0:51820" ];
    quicListen = [ "0.0.0.0:51821" ];
  };
}
```

See [Configuration](configuration.md#packet-plane) for JSON field names.

## Inspect A Path

Use read-only daemon commands:

```sh
p2p-vpn daemon-paths --instance lab
p2p-vpn daemon-state --instance lab
p2p-vpn daemon-mtu --instance lab
p2p-vpn daemon-capabilities --instance lab
```

Check all three facts separately:

| Fact | Evidence |
| --- | --- |
| Connection exists | A live path has a `connection_id` |
| Scheduler selected it | `selected_path` names the path |
| Payload used it | Its backend-specific counter increases |

An installed QUIC session alone does not prove QUIC payload delivery.

## Useful Counters

| Counter | Meaning |
| --- | --- |
| `packet_plane_quic_sessions` | Installed QUIC packet sessions |
| `outbound_owned_quic_datagram_packets` | QUIC payload submissions |
| `outbound_owned_udp_datagram_packets` | UDP payload submissions |
| `outbound_direct_quic_stream_fallback_packets` | Direct QUIC stream payloads |
| `outbound_direct_tcp_stream_fallback_packets` | Direct TCP stream payloads |
| `outbound_relay_stream_fallback_packets` | Relay stream payloads |

Successful submission does not by itself prove remote delivery.
Confirm with application traffic or an overlay ping.

## MTU Behavior

Peers negotiate a usable MTU.
The daemon also enforces the live QUIC datagram limit.

| Situation | Result |
| --- | --- |
| Packet fits | Send on the selected path |
| QUIC limit is smaller | Try the next compatible path |
| Selected fallback has a smaller MTU | Reject the oversized packet |
| Oversized IPv4 packet | Return fragmentation-needed information |

The daemon does not fragment packets inside the overlay.
Applications and kernels should adapt to the reported path MTU.

## Recovery

Path failure demotes the failed backend.
Bounded queues retain only recent packets while another path is selected.

Recovery does not require:

- Static peer multiaddrs.
- Manual overlay routes.
- A daemon restart.

Configured addresses remain optional hints.
Public bootstrap and relay peers provide reachability, not VPN membership.

## Current Certification Limits

Deterministic Linux tests cover QUIC loss, blocking, restoration, endpoint changes,
fallback, relay paths, replay, and MTU boundaries.

Physical hotspot, upstream VPN, Android movement, and arbitrary IPv6 PMTU behavior
need environment-specific testing before relying on those exact topologies.
