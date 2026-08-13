{
  self,
  pkgs,
  package,
}:
let
  isolatedDiscovery = {
    mdns = false;
    kademlia = false;
    kademliaProviderAdvertisement = false;
    dcutr = false;
    autonat = false;
  };
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-module-lifecycle";

  nodes.machine =
    { ... }:
    {
      imports = [ self.nixosModules.default ];

      system.stateVersion = "25.11";
      networking.firewall.enable = true;
      environment.systemPackages = [
        package
        pkgs.iproute2
        pkgs.jq
      ];

      services.p2p-vpn.instances.alpha = {
        enable = true;
        discovery = isolatedDiscovery;
      };
      services.p2p-vpn.instances.beta = {
        enable = true;
        discovery = isolatedDiscovery;
      };
    };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("p2p-vpn-alpha.service")
    machine.wait_for_unit("p2p-vpn-beta.service")
    machine.wait_for_file("/run/p2p-vpn-alpha/control.sock")
    machine.wait_for_file("/run/p2p-vpn-beta/control.sock")

    with subtest("minimal instances generate private persistent identities"):
        for name in ["alpha", "beta"]:
            machine.succeed(f"test -s /var/lib/p2p-vpn/{name}/private.key")
            machine.succeed(f"test $(stat -c %a /var/lib/p2p-vpn/{name}/private.key) = 600")
            machine.succeed(f"test $(stat -c %a /run/p2p-vpn-{name}/config.json) = 600")
            machine.succeed(
                f"p2p-vpn status --config /run/p2p-vpn-{name}/config.json >/tmp/{name}-status"
            )
            machine.succeed(f"grep -q '^local peer: 12D3KooW' /tmp/{name}-status")
            machine.succeed(
                f"sha256sum /var/lib/p2p-vpn/{name}/private.key | cut -d' ' -f1 >/tmp/{name}-key.sha"
            )

        alpha_peer = machine.succeed("sed -n 's/^local peer: //p' /tmp/alpha-status").strip()
        beta_peer = machine.succeed("sed -n 's/^local peer: //p' /tmp/beta-status").strip()
        assert alpha_peer != beta_peer

    with subtest("computed interfaces and listeners do not collide"):
        machine.succeed("ip link show pv0")
        machine.succeed("ip link show pv1")
        machine.succeed(
            "jq -e '.interface.name == \"pv0\" "
            "and .network.listen_addresses == ["
            "\"/ip4/0.0.0.0/tcp/4001\","
            "\"/ip4/0.0.0.0/udp/4001/quic-v1\"] "
            "and .network.packet_plane.listen == [\"0.0.0.0:51820\"]' "
            "/run/p2p-vpn-alpha/config.json"
        )
        machine.succeed(
            "jq -e '.interface.name == \"pv1\" "
            "and .network.listen_addresses == ["
            "\"/ip4/0.0.0.0/tcp/4002\","
            "\"/ip4/0.0.0.0/udp/4002/quic-v1\"] "
            "and .network.packet_plane.listen == [\"0.0.0.0:51821\"]' "
            "/run/p2p-vpn-beta/config.json"
        )
        machine.succeed("ss -H -lun 'sport = :51820' | grep -q .")
        machine.succeed("ss -H -lun 'sport = :51821' | grep -q .")
    with subtest("restart and crash recovery preserve identities"):
        machine.succeed("systemctl restart p2p-vpn-alpha.service")
        machine.wait_for_unit("p2p-vpn-alpha.service")
        machine.succeed(
            "test $(sha256sum /var/lib/p2p-vpn/alpha/private.key | cut -d' ' -f1) "
            "= $(cat /tmp/alpha-key.sha)"
        )

        machine.succeed("systemctl kill --kill-who=main --signal=KILL p2p-vpn-beta.service")
        machine.wait_until_succeeds("systemctl is-active --quiet p2p-vpn-beta.service", timeout=30)
        machine.wait_for_file("/run/p2p-vpn-beta/control.sock")
        machine.succeed(
            "test $(sha256sum /var/lib/p2p-vpn/beta/private.key | cut -d' ' -f1) "
            "= $(cat /tmp/beta-key.sha)"
        )

    with subtest("instances remain independently operable"):
        machine.succeed("systemctl stop p2p-vpn-beta.service")
        machine.succeed("systemctl is-active --quiet p2p-vpn-alpha.service")
        machine.succeed("ip link show pv0")
        machine.fail("ip link show pv1")
  '';
}
