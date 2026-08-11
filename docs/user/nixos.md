# NixOS Module

Use the flake module to run one or more `p2p-vpn` daemons as systemd services.

## Import

```nix
{
  inputs.p2p-vpn.url = "github:hermetic-foundation/p2p-vpn";
}
```

```nix
{ inputs, ... }:
{
  imports = [ inputs.p2p-vpn.nixosModules.default ];
}
```

## Minimal Service

Use typed module options for ordinary NixOS hosts:

```nix
{
  services.p2p-vpn.instances.lab = {
    enable = true;

    networkName = "lab";
    privateKeyFile = "/run/secrets/p2p-vpn/lab.key";

    peers."REMOTE_PEER_ID" = {};
  };
}
```

Required values:

| Field | Purpose |
| --- | --- |
| `networkName` | Shared overlay name. |
| `privateKeyFile` | This host's identity key. |
| `peers.<id>` | Remote peer authorization. |

`localPeer` is optional.

Set it only to assert the private key matches an expected peer ID.

## Stable Overlay IPs

Add `vpnIp` values when humans or services need stable addresses:

```nix
{
  services.p2p-vpn.instances.lab = {
    vpnIp = "10.44.0.1";

    peers."REMOTE_PEER_ID".vpnIp = "10.44.0.2";
  };
}
```

Stable IP values:

| Field | Purpose |
| --- | --- |
| `vpnIp` | This host's preferred overlay IP. |
| `peers.<id>.vpnIp` | The peer's authorized overlay IP. |

Both service shapes use the default profile:

| Item | Default |
| --- | --- |
| Interface | `pv0` |
| MTU | `1280` |
| LAN discovery | mDNS |
| Public discovery | IPFS-compatible Kademlia |
| Hole punching | DCUtR and AutoNAT |
| Relay fallback | automatic public relay candidates |
| Packet plane | UDP listener on `0.0.0.0:0` |

Add a direct LAN IP only as an override:

```nix
{
  services.p2p-vpn.instances.lab.peers."REMOTE_PEER_ID" = {
    ip = "192.168.0.203";
  };
}
```

Use explicit `addresses` only for custom ports, DNS, or relayed paths.

## Network Moves

The daemon keeps using the same generated config when a host changes networks.

Expected recovery order:

| Step | Mechanism |
| --- | --- |
| LAN available | mDNS and direct paths are preferred. |
| LAN lost | Existing direct paths are demoted. |
| Relay available | Automatically discovered relay paths keep traffic moving. |
| LAN returns | Direct paths are promoted again. |

Developer VM checks for this flow are listed in
[developer testing](../developer/testing.md).

## Discovery Options

Leave discovery unset for normal hosts.

The default profile enables:

| Mechanism | Purpose |
| --- | --- |
| mDNS | LAN peer discovery |
| Kademlia | Public IPFS-compatible routing |
| DCUtR | Relay-assisted direct upgrade |
| AutoNAT | Reachability probing |

Override discovery only for deterministic tests or controlled networks:

```nix
{
  services.p2p-vpn.instances.lab.discovery = {
    mdns = false;
    kademlia = false;
    kademliaProviderAdvertisement = false;
    dcutr = false;
    autonat = false;
  };
}
```

Use a custom Kademlia protocol to isolate a deployment:

```nix
{
  services.p2p-vpn.instances.lab.discovery.kademliaProtocol =
    "/p2p-vpn/lab/kad/1.0.0";
}
```

## Overlay IPs

Use `vpnIp` for stable host addresses:

```nix
{
  services.p2p-vpn.instances.lab = {
    vpnIp = "10.44.0.1";
    peers."REMOTE_PEER_ID".vpnIp = "10.44.0.2";
  };
}
```

The module writes these as `vpn_ip` fields:

| Value | Compiled daemon route |
| --- | --- |
| `10.44.0.1` | `10.44.0.1/32` |
| `fd00::1` | `fd00::1/128` |

Use `routes` only for prefixes or extra routed networks.

## Relay Options

Enable a relay server on infrastructure nodes:

```nix
{
  services.p2p-vpn.instances.relay = {
    enable = true;
    relayServer = true;
  };
}
```

Tune automatic relay discovery only when needed:

```nix
{
  services.p2p-vpn.instances.lab.autoRelay = {
    maxCandidates = 16;
    maxReservations = 2;
    retryIntervalSeconds = 30;
  };
}
```

Leave `autoRelay` unset for compact default configs.

Reserve an explicit relay from a VPN node:

```nix
{
  services.p2p-vpn.instances.lab.relayReservations = [
    "/ip4/203.0.113.10/tcp/4001/p2p/RELAY_PEER_ID/p2p-circuit"
  ];
}
```

