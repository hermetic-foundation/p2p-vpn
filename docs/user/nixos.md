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

Use `settings` for throwaway test configs:

```nix
{
  services.p2p-vpn.instances.lab = {
    enable = true;
    settings = {
      network = {
        name = "lab";
        local_peer = "LOCAL_PEER_ID";
        private_key = "BASE64_PRIVATE_KEY";
        routes = [ { prefix = "10.44.0.1/32"; } ];
      };
      peers = [
        {
          id = "REMOTE_PEER_ID";
          ip = "192.168.0.203";
          routes = [ { prefix = "10.44.0.2/32"; } ];
        }
      ];
    };
  };
}
```

The example above stores `private_key` in the Nix store.

Do not put private keys or membership keys in `settings` on real machines.

## Secret Config

Use `configFile` for real deployments:

```nix
{
  services.p2p-vpn.instances.lab = {
    enable = true;
    configFile = "/run/secrets/p2p-vpn/lab.json";
  };
}
```

## Firewall

Open ports per instance:

```nix
{
  services.p2p-vpn.instances.lab = {
    openFirewall = true;
    tcpPorts = [ 4001 ];
    udpPorts = [ 4001 ];
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
