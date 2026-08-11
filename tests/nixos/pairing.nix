{
  self,
  pkgs,
  package,
}:
let
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.47.0.1";
    underlayIp = "192.168.61.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.47.0.2";
    underlayIp = "192.168.61.2";
  };

  common =
    { ... }:
    {
      imports = [ self.nixosModules.default ];

      system.stateVersion = "25.11";
      virtualisation.vlans = [ 1 ];
      networking.useDHCP = false;
      networking.firewall.enable = false;
      environment.systemPackages = [
        package
        pkgs.iproute2
        pkgs.iputils
        pkgs.jq
      ];
    };

  nodeAModule =
    { ... }:
    {
      imports = [ common ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = nodeA.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.node-a = {
        enable = true;
        settings = {
          network = {
            name = "nixos-vm-pairing";
            private_key = nodeA.privateKey;
            vpn_ip = nodeA.vpnIp;
            listen_addresses = [ "/ip4/${nodeA.underlayIp}/tcp/4001" ];
          };
          peers = [ ];
        };
        metricsIntervalSeconds = 1;
        controlSocket = "/run/p2p-vpn-node-a/control.sock";
      };
    };

  nodeBModule =
    { ... }:
    {
      imports = [ common ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = nodeB.underlayIp;
          prefixLength = 24;
        }
      ];
    };

  health =
    name:
    "p2p-vpn daemon-health "
    + "--socket /run/p2p-vpn-${name}/control.sock "
    + "--timeout-seconds 5 "
    + "--wait-seconds 90 "
    + "--require-validated-peers "
    + "--require-supported-paths";

  state =
    name:
    "p2p-vpn daemon-state "
    + "--socket /run/p2p-vpn-${name}/control.sock";

  status =
    name:
    "p2p-vpn daemon-status "
    + "--socket /run/p2p-vpn-${name}/control.sock";

  routes =
    name:
    "p2p-vpn daemon-routes "
    + "--socket /run/p2p-vpn-${name}/control.sock";

  runtimePath = pkgs.lib.makeBinPath [
    pkgs.iproute2
  ];
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-pairing";

  nodes = {
    node-a = nodeAModule;
    node-b = nodeBModule;
  };

  testScript = ''
    start_all()

    node_a.wait_for_unit("multi-user.target")
    node_b.wait_for_unit("multi-user.target")
    node_a.wait_for_unit("p2p-vpn-node-a.service")
    node_a.wait_for_file("/run/p2p-vpn-node-a/control.sock")

    with subtest("inviter starts without a preconfigured joiner"):
        node_a.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-pairing\" "
            "and .network.vpn_ip == \"${nodeA.vpnIp}\" "
            "and .network.listen_addresses == [\"/ip4/${nodeA.underlayIp}/tcp/4001\"] "
            "and .peers == []"
            "' /etc/p2p-vpn/node-a.json"
        )
        node_a.succeed("${state "node-a"} | tee /tmp/node-a-initial-state")
        node_a.fail("grep -q '${nodeB.peerId}' /tmp/node-a-initial-state")

    with subtest("pair offer creates inspectable one-time URI"):
        node_a.succeed(
            "p2p-vpn pair offer "
            "--config /etc/p2p-vpn/node-a.json "
            "--output /tmp/node-a.pair "
            "--expires-in-seconds 600 "
            "--force"
        )
        node_a.succeed("test -s /tmp/node-a.pair")
        node_a.succeed(
            "p2p-vpn pair inspect /tmp/node-a.pair "
            "| tee /tmp/node-a-pair-inspect"
        )
        node_a.succeed("grep -q '^pairing offer: valid$' /tmp/node-a-pair-inspect")
        node_a.succeed("grep -q '^inviter address hints: 1$' /tmp/node-a-pair-inspect")
        node_a.succeed("grep -q '^inviter address: /ip4/${nodeA.underlayIp}/tcp/4001$' /tmp/node-a-pair-inspect")
        node_a.succeed("grep -q '^rendezvous token: hidden$' /tmp/node-a-pair-inspect")

    with subtest("new node accepts with only URI and its identity"):
        offer_uri = node_a.succeed("cat /tmp/node-a.pair").strip()
        node_b.succeed("printf '%s\n' " + repr(offer_uri) + " > /tmp/node-a.pair")
        node_b.succeed(
            "p2p-vpn pair accept /tmp/node-a.pair "
            "--output /tmp/node-b.json "
            "--nixos-output /tmp/node-b.nix "
            "--nixos-instance nixos-vm-pairing "
            "--private-key '${nodeB.privateKey}' "
            "--interface pv0 "
            "--vpn-ip ${nodeB.vpnIp} "
            "--timeout-seconds 30 "
            "--force "
            "| tee /tmp/node-b-pair-accept"
        )
        node_b.succeed("test -s /tmp/node-b.json")
        node_b.succeed("test -s /tmp/node-b.nix")
        node_b.succeed("grep -q '^wrote /tmp/node-b.json$' /tmp/node-b-pair-accept")
        node_b.succeed("grep -q '^wrote /tmp/node-b.nix$' /tmp/node-b-pair-accept")
        node_b.succeed("grep -q '^paired with: ${nodeA.peerId}$' /tmp/node-b-pair-accept")
        node_b.succeed("grep -q 'services.p2p-vpn.instances.\"nixos-vm-pairing\"' /tmp/node-b.nix")
        node_b.succeed("grep -q 'configFile = \"/tmp/node-b.json\";' /tmp/node-b.nix")
        node_b.succeed("jq -e '.network.name == \"nixos-vm-pairing\"' /tmp/node-b.json")
        node_b.succeed("jq -e '.network.vpn_ip == \"${nodeB.vpnIp}\"' /tmp/node-b.json")
        node_b.succeed("jq -e '(.interface | has(\"name\") | not)' /tmp/node-b.json")
        node_b.succeed("jq -e '(.network | has(\"member_records\"))' /tmp/node-b.json")
        node_b.succeed("jq -e '(.network.member_records | length) >= 1' /tmp/node-b.json")
        node_b.succeed("jq -e '(.network | has(\"membership_key\") | not)' /tmp/node-b.json")
        node_b.succeed("jq -e '(.peers | length) == 1' /tmp/node-b.json")
        node_b.succeed("jq -e '.peers[0].id == \"${nodeA.peerId}\"' /tmp/node-b.json")
        node_b.succeed("jq -e '(.peers[0].addresses | length) >= 1' /tmp/node-b.json")
        node_b.succeed("p2p-vpn routes --config /tmp/node-b.json | tee /tmp/node-b-config-routes")
        node_b.succeed("grep -q 'route: ${nodeA.vpnIp}/32 owner peer ${nodeA.peerId}' /tmp/node-b-config-routes")

    with subtest("replayed URI is rejected with diagnostics"):
        node_b.fail(
            "p2p-vpn pair accept /tmp/node-a.pair "
            "--output /tmp/node-b-replay.json "
            "--private-key '${nodeB.privateKey}' "
            "--interface pv0 "
            "--vpn-ip ${nodeB.vpnIp} "
            "--timeout-seconds 10 "
            "--force "
            "> /tmp/node-b-replay.out 2> /tmp/node-b-replay.err"
        )
        node_b.succeed("grep -q 'live pairing exchange failed' /tmp/node-b-replay.err")
        node_b.succeed("grep -q 'pairing diagnostics:' /tmp/node-b-replay.err")

    with subtest("accepted membership installs live on inviter"):
        node_a.wait_until_succeeds(
            "${state "node-a"} | tee /tmp/node-a-paired-state | grep -q '${nodeB.peerId}'",
            timeout=30
        )
        node_a.succeed("${status "node-a"} | tee /tmp/node-a-paired-status")
        node_a.succeed(
            "awk '/^pairing_requests_accepted / && $2 >= 1 { found = 1 } END { exit(found ? 0 : 1) }' "
            "/tmp/node-a-paired-status"
        )
        node_a.succeed(
            "awk '/^pairing_reject_replayed_token / && $2 >= 1 { found = 1 } END { exit(found ? 0 : 1) }' "
            "/tmp/node-a-paired-status"
        )
        node_a.succeed("${routes "node-a"} | tee /tmp/node-a-paired-routes")

    with subtest("generated joiner config starts and carries overlay traffic"):
        node_b.succeed("mkdir -p /run/p2p-vpn-node-b")
        node_b.succeed(
            "systemd-run "
            "--unit=p2p-vpn-node-b "
            "--property=Type=simple "
            "--property=Restart=no "
            "--setenv=PATH=${runtimePath} "
            "${package}/bin/p2p-vpn up "
            "--config /tmp/node-b.json "
            "--metrics-interval-seconds 1 "
            "--control-socket /run/p2p-vpn-node-b/control.sock"
        )
        node_b.wait_for_unit("p2p-vpn-node-b.service")
        node_b.wait_for_file("/run/p2p-vpn-node-b/control.sock")
        node_b.succeed("${state "node-b"} | tee /tmp/node-b-started-state")
        node_b.succeed("${status "node-b"} | tee /tmp/node-b-started-status")
        node_b.succeed("${routes "node-b"} | tee /tmp/node-b-started-routes")
        node_a.succeed("${health "node-a"} | tee /tmp/node-a-health")
        node_b.succeed("${health "node-b"} | tee /tmp/node-b-health")
        node_a.succeed("grep -q '^daemon_health_ready true$' /tmp/node-a-health")
        node_b.succeed("grep -q '^daemon_health_ready true$' /tmp/node-b-health")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)
  '';
}
