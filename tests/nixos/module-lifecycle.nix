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
        pkgs.bind
        pkgs.iproute2
        pkgs.jq
      ];

      services.p2p-vpn.instances.alpha = {
        enable = true;
        discovery = isolatedDiscovery;
        dns = {
          enable = true;
          hostname = "alpha-host";
        };
      };
      services.p2p-vpn.instances.beta = {
        enable = true;
        discovery = isolatedDiscovery;
        dns = {
          enable = true;
          hostname = "beta-host";
        };
      };
    };

  testScript = ''
    def assert_private_query_fails_fast(name):
        for label, command in [
            ("resolvectl", f"resolvectl query {name}"),
            ("ahostsv4", f"getent ahostsv4 {name}"),
            ("ahostsv6", f"getent ahostsv6 {name}"),
        ]:
            machine.succeed(
                f"started=$(date +%s%3N); rc=0; timeout 10s {command} "
                f">/tmp/private-query-{label} 2>&1 || rc=$?; "
                f"elapsed=$(($(date +%s%3N) - started)); "
                f"echo {label} rc=$rc elapsed_ms=$elapsed >&2; "
                'test "$rc" -ne 0; test "$rc" -ne 124'
            )

    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("systemd-resolved.service")
    machine.wait_for_unit("p2p-vpn-dns-guard.service")
    machine.wait_for_unit("p2p-vpn-alpha.service")
    machine.wait_for_unit("p2p-vpn-beta.service")
    machine.wait_for_unit("p2p-vpn-alpha-resolved.service")
    machine.wait_for_unit("p2p-vpn-beta-resolved.service")
    machine.wait_for_file("/run/p2p-vpn-dns-guard/listener")
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

    with subtest("instance inspection maps networks, interfaces, and peer IDs"):
        machine.succeed("p2p-vpn instance list >/tmp/instances")
        machine.succeed(
            "grep -Eq '^alpha[[:space:]]+alpha[[:space:]]+pv0[[:space:]]+12D3KooW' /tmp/instances"
        )
        machine.succeed(
            "grep -Eq '^beta[[:space:]]+beta[[:space:]]+pv1[[:space:]]+12D3KooW' /tmp/instances"
        )
        machine.succeed(
            "p2p-vpn instance list --format json "
            "| jq -e 'length == 2 "
            "and .[0].instance == \"alpha\" and .[0].network == \"alpha\" "
            "and .[0].interface == \"pv0\" and (.[0].peer_id | startswith(\"12D3KooW\")) "
            "and .[1].instance == \"beta\" and .[1].network == \"beta\" "
            "and .[1].interface == \"pv1\" and (.[1].peer_id | startswith(\"12D3KooW\"))'"
        )
        machine.succeed(
            f"p2p-vpn instance show alpha "
            f"| grep -F 'peer ID: {alpha_peer}'"
        )

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

    with subtest("split DNS routes each overlay through its own link"):
        for name, interface, hostname in [
            ("alpha", "pv0", "alpha-host"),
            ("beta", "pv1", "beta-host"),
        ]:
            zone = f"{name}.p2p-vpn.internal"
            machine.wait_for_file(f"/run/p2p-vpn-{name}/resolved-interface")
            machine.succeed(
                f"test $(cut -f1 /run/p2p-vpn-{name}/resolved-interface) = {interface}"
            )
            machine.succeed(
                f"test $(cut -f2 /run/p2p-vpn-{name}/resolved-interface) = {zone}"
            )
            machine.succeed(f"resolvectl dns {interface} | grep -F '127.0.0.1:'")
            machine.succeed(f"resolvectl domain {interface} | grep -F '{zone}'")
            machine.fail(f"resolvectl domain {interface} | grep -E 'in-addr|ip6.arpa'")
            machine.wait_until_succeeds(
                f"resolvectl query {hostname}.{zone} | grep -Eq '100\\.64\\.|fd00:'"
            )
            machine.wait_until_succeeds(
                f"resolvectl query {hostname} | grep -Eq '100\\.64\\.|fd00:'"
            )

        machine.fail("resolvectl query alpha-host.beta.p2p-vpn.internal")
        machine.fail("resolvectl query beta-host.alpha.p2p-vpn.internal")

    with subtest("private suffix guard rejects unknown names without public fallback"):
        machine.succeed("ip link show pvdns0")
        machine.succeed(
            "ip -6 address show dev pvdns0 | grep -F 'fd70:3270:2d76:706e::53/128'"
        )
        machine.succeed("resolvectl dns pvdns0 | grep -F '127.0.0.1:'")
        machine.succeed("resolvectl domain pvdns0 | grep -F '~p2p-vpn.internal'")
        machine.succeed("resolvectl status pvdns0 | grep -F 'Current Scopes: DNS'")
        guard_listener = machine.succeed(
            "cat /run/p2p-vpn-dns-guard/listener"
        ).strip()
        guard_host, guard_port = guard_listener.rsplit(":", 1)
        machine.succeed(
            f"dig @{guard_host} -p {guard_port} missing.unknown.p2p-vpn.internal A "
            "+noall +comments | grep -F 'status: NXDOMAIN'"
        )
        machine.succeed(
            f"dig +tcp @{guard_host} -p {guard_port} missing.unknown.p2p-vpn.internal A "
            "+noall +comments | grep -F 'status: NXDOMAIN'"
        )
        machine.succeed(
            f"dig @{guard_host} -p {guard_port} example.com A +noall +comments "
            "| grep -F 'status: REFUSED'"
        )
        assert_private_query_fails_fast("missing.unknown.p2p-vpn.internal")

    with subtest("authoritative listeners serve bounded UDP TCP and PTR answers"):
        for name, hostname in [("alpha", "alpha-host"), ("beta", "beta-host")]:
            zone = f"{name}.p2p-vpn.internal"
            listener = machine.succeed(
                f"p2p-vpn dns status --socket /run/p2p-vpn-{name}/control.sock "
                "| sed -n 's/.* listener=\\([^ ]*\\).*/\\1/p'"
            ).strip()
            dns_host, dns_port = listener.rsplit(":", 1)
            overlay_ipv4 = machine.succeed(
                f"p2p-vpn status --config /run/p2p-vpn-{name}/config.json "
                "| sed -n 's/^local overlay ipv4: //p'"
            ).strip()
            machine.succeed(
                f"dig +short @{dns_host} -p {dns_port} {hostname}.{zone} A | grep -Fx '{overlay_ipv4}'"
            )
            machine.succeed(
                f"dig +tcp +short @{dns_host} -p {dns_port} {hostname}.{zone} A | grep -Fx '{overlay_ipv4}'"
            )
            machine.succeed(
                f"dig +short @{dns_host} -p {dns_port} -x {overlay_ipv4} "
                f"| grep -Fx '{hostname}.{zone}.'"
            )
            machine.succeed(
                f"dig @{dns_host} -p {dns_port} example.com A +noall +comments "
                "| grep -F 'status: REFUSED'"
            )

    with subtest("restart and crash recovery preserve identities"):
        machine.succeed("systemctl restart p2p-vpn-alpha.service")
        machine.wait_for_unit("p2p-vpn-alpha.service")
        machine.wait_for_unit("p2p-vpn-alpha-resolved.service")
        machine.wait_for_file("/run/p2p-vpn-alpha/resolved-interface")
        machine.wait_until_succeeds(
            "resolvectl query alpha-host.alpha.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )
        machine.succeed(
            "test $(sha256sum /var/lib/p2p-vpn/alpha/private.key | cut -d' ' -f1) "
            "= $(cat /tmp/alpha-key.sha)"
        )

        machine.succeed("systemctl kill --kill-who=main --signal=KILL p2p-vpn-beta.service")
        machine.wait_until_succeeds("systemctl is-active --quiet p2p-vpn-beta.service", timeout=30)
        machine.wait_until_succeeds(
            "systemctl is-active --quiet p2p-vpn-beta-resolved.service", timeout=30
        )
        machine.wait_for_file("/run/p2p-vpn-beta/control.sock")
        machine.wait_for_file("/run/p2p-vpn-beta/resolved-interface")
        machine.wait_until_succeeds(
            "resolvectl query beta-host.beta.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )
        machine.succeed(
            "test $(sha256sum /var/lib/p2p-vpn/beta/private.key | cut -d' ' -f1) "
            "= $(cat /tmp/beta-key.sha)"
        )

    with subtest("private suffix guard recovers after a crash"):
        alpha_pid = machine.succeed(
            "systemctl show p2p-vpn-alpha.service -p MainPID --value"
        ).strip()
        beta_pid = machine.succeed(
            "systemctl show p2p-vpn-beta.service -p MainPID --value"
        ).strip()
        machine.succeed(
            "systemctl kill --kill-who=main --signal=KILL p2p-vpn-dns-guard.service"
        )
        machine.wait_until_succeeds(
            "systemctl is-active --quiet p2p-vpn-dns-guard.service", timeout=30
        )
        machine.wait_for_file("/run/p2p-vpn-dns-guard/listener")
        machine.wait_until_succeeds(
            "resolvectl domain pvdns0 | grep -F '~p2p-vpn.internal'", timeout=30
        )
        for service in ["alpha", "beta"]:
            machine.wait_until_succeeds(
                f"systemctl is-active --quiet p2p-vpn-{service}.service", timeout=30
            )
            machine.wait_until_succeeds(
                f"systemctl is-active --quiet p2p-vpn-{service}-resolved.service",
                timeout=30,
            )
        machine.succeed(
            f"test $(systemctl show p2p-vpn-alpha.service -p MainPID --value) = {alpha_pid}"
        )
        machine.succeed(
            f"test $(systemctl show p2p-vpn-beta.service -p MainPID --value) = {beta_pid}"
        )
        machine.wait_until_succeeds(
            "resolvectl query alpha-host.alpha.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )
        machine.wait_until_succeeds(
            "resolvectl query beta-host.beta.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )

    with subtest("resolved restart reapplies DNS without restarting VPN daemons"):
        alpha_pid = machine.succeed(
            "systemctl show p2p-vpn-alpha.service -p MainPID --value"
        ).strip()
        beta_pid = machine.succeed(
            "systemctl show p2p-vpn-beta.service -p MainPID --value"
        ).strip()
        machine.succeed("systemctl restart systemd-resolved.service")
        machine.wait_for_unit("systemd-resolved.service")
        machine.wait_for_unit("p2p-vpn-dns-guard.service")
        for service in ["alpha", "beta"]:
            machine.wait_until_succeeds(
                f"systemctl is-active --quiet p2p-vpn-{service}-resolved.service",
                timeout=30,
            )
            machine.wait_for_file(f"/run/p2p-vpn-{service}/resolved-interface")
        machine.succeed(
            f"test $(systemctl show p2p-vpn-alpha.service -p MainPID --value) = {alpha_pid}"
        )
        machine.succeed(
            f"test $(systemctl show p2p-vpn-beta.service -p MainPID --value) = {beta_pid}"
        )
        machine.wait_until_succeeds(
            "resolvectl query alpha-host.alpha.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )
        machine.wait_until_succeeds(
            "resolvectl query beta-host.beta.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )
        assert_private_query_fails_fast("missing.after-restart.p2p-vpn.internal")

    with subtest("instances remain independently operable"):
        machine.succeed("systemctl stop p2p-vpn-beta.service")
        machine.succeed("systemctl is-active --quiet p2p-vpn-alpha.service")
        machine.fail("systemctl is-active --quiet p2p-vpn-beta-resolved.service")
        machine.succeed("ip link show pv0")
        machine.fail("ip link show pv1")
        machine.fail("test -e /run/p2p-vpn-beta/resolved-interface")
        machine.fail("resolvectl domain pv1")
        assert_private_query_fails_fast("beta-host.beta.p2p-vpn.internal")
        machine.succeed(
            "resolvectl query alpha-host.alpha.p2p-vpn.internal | grep -Eq '100\\.64\\.|fd00:'"
        )

    with subtest("guard remains until disabled and cleans up its resolver state"):
        machine.succeed("systemctl stop p2p-vpn-alpha.service")
        machine.succeed("systemctl is-active --quiet p2p-vpn-dns-guard.service")
        assert_private_query_fails_fast("alpha-host.alpha.p2p-vpn.internal")
        machine.succeed("systemctl stop p2p-vpn-dns-guard.service")
        machine.fail("ip link show pvdns0")
        machine.fail("test -e /run/p2p-vpn-dns-guard/listener")
        machine.fail("test -e /run/p2p-vpn-dns-guard/owned-interface")
        machine.fail("resolvectl domain pvdns0")
        machine.succeed(
            "test $(systemctl show p2p-vpn-dns-guard.service -p Result --value) = success"
        )
  '';
}
