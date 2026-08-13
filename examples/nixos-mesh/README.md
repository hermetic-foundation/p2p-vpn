# Two-Node NixOS Mesh

The example declares one native `lab` instance on each host.

No JSON config, key path, peer ID, address, or relay is required.

## Build

```sh
nix build .#nixosConfigurations.node-a.config.system.build.toplevel
nix build .#nixosConfigurations.node-b.config.system.build.toplevel
```

## Deploy

Deploy each configuration through the normal host workflow.

The first service start creates one persistent identity per host.

## Pair

On `node-a`:

```sh
sudo p2p-vpn pair offer \
  --nixos-instance lab \
  --output /tmp/lab.pair \
  --force
```

Transfer the offer to `node-b`.

On `node-b`:

```sh
sudo systemctl stop p2p-vpn-lab.service
sudo p2p-vpn pair accept /tmp/lab.pair \
  --nixos-output /etc/nixos/p2p-vpn-lab.nix \
  --nixos-instance lab \
  --nixos-only
```

Import `/etc/nixos/p2p-vpn-lab.nix` from the node configuration.

Rebuild `node-b`; `node-a` requires no configuration change.

## Stable Addresses

Add `--vpn-ip 10.44.0.2` on accept for a fixed joiner address.

Set `services.p2p-vpn.instances.lab.vpnIp = "10.44.0.1";` on `node-a`.
