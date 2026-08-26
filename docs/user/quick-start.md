# Quick Start

This guide admits a host to an overlay with code pairing.

No peer ID, peer address, relay, route, or JSON file is exchanged by hand.

Pair each new node once with any authorized member.

Admitted nodes then learn every other member without pairwise pairing.

## Requirements

| Requirement | Both Hosts |
| --- | --- |
| Linux with `/dev/net/tun` | required |
| Root or `CAP_NET_ADMIN` | required |
| Same network name | required |
| Running `p2p-vpn` daemon | required |
| Common LAN or public libp2p path | required |

## NixOS: Minimum Setup

### 1. Add the Flake Input

```nix
{
  inputs.p2p-vpn.url = "github:hermetic-foundation/p2p-vpn";
}
```

### 2. Import and Enable

Use the same instance name on both hosts:

```nix
{ inputs, ... }:
{
  imports = [ inputs.p2p-vpn.nixosModules.default ];

  services.p2p-vpn.instances.lab.enable = true;
}
```

This is the complete pre-pairing configuration.

The module supplies identity storage, listeners, discovery, relay fallback, and state.

### 3. Start Both Nodes

```sh
sudo nixos-rebuild switch
```

Confirm each daemon is running:

```sh
sudo systemctl status p2p-vpn-lab.service
sudo p2p-vpn instance show lab
```

### 4. Open a Pairing Code

Run on Host A:

```sh
sudo p2p-vpn pair open --instance lab
```

Record `OPEN_OPERATION` and `CODE` from the output.

### 5. Join

Run on Host B:

```sh
sudo p2p-vpn pair join CODE \
  --instance lab \
  --no-wait
```

Record `JOIN_OPERATION`.

### 6. Approve

Run on Host A:

```sh
sudo p2p-vpn pair status OPEN_OPERATION --instance lab
```

Compare `candidate peer` with Host B:

```sh
sudo p2p-vpn instance show lab
```

Approve the displayed `APPROVAL_ID` on Host A:

```sh
sudo p2p-vpn pair approve \
  OPEN_OPERATION APPROVAL_ID \
  --instance lab
```

### 7. Verify Traffic

On each host, show its built-in overlay address:

```sh
sudo p2p-vpn status \
  --config /run/p2p-vpn-lab/config.json
```

Ping the other host's `local overlay ipv4`:

```sh
ping -I pv0 PEER_OVERLAY_IPV4
```

### 8. Make the Grant Declarative

Run on Host A with `OPEN_OPERATION`.

Run on Host B with `JOIN_OPERATION`:

```sh
sudo p2p-vpn pair artifacts LOCAL_OPERATION \
  --instance lab \
  --output /etc/nixos/p2p-vpn-lab-paired.nix \
  --force
```

Import the local fragment on each host:

```nix
{
  imports = [ ./p2p-vpn-lab-paired.nix ];
}
```

Rebuild both hosts:

```sh
sudo nixos-rebuild switch
```

Follow [Pairing](pairing.md) to acknowledge the matching receipt on each host.

## Defaults Used

| Resource | Default |
| --- | --- |
| Network name | Instance name, `lab` |
| Identity | Persistent per-instance key |
| Interface | `pv0` for the first native instance |
| Overlay addresses | Derived from each peer ID |
| MTU | `1280` |
| LAN discovery | mDNS |
| Public discovery | IPFS-compatible Kademlia bootstrap |
| NAT traversal | AutoNAT and DCUtR |
| Relay fallback | Automatic candidate discovery and reservations |
| Packet transport | Datagram first, stream fallback |
| Queue and resources | Bounded defaults |

Add options only when overriding these values.

## Optional Stable Addresses

Request a chosen Host B address during join:

```sh
sudo p2p-vpn pair join CODE \
  --instance lab \
  --vpn-ip 10.44.0.2 \
  --no-wait
```

Set Host A's address declaratively before pairing:

```nix
{
  services.p2p-vpn.instances.lab.vpnIp = "10.44.0.1";
}
```

Chosen addresses are optional.

Peer-derived addresses require no address configuration.

## JSON: Minimum Static Setup

Use this path on non-NixOS hosts.

Each JSON file needs a local identity and the authorized remote peer ID.

```json
{
  "network": {
    "name": "lab",
    "private_key": "BASE64_PRIVATE_KEY"
  },
  "peers": [
    { "id": "REMOTE_PEER_ID" }
  ]
}
```

Generate the local identity and compact file:

```sh
nix run .# -- init-config \
  --output p2p-vpn.json \
  --network lab \
  --force
```

Show the local peer ID:

```sh
nix run .# -- status --config p2p-vpn.json
```

Add the remote ID while preserving the generated private key:

```sh
jq --arg peer REMOTE_PEER_ID \
  '.peers = [{ "id": $peer }]' \
  p2p-vpn.json > p2p-vpn.next.json
mv p2p-vpn.next.json p2p-vpn.json
```

Do not rerun `init-config` without passing the original key.

That would create a new identity.

Start the daemon with durable code-pairing support:

```sh
sudo ./result/bin/p2p-vpn up \
  --config p2p-vpn.json \
  --control-socket /run/p2p-vpn/control.sock \
  --pairing-state /var/lib/p2p-vpn/pairing-state.json \
  --membership-state /var/lib/p2p-vpn/membership-state.json
```

For persistent JSON onboarding, use the offline workflow in [Pairing](pairing.md).

See [Network Membership](membership.md) for whole-overlay convergence and recovery.

## Health Check

For NixOS:

```sh
sudo p2p-vpn daemon-health \
  --socket /run/p2p-vpn-lab/control.sock \
  --require-validated-peers \
  --require-supported-paths \
  --wait-seconds 30
```

For a standalone daemon, use its configured control socket.
