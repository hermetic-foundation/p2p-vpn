{
  self,
  pkgs,
  package,
}:
let
  networkName = "nixos-vm-membership-convergence";
  instance = "mesh";
  membershipKey = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.50.0.1";
    lanIp = "192.168.65.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.50.0.2";
    lanIp = "192.168.65.2";
  };
  nodeC = {
    peerId = "12D3KooWLddBSSTE39542CEtbZG2CN3sJu6bkxbYTxy19ZterSbq";
    privateKey = "CAESQIrtTosx6EJrcoppcZL+8ijzijdnHHqOcIXNkjeVZVK5oLAkMkt53aaP6enVFBdg7I1jMVnywfWmfgEzzEH4bEA=";
    vpnIp = "10.50.0.3";
    lanIp = "192.168.65.3";
    movedIp = "192.168.66.3";
  };
  relay = {
    peerId = "12D3KooWL7CCANbqH1tUaUdKRk2Vz9Tm2S53b1TBDttv7DcaNjRT";
    privateKey = "CAESQJZUbE6ZIDZ0u3iiRlWf1ILXL/26CNxNQqXC6En+RuhCmOR2sBxazL1e7LCug1gmeK0j7WMsthUkiHEG0WvQL6o=";
    lanIp = "192.168.65.254";
    movedIp = "192.168.66.254";
  };

  relayAddressLan = "/ip4/${relay.lanIp}/tcp/4001";
  relayAddressMoved = "/ip4/${relay.movedIp}/tcp/4001";
  relayCircuitLan = "${relayAddressLan}/p2p/${relay.peerId}/p2p-circuit";
  relayCircuitMoved = "${relayAddressMoved}/p2p/${relay.peerId}/p2p-circuit";

  common =
    { ... }:
    {
      imports = [ self.nixosModules.default ];

      system.stateVersion = "25.11";
      networking.useDHCP = false;
      networking.firewall.enable = false;
      environment.systemPackages = [
        package
        pkgs.iproute2
        pkgs.iputils
        pkgs.jq
      ];
    };

  edgeNode =
    local: root: movable:
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = if movable then [ 1 2 ] else [ 1 ];
      systemd.tmpfiles.rules = [
        "d /run/agenix 0700 root root -"
        "f /run/agenix/p2p-vpn-${instance}-identity 0400 root root - ${local.privateKey}"
        "f /run/agenix/p2p-vpn-${instance}-membership 0400 root root - ${membershipKey}"
      ];
      networking.interfaces = {
        eth1.ipv4.addresses = [
          {
            address = local.lanIp;
            prefixLength = 24;
          }
        ];
      } // pkgs.lib.optionalAttrs movable {
        eth2.ipv4.addresses = [
          {
            address = local.movedIp;
            prefixLength = 24;
          }
        ];
      };

      services.p2p-vpn.instances.${instance} = {
        enable = true;
        inherit networkName;
        privateKeyFile = "/run/agenix/p2p-vpn-${instance}-identity";
        vpnIp = if root then local.vpnIp else null;
        membershipKeyFile = if root then "/run/agenix/p2p-vpn-${instance}-membership" else null;
        listenAddresses = [ "/ip4/0.0.0.0/tcp/4001" ];
        bootstrapPeers = [
          {
            id = relay.peerId;
            address = relayAddressLan;
          }
        ] ++ pkgs.lib.optionals movable [
          {
            id = relay.peerId;
            address = relayAddressMoved;
          }
        ];
        discovery = {
          mdns = true;
          kademlia = true;
          kademliaProviderAdvertisement = true;
          dcutr = false;
          autonat = false;
        };
        relayReservations = [ relayCircuitLan ] ++ pkgs.lib.optionals movable [ relayCircuitMoved ];
        autoRelay = {
          maxCandidates = 0;
          maxReservations = 0;
          retryIntervalSeconds = 30;
        };
        metricsIntervalSeconds = 10;
        controlSocket = "/run/p2p-vpn-${instance}/control.sock";
      };
    };

  relayNode =
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = [ 1 2 ];
      systemd.tmpfiles.rules = [
        "f /run/p2p-vpn-relay.key 0600 root root - ${relay.privateKey}"
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = relay.lanIp;
          prefixLength = 24;
        }
      ];
      networking.interfaces.eth2.ipv4.addresses = [
        {
          address = relay.movedIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.relay = {
        enable = true;
        inherit networkName;
        privateKeyFile = "/run/p2p-vpn-relay.key";
        listenAddresses = [ "/ip4/0.0.0.0/tcp/4001" ];
        discovery = {
          mdns = false;
          kademlia = true;
          kademliaProviderAdvertisement = false;
          dcutr = false;
          autonat = false;
        };
        relayServer = true;
        metricsIntervalSeconds = 10;
        controlSocket = "/run/p2p-vpn-relay/control.sock";
      };
    };

  socket = "/run/p2p-vpn-${instance}/control.sock";
  pair = command: "p2p-vpn pair ${command} --instance ${instance}";
  state = "p2p-vpn daemon-state --socket ${socket}";
  capabilities = "p2p-vpn daemon-capabilities --socket ${socket}";
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-membership-convergence";

  nodes = {
    node-a = edgeNode nodeA true false;
    node-b = edgeNode nodeB false false;
    node-c = edgeNode nodeC false true;
    relay = relayNode;
  };

  testScript = ''
    start_all()

    relay.wait_for_unit("p2p-vpn-relay.service")
    node_a.wait_for_unit("p2p-vpn-${instance}.service")
    node_b.wait_for_unit("p2p-vpn-${instance}.service")
    node_c.wait_for_unit("p2p-vpn-${instance}.service")
    relay.wait_for_file("/run/p2p-vpn-relay/control.sock")
    node_a.wait_for_file("${socket}")
    node_b.wait_for_file("${socket}")
    node_c.wait_for_file("${socket}")
    node_c.succeed("ip link set eth2 down")

    def pair_with_inviter(inviter, joiner, expected_peer, vpn_ip, label):
        open_file = "/tmp/open-%s.json" % label
        join_file = "/tmp/join-%s.json" % label
        open_status = "/tmp/open-status-%s.json" % label
        inviter.succeed("${pair "open"} --expires-in-seconds 900 --format json > " + open_file)
        code = inviter.succeed("jq -r .code " + open_file).strip()
        open_operation = inviter.succeed("jq -r .operation_id " + open_file).strip()
        joiner.succeed(
            "${pair "join"} " + repr(code) + " --vpn-ip " + vpn_ip
            + " --timeout-seconds 900 --no-wait --format json > " + join_file
        )
        join_operation = joiner.succeed("jq -r .operation_id " + join_file).strip()
        inviter.wait_until_succeeds(
            "${pair "status"} " + open_operation + " --format json | tee " + open_status
            + " | jq -e '.phase == \"awaiting_approval\" and .candidate.peer_id == \""
            + expected_peer + "\"'",
            timeout=120,
        )
        approval = inviter.succeed("jq -r .candidate.approval_id " + open_status).strip()
        inviter.succeed(
            "${pair "approve"} " + open_operation + " " + approval
            + " --vpn-ip " + vpn_ip + " --format json > /tmp/approve-" + label + ".json"
        )
        joiner.wait_until_succeeds(
            "${pair "status"} " + join_operation
            + " --format json | jq -e '.phase == \"completed\" and .artifacts_ready'",
            timeout=120,
        )

    def wait_for_three_records(node):
        node.wait_until_succeeds(
            "${capabilities} | grep -q '^local capability membership record count: 3$'",
            timeout=120,
        )

    def assert_metric_never_exceeds(node, metric, maximum):
        node.succeed(
            "journalctl -u p2p-vpn-${instance}.service -b --no-pager"
            " | awk '/  " + metric + " / { found=1; if ($NF > maximum) maximum=$NF }"
            " END { exit !(found && maximum <= " + str(maximum) + ") }'"
        )

    with subtest("all edge configs are peerless and use durable state"):
        for node in [node_a, node_b, node_c]:
            node.succeed("jq -e '.peers == []' /run/p2p-vpn-${instance}/config.json")
            node.succeed("test $(stat -c %a /var/lib/p2p-vpn/${instance}/membership-state.json) = 600")
            node.succeed(
                "sha256sum /run/p2p-vpn-${instance}/config.json | awk '{print $1}'"
                " > /tmp/config.sha256"
            )

    with subtest("root A admits B, then delegated member B admits C"):
        pair_with_inviter(node_a, node_b, "${nodeB.peerId}", "${nodeB.vpnIp}", "b")
        node_b.wait_until_succeeds(
            "${capabilities} | grep -q '^local capability membership record count: 2$'",
            timeout=120,
        )
        pair_with_inviter(node_b, node_c, "${nodeC.peerId}", "${nodeC.vpnIp}", "c")

    with subtest("B and C converge without pairwise pairing"):
        for node in [node_a, node_b, node_c]:
            wait_for_three_records(node)
        node_b.wait_until_succeeds("${state} | grep -q '${nodeC.peerId}'", timeout=120)
        node_c.wait_until_succeeds("${state} | grep -q '${nodeB.peerId}'", timeout=120)
        node_b.wait_until_succeeds(
            "ip -4 route show ${nodeC.vpnIp}/32 dev pv0 | grep -q '${nodeC.vpnIp}'",
            timeout=120,
        )
        node_c.wait_until_succeeds(
            "ip -4 route show ${nodeB.vpnIp}/32 dev pv0 | grep -q '${nodeB.vpnIp}'",
            timeout=120,
        )
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeC.vpnIp}", timeout=120)
        node_c.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=120)
        node_b.wait_until_succeeds(
            "${state} | grep -E 'peer state: [^ ]+ transport ${nodeC.peerId} .*selected_path direct_'",
            timeout=120,
        )
        node_c.wait_until_succeeds(
            "${state} | grep -E 'peer state: [^ ]+ transport ${nodeB.peerId} .*selected_path direct_'",
            timeout=120,
        )
        for node, peer in [
            (node_a, "${nodeA.peerId}"),
            (node_b, "${nodeB.peerId}"),
            (node_c, "${nodeC.peerId}"),
        ]:
            node.succeed(
                "jq -e '.version == 1 and .network_name == \"${networkName}\" "
                "and .local_peer == \"" + peer + "\" and (.records | length) == 3' "
                "/var/lib/p2p-vpn/${instance}/membership-state.json"
            )

    with subtest("B restores C membership and route while every source is offline"):
        node_a.succeed("systemctl stop p2p-vpn-${instance}.service")
        node_c.succeed("systemctl stop p2p-vpn-${instance}.service")
        node_b.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_b.wait_until_succeeds("systemctl is-active p2p-vpn-${instance}.service", timeout=30)
        wait_for_three_records(node_b)
        node_b.succeed("${state} | grep -q '${nodeC.peerId}'")
        node_b.succeed("ip -4 route show ${nodeC.vpnIp}/32 dev pv0 | grep -q '${nodeC.vpnIp}'")
        node_b.succeed(
            "journalctl -u p2p-vpn-${instance}.service -b --no-pager"
            " | grep -q 'event=membership_state_loaded'"
        )

    with subtest("simultaneous recovery reconnects the full mesh"):
        node_b.succeed("systemctl stop p2p-vpn-${instance}.service")
        node_a.succeed("systemctl start p2p-vpn-${instance}.service")
        node_b.succeed("systemctl start p2p-vpn-${instance}.service")
        node_c.succeed("systemctl start p2p-vpn-${instance}.service")
        for node in [node_a, node_b, node_c]:
            node.wait_until_succeeds("systemctl is-active p2p-vpn-${instance}.service", timeout=30)
            wait_for_three_records(node)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeC.vpnIp}", timeout=120)
        node_c.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=120)

    with subtest("moving C to an isolated VLAN falls back through the relay"):
        node_c.succeed("ip link set eth2 up")
        node_c.succeed("ping -c 1 -W 2 ${relay.movedIp}")
        node_c.succeed("ip link set eth1 down")
        node_b.fail("ping -c 1 -W 1 ${nodeC.lanIp}")
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeC.vpnIp}", timeout=180)
        node_c.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=180)
        node_b.wait_until_succeeds(
            "${state} | grep -E 'peer state: [^ ]+ transport ${nodeC.peerId} .*selected_path circuit_relay .*relay_paths [1-9]'",
            timeout=180,
        )
        node_c.wait_until_succeeds(
            "${state} | grep -E 'peer state: [^ ]+ transport ${nodeB.peerId} .*selected_path circuit_relay .*relay_paths [1-9]'",
            timeout=180,
        )

    with subtest("relay recovery survives a cold daemon restart"):
        for node in [node_a, node_b, node_c]:
            node.succeed("systemctl restart p2p-vpn-${instance}.service")
        for node in [node_a, node_b, node_c]:
            node.wait_until_succeeds("systemctl is-active p2p-vpn-${instance}.service", timeout=30)
            wait_for_three_records(node)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeC.vpnIp}", timeout=180)
        node_c.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=180)

    with subtest("returning C to LAN promotes the direct path without config changes"):
        node_c.succeed("ip link set eth1 up")
        node_c.succeed("ip link set eth2 down")
        node_b.wait_until_succeeds("ping -c 1 -W 2 ${nodeC.lanIp}", timeout=30)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeC.vpnIp}", timeout=180)
        node_c.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=180)
        node_b.wait_until_succeeds(
            "${state} | grep -E 'peer state: [^ ]+ transport ${nodeC.peerId} .*selected_path direct_'",
            timeout=180,
        )
        node_c.wait_until_succeeds(
            "${state} | grep -E 'peer state: [^ ]+ transport ${nodeB.peerId} .*selected_path direct_'",
            timeout=180,
        )
        for node in [node_a, node_b, node_c]:
            node.succeed(
                "test \"$(sha256sum /run/p2p-vpn-${instance}/config.json | awk '{print $1}')\""
                " = \"$(cat /tmp/config.sha256)\""
            )

    with subtest("relay and peer recovery stay bounded under failure pressure"):
        relay.fail(
            "journalctl -u p2p-vpn-relay.service -b --no-pager"
            " | grep -q 'ResourceLimitExceeded'"
        )
        for node in [node_a, node_b, node_c]:
            assert_metric_never_exceeds(node, "packet_plane_path_recovery_dial_attempts", 128)
            assert_metric_never_exceeds(node, "kademlia_provider_lookups", 128)
            assert_metric_never_exceeds(node, "redial_attempts", 128)
  '';
}