Use peer `addresses` for forced relay paths:

```nix
{
  services.p2p-vpn.instances.lab.peers."REMOTE_PEER_ID".addresses = [
    "/ip4/203.0.113.10/tcp/4001/p2p/RELAY_PEER_ID/p2p-circuit/p2p/REMOTE_PEER_ID"
  ];
}
```

## Private Keys

Prefer `privateKeyFile`:

```nix
{
  services.p2p-vpn.instances.lab.privateKeyFile =
    "/run/secrets/p2p-vpn/lab.key";
}
```

The file must contain the base64 identity key.

The generated runtime JSON is written to:

```text
/run/p2p-vpn-<instance>/config.json
```

Use `privateKey` only for throwaway test hosts:

```nix
{
  services.p2p-vpn.instances.lab = {
    privateKey = "BASE64_PRIVATE_KEY";
    peers."REMOTE_PEER_ID" = { };
  };
}
```

`privateKey` is copied into the Nix store.

## Raw Settings

Use `settings` only when you need full JSON control:

```nix
{
  services.p2p-vpn.instances.lab = {
    enable = true;
    settings = {
      network = {
        name = "lab";
        private_key = "BASE64_PRIVATE_KEY";
      };
      peers = [
        { id = "REMOTE_PEER_ID"; }
      ];
    };
  };
}
```

`settings` values are copied into the Nix store.

Do not put private keys or membership keys in `settings` on real hosts.

## Secret Config

Use `configFile` when another tool writes the complete JSON:

```nix
{
  services.p2p-vpn.instances.lab = {
    enable = true;
    configFile = "/run/secrets/p2p-vpn/lab.json";
  };
}
```

Do not combine `configFile` with generated config options.

## Pairing Output

Use pairing to create both files for a NixOS host:

```sh
sudo install -d -m 0700 /var/lib/p2p-vpn
sudo p2p-vpn pair accept lab.pair \
  --output /var/lib/p2p-vpn/lab.json \
  --nixos-output /etc/nixos/p2p-vpn-lab.nix \
  --nixos-instance lab
```

Generated JSON:

| File | Purpose |
| --- | --- |
| `/var/lib/p2p-vpn/lab.json` | Complete paired runtime config. |
| `/etc/nixos/p2p-vpn-lab.nix` | NixOS module snippet. |

The generated Nix uses this shape:

```nix
{
  services.p2p-vpn.instances."lab" = {
    enable = true;
    configFile = "/var/lib/p2p-vpn/lab.json";
  };
}
```

Import it from your system config:

```nix
{
  imports = [ ./p2p-vpn-lab.nix ];
}
```

This is the recommended pairing path.

It does not require hand translation into typed Nix options.

Keep the paired JSON root-owned and private:

```sh
sudo chmod 0600 /var/lib/p2p-vpn/lab.json
```

Use typed options later only if you deliberately want fully declarative state.

## Firewall

Open default service ports:

```nix
{
  services.p2p-vpn.instances.lab = {
    openFirewall = true;
  };
}
```

This opens:

| Port | Purpose |
| --- | --- |
| TCP `4001` | Default libp2p listener |
| UDP `5353` | mDNS LAN discovery |

Add custom ports only when your config overrides listeners:

```nix
{
  services.p2p-vpn.instances.lab = {
    openFirewall = true;
    tcpPorts = [ 4001 4401 ];
    udpPorts = [ 5353 4001 ];
    packetPlaneUdpPorts = [ 51820 ];
    packetPlaneQuicPorts = [ 51821 ];
  };
}
```

## Control Socket

The default socket is:

```text
/run/p2p-vpn-<instance>/control.sock
```

Override it when needed:

```nix
{
  services.p2p-vpn.instances.lab.controlSocket =
    "/run/p2p-vpn/control.sock";
}
```

Set it to `null` to disable daemon control commands.

## Multiple Instances

Each instance creates a service:

| Instance | Unit |
| --- | --- |
| `lab` | `p2p-vpn-lab.service` |
| `relay` | `p2p-vpn-relay.service` |

Each instance gets:

| Resource | Value |
| --- | --- |
| Runtime directory | `/run/p2p-vpn-<instance>` |
| TUN access | `/dev/net/tun` |
| Capabilities | `CAP_NET_ADMIN`, `CAP_NET_RAW` |

## Commands

Start:

```sh
sudo systemctl start p2p-vpn-lab.service
```

Health:

```sh
sudo p2p-vpn daemon-health \
  --socket /run/p2p-vpn-lab/control.sock \
  --require-validated-peers \
  --require-supported-paths
```

Stop:

```sh
sudo systemctl stop p2p-vpn-lab.service
```
