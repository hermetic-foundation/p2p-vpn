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
    hostname = "mesh-a";
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.50.0.1";
    lanIp = "192.168.65.1";
  };
  nodeB = {
    hostname = "mesh-b";
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.50.0.2";
    lanIp = "192.168.65.2";
  };
  nodeC = {
    hostname = "mesh-c";
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
        dns = {
          enable = true;
          hostname = local.hostname;
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
  zone = "${networkName}.p2p-vpn.internal";
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
    import json
    import shlex

    start_all()

    relay.wait_for_unit("p2p-vpn-relay.service")
    node_a.wait_for_unit("p2p-vpn-${instance}.service")
    node_b.wait_for_unit("p2p-vpn-${instance}.service")
    node_c.wait_for_unit("p2p-vpn-${instance}.service")
    relay.wait_for_file("/run/p2p-vpn-relay/control.sock")
    node_a.wait_for_file("${socket}")
    node_b.wait_for_file("${socket}")
    node_c.wait_for_file("${socket}")
    for node in [node_a, node_b, node_c]:
        node.wait_for_unit("systemd-resolved.service")
        node.wait_for_unit("p2p-vpn-dns-guard.service")
        node.wait_for_unit("p2p-vpn-${instance}-resolved.service")
    node_c.succeed("ip link set eth2 down")

    def pair_with_inviter(inviter, joiner, expected_peer, hostname, vpn_ip, label):
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
            + " --hostname " + hostname + " --vpn-ip " + vpn_ip
            + " --format json > /tmp/approve-" + label + ".json"
        )
        joiner.wait_until_succeeds(
            "${pair "status"} " + join_operation
            + " --format json | jq -e '.phase == \"completed\" and .artifacts_ready'",
            timeout=120,
        )

    def wait_for_record_count(node, count):
        node.wait_until_succeeds(
            "${capabilities} | grep -q '^local capability membership record count: "
            + str(count) + "$'",
            timeout=120,
        )

    def wait_for_three_records(node):
        wait_for_record_count(node, 3)

    def wait_for_records_everywhere(count):
        for node in [node_a, node_b, node_c]:
            wait_for_record_count(node, count)

    def wait_for_membership_state(
        node,
        peer_id,
        expected_state,
        effective_inviter=None,
        original_inviter=None,
    ):
        expression = (
            '.peers[] | select(.peer_id == $peer and .membership.state == $state)'
        )
        arguments = (
            " --arg peer " + shlex.quote(peer_id)
            + " --arg state " + shlex.quote(expected_state)
        )
        if effective_inviter is not None:
            expression += " | select(.membership.effective_inviter.peer_id == $effective)"
            arguments += " --arg effective " + shlex.quote(effective_inviter)
        if original_inviter is not None:
            expression += " | select(.membership.original_inviter.peer_id == $original)"
            arguments += " --arg original " + shlex.quote(original_inviter)
        node.wait_until_succeeds(
            "p2p-vpn peers --instance ${instance} --format json | jq -e"
            + arguments + " " + shlex.quote(expression),
            timeout=120,
        )

    def dns_resolve(name):
        return "p2p-vpn dns resolve " + name + " --socket ${socket} --type a"

    def wait_for_dns(node, hostname, vpn_ip, timeout=120):
        node.wait_until_succeeds(
            dns_resolve(hostname) + " | grep -F 'status=ok' | grep -F '" + vpn_ip + "'",
            timeout=timeout,
        )
        node.wait_until_succeeds(
            "resolvectl query " + hostname + " | grep -F '" + vpn_ip + "'",
            timeout=timeout,
        )
        node.wait_until_succeeds(
            "resolvectl query " + hostname + ".${zone} | grep -F '" + vpn_ip + "'",
            timeout=timeout,
        )

    def wait_for_dns_status(node, name, status, timeout=120):
        node.wait_until_succeeds(
            dns_resolve(name) + " | grep -F 'status=" + status + "'",
            timeout=timeout,
        )

    def peer_fallback_name(node, peer_id):
        output = node.succeed("p2p-vpn dns list --socket ${socket}")
        for line in output.splitlines():
            if not line.startswith("dns_record "):
                continue
            fields = dict(field.split("=", 1) for field in line.split()[1:])
            if fields.get("transport_peer") == peer_id and fields.get("fallback") == "true":
                return fields["name"].split(".", 1)[0]
        raise AssertionError("missing DNS fallback for peer " + peer_id)

    def latest_membership_record(node, issuer_peer, member_peer):
        command = (
            "jq -c --arg issuer " + shlex.quote(issuer_peer)
            + " --arg member " + shlex.quote(member_peer)
            + " '[.records[] | select(.payload.issuer_peer == $issuer "
            + "and .payload.member_peer == $member)] "
            + "| sort_by(.payload.membership_epoch, .payload.sequence) | last' "
            + "/var/lib/p2p-vpn/${instance}/membership-state.json"
        )
        record = json.loads(node.succeed(command))
        assert record is not None
        return record

    def issue_membership_record(
        node,
        issuer_peer,
        member_peer,
        output,
        hostname=None,
        vpn_ip=None,
        expires_at=None,
        revoked=False,
        next_epoch=False,
    ):
        current = latest_membership_record(node, issuer_peer, member_peer)["payload"]
        membership_epoch = current["membership_epoch"] + (1 if next_epoch else 0)
        command = (
            "p2p-vpn membership-record-issue"
            + " --issuer-config /run/p2p-vpn-${instance}/config.json"
            + " --member-peer " + shlex.quote(member_peer)
            + " --member-public-key " + shlex.quote(current["member_public_key"])
            + " --membership-epoch " + str(membership_epoch)
            + " --sequence " + str(current["sequence"] + 1)
            + " --output " + shlex.quote(output)
            + " --force"
        )
        if revoked:
            command += " --revoked"
        else:
            assert hostname is not None
            assert vpn_ip is not None
            command += " --hostname " + shlex.quote(hostname)
            command += " --route-grant " + shlex.quote(vpn_ip + "/32")
            if expires_at is not None:
                command += " --expires-at-unix-seconds " + str(expires_at)
        node.succeed(command)

    def replace_persisted_record(node, issuer_peer, member_peer, record_path, label):
        state_path = "/var/lib/p2p-vpn/${instance}/membership-state.json"
        next_path = "/tmp/membership-state-" + label + ".json"
        node.succeed("systemctl stop p2p-vpn-${instance}.service")
        node.succeed(
            "jq --slurpfile incoming " + shlex.quote(record_path)
            + " --arg issuer " + shlex.quote(issuer_peer)
            + " --arg member " + shlex.quote(member_peer)
            + " '.records = ([.records[] | select(.payload.issuer_peer != $issuer "
            + "or .payload.member_peer != $member)] + $incoming)' "
            + state_path + " > " + next_path
        )
        node.succeed("install -m 0600 " + next_path + " " + state_path)
        node.succeed("systemctl start p2p-vpn-${instance}.service")
        node.wait_for_unit("p2p-vpn-${instance}.service")
        node.wait_for_unit("p2p-vpn-${instance}-resolved.service")
        node.wait_for_file("${socket}")

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
        pair_with_inviter(
            node_a,
            node_b,
            "${nodeB.peerId}",
            "${nodeB.hostname}",
            "${nodeB.vpnIp}",
            "b",
        )
        node_b.wait_until_succeeds(
            "${capabilities} | grep -q '^local capability membership record count: 2$'",
            timeout=120,
        )
        pair_with_inviter(
            node_b,
            node_c,
            "${nodeC.peerId}",
            "${nodeC.hostname}",
            "${nodeC.vpnIp}",
            "c",
        )

    with subtest("A and C converge without pairwise pairing"):
        for node in [node_a, node_b, node_c]:
            wait_for_three_records(node)
        for observer in [node_a, node_b, node_c]:
            for hostname, vpn_ip in [
                ("${nodeA.hostname}", "${nodeA.vpnIp}"),
                ("${nodeB.hostname}", "${nodeB.vpnIp}"),
                ("${nodeC.hostname}", "${nodeC.vpnIp}"),
            ]:
                wait_for_dns(observer, hostname, vpn_ip)
        node_a.wait_until_succeeds(
            "ping -4 -I pv0 -c 5 -W 2 ${nodeC.hostname}", timeout=120
        )
        node_c.wait_until_succeeds(
            "ping -4 -I pv0 -c 5 -W 2 ${nodeA.hostname}.${zone}", timeout=120
        )
        node_a.wait_until_succeeds("${state} | grep -q '${nodeC.peerId}'", timeout=120)
        node_c.wait_until_succeeds("${state} | grep -q '${nodeA.peerId}'", timeout=120)
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
                "jq -e '.version == 2 and .network_name == \"${networkName}\" "
                "and .local_peer == \"" + peer + "\" and (.records | length) == 3' "
                "/var/lib/p2p-vpn/${instance}/membership-state.json"
            )
        expected_inventory = {
            "${nodeA.peerId}": ("${nodeA.hostname}", "${nodeA.vpnIp}"),
            "${nodeB.peerId}": ("${nodeB.hostname}", "${nodeB.vpnIp}"),
            "${nodeC.peerId}": ("${nodeC.hostname}", "${nodeC.vpnIp}"),
        }
        for node, local_peer in [
            (node_a, "${nodeA.peerId}"),
            (node_b, "${nodeB.peerId}"),
            (node_c, "${nodeC.peerId}"),
        ]:
            inventory = json.loads(
                node.succeed("p2p-vpn peers --instance ${instance} --format json")
            )
            assert inventory["schema_version"] == 1
            assert inventory["network"] == "${networkName}"
            assert len(inventory["peers"]) == 3
            by_peer = {peer["peer_id"]: peer for peer in inventory["peers"]}
            assert set(by_peer) == set(expected_inventory)
            for peer_id, (hostname, vpn_ip) in expected_inventory.items():
                peer = by_peer[peer_id]
                assert hostname in peer["hostnames"]
                assert vpn_ip in peer["ipv4"]
                assert peer["local"] == (peer_id == local_peer)
            text = node.succeed("p2p-vpn peers --instance ${instance}")
            for peer_id, (hostname, vpn_ip) in expected_inventory.items():
                assert hostname in text
                assert vpn_ip in text
                assert peer_id in text

    with subtest("B restores C membership and route while every source is offline"):
        node_a.succeed("systemctl stop p2p-vpn-${instance}.service")
        node_c.succeed("systemctl stop p2p-vpn-${instance}.service")
        node_b.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_b.wait_until_succeeds("systemctl is-active p2p-vpn-${instance}.service", timeout=30)
        wait_for_three_records(node_b)
        node_b.succeed("${state} | grep -q '${nodeC.peerId}'")
        node_b.succeed("ip -4 route show ${nodeC.vpnIp}/32 dev pv0 | grep -q '${nodeC.vpnIp}'")
        wait_for_dns(node_b, "${nodeC.hostname}", "${nodeC.vpnIp}")
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
        node_b.wait_until_succeeds(
            "ping -4 -I pv0 -c 5 -W 2 ${nodeC.hostname}.${zone}", timeout=180
        )
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

    with subtest("signed DNS claims disappear automatically after expiry"):
        node_c_fallback = peer_fallback_name(node_a, "${nodeC.peerId}")
        wait_for_dns(node_a, node_c_fallback, "${nodeC.vpnIp}")
        expires_at = int(node_b.succeed("date +%s").strip()) + 30
        issue_membership_record(
            node_b,
            "${nodeB.peerId}",
            "${nodeC.peerId}",
            "/tmp/expire-c.json",
            hostname="${nodeC.hostname}",
            vpn_ip="${nodeC.vpnIp}",
            expires_at=expires_at,
        )
        replace_persisted_record(
            node_b,
            "${nodeB.peerId}",
            "${nodeC.peerId}",
            "/tmp/expire-c.json",
            "expire-c",
        )
        for node in [node_a, node_b, node_c]:
            wait_for_record_count(node, 4)
        wait_for_dns(node_a, "${nodeC.hostname}", "${nodeC.vpnIp}")
        node_a.wait_until_succeeds(
            "jq -e --argjson expires " + str(expires_at)
            + " --arg issuer '${nodeB.peerId}' --arg member '${nodeC.peerId}' "
            + "'.records[] | select(.payload.issuer_peer == $issuer "
            + "and .payload.member_peer == $member "
            + "and .payload.expires_at_unix_seconds == $expires)' "
            + "/var/lib/p2p-vpn/${instance}/membership-state.json",
            timeout=120,
        )
        for node in [node_a, node_b]:
            wait_for_dns_status(node, "${nodeC.hostname}", "nxdomain", timeout=120)
            wait_for_dns_status(node, node_c_fallback, "nxdomain", timeout=120)
            wait_for_record_count(node, 4)
        node_a.wait_until_succeeds(
            "! resolvectl query ${nodeC.hostname}.${zone}", timeout=120
        )
        node_a.wait_until_succeeds(
            "! ip -4 route show ${nodeC.vpnIp}/32 dev pv0 | grep -q '${nodeC.vpnIp}'",
            timeout=120,
        )

    with subtest("dedicated hostname records override inviter membership claims"):
        issue_membership_record(
            node_b,
            "${nodeB.peerId}",
            "${nodeC.peerId}",
            "/tmp/conflict-c.json",
            hostname="${nodeB.hostname}",
            vpn_ip="${nodeC.vpnIp}",
            next_epoch=True,
        )
        replace_persisted_record(
            node_b,
            "${nodeB.peerId}",
            "${nodeC.peerId}",
            "/tmp/conflict-c.json",
            "conflict-c",
        )
        for node in [node_a, node_b, node_c]:
            wait_for_record_count(node, 5)
            wait_for_dns(node, "${nodeB.hostname}", "${nodeB.vpnIp}")
            wait_for_dns(node, "${nodeC.hostname}", "${nodeC.vpnIp}")
        node_a.fail(
            "p2p-vpn dns list --socket ${socket} "
            "| grep -F 'dns_conflict name=${nodeB.hostname}.${zone}.'"
        )
        wait_for_dns(node_a, node_c_fallback, "${nodeC.vpnIp}")

    with subtest("later membership hostname leaves dedicated hostname authoritative"):
        issue_membership_record(
            node_b,
            "${nodeB.peerId}",
            "${nodeC.peerId}",
            "/tmp/rename-c.json",
            hostname="${nodeC.hostname}",
            vpn_ip="${nodeC.vpnIp}",
        )
        replace_persisted_record(
            node_b,
            "${nodeB.peerId}",
            "${nodeC.peerId}",
            "/tmp/rename-c.json",
            "rename-c",
        )
        for node in [node_a, node_b, node_c]:
            wait_for_record_count(node, 6)
            wait_for_dns(node, "${nodeB.hostname}", "${nodeB.vpnIp}")
            wait_for_dns(node, "${nodeC.hostname}", "${nodeC.vpnIp}")

    with subtest("runtime revocation removes names routes and fallback after restart"):
        node_b.succeed(
            "p2p-vpn membership revoke ${nodeC.peerId} --instance ${instance}"
        )
        for node in [node_a, node_b]:
            wait_for_record_count(node, 7)
            wait_for_membership_state(node, "${nodeC.peerId}", "revoked")
            wait_for_dns_status(node, "${nodeC.hostname}", "nxdomain")
            wait_for_dns_status(node, node_c_fallback, "nxdomain")
            node.wait_until_succeeds(
                "! ip -4 route show ${nodeC.vpnIp}/32 dev pv0 | grep -q '${nodeC.vpnIp}'",
                timeout=120,
            )
        node_a.wait_until_succeeds(
            "jq -e --arg issuer '${nodeB.peerId}' --arg member '${nodeC.peerId}' "
            + "'.records[] | select(.payload.issuer_peer == $issuer "
            + "and .payload.member_peer == $member and .payload.revoked)' "
            + "/var/lib/p2p-vpn/${instance}/membership-state.json",
            timeout=120,
        )
        node_a.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_a.wait_for_unit("p2p-vpn-${instance}.service")
        node_a.wait_for_unit("p2p-vpn-${instance}-resolved.service")
        node_a.wait_for_file("${socket}")
        wait_for_record_count(node_a, 7)
        wait_for_membership_state(node_a, "${nodeC.peerId}", "revoked")
        wait_for_dns_status(node_a, "${nodeC.hostname}", "nxdomain")
        wait_for_dns_status(node_a, node_c_fallback, "nxdomain")
        node_a.wait_until_succeeds(
            "! resolvectl query ${nodeC.hostname}.${zone}", timeout=120
        )

    with subtest("revoked member is re-admitted at a higher epoch"):
        revoked_c = latest_membership_record(
            node_a, "${nodeB.peerId}", "${nodeC.peerId}"
        )["payload"]
        pair_with_inviter(
            node_a,
            node_c,
            "${nodeC.peerId}",
            "${nodeC.hostname}",
            "${nodeC.vpnIp}",
            "readmit-c",
        )
        wait_for_records_everywhere(8)
        for node in [node_a, node_b, node_c]:
            wait_for_membership_state(
                node,
                "${nodeC.peerId}",
                "active",
                effective_inviter="${nodeA.peerId}",
                original_inviter="${nodeB.peerId}",
            )
        readmitted_c = latest_membership_record(
            node_a, "${nodeA.peerId}", "${nodeC.peerId}"
        )["payload"]
        assert (
            readmitted_c["membership_epoch"]
            == revoked_c["membership_epoch"] + 1
        )
        wait_for_dns(node_b, "${nodeC.hostname}", "${nodeC.vpnIp}")

    with subtest("member C revokes inviter B without cascading to C"):
        node_c.succeed(
            "p2p-vpn membership revoke ${nodeB.peerId} --instance ${instance}"
        )
        for node in [node_a, node_c]:
            wait_for_record_count(node, 9)
            wait_for_membership_state(node, "${nodeB.peerId}", "revoked")
            wait_for_membership_state(
                node,
                "${nodeC.peerId}",
                "active",
                effective_inviter="${nodeA.peerId}",
                original_inviter="${nodeB.peerId}",
            )
            wait_for_dns(node, "${nodeC.hostname}", "${nodeC.vpnIp}")
        node_a.wait_until_succeeds(
            "ping -4 -I pv0 -c 5 -W 2 ${nodeC.hostname}", timeout=120
        )

    with subtest("remaining member C re-admits B and preserves original inviter"):
        revoked_b = latest_membership_record(
            node_c, "${nodeC.peerId}", "${nodeB.peerId}"
        )["payload"]
        pair_with_inviter(
            node_c,
            node_b,
            "${nodeB.peerId}",
            "${nodeB.hostname}",
            "${nodeB.vpnIp}",
            "readmit-b-by-c",
        )
        wait_for_records_everywhere(10)
        for node in [node_a, node_b, node_c]:
            wait_for_membership_state(
                node,
                "${nodeB.peerId}",
                "active",
                effective_inviter="${nodeC.peerId}",
                original_inviter="${nodeA.peerId}",
            )
        readmitted_b = latest_membership_record(
            node_c, "${nodeC.peerId}", "${nodeB.peerId}"
        )["payload"]
        assert (
            readmitted_b["membership_epoch"]
            == revoked_b["membership_epoch"] + 1
        )

    with subtest("self-resignation is global and does not stop the network"):
        node_b.succeed("p2p-vpn membership resign --instance ${instance}")
        wait_for_records_everywhere(11)
        for node in [node_a, node_c]:
            wait_for_membership_state(node, "${nodeB.peerId}", "revoked")
            wait_for_membership_state(node, "${nodeC.peerId}", "active")
            wait_for_dns(node, "${nodeC.hostname}", "${nodeC.vpnIp}")
        node_a.wait_until_succeeds(
            "ping -4 -I pv0 -c 5 -W 2 ${nodeC.hostname}.${zone}", timeout=120
        )

    with subtest("resigned member can pair again at the next epoch"):
        pair_with_inviter(
            node_a,
            node_b,
            "${nodeB.peerId}",
            "${nodeB.hostname}",
            "${nodeB.vpnIp}",
            "readmit-b-after-resign",
        )
        wait_for_records_everywhere(12)
        for node in [node_a, node_b, node_c]:
            wait_for_membership_state(
                node,
                "${nodeB.peerId}",
                "active",
                effective_inviter="${nodeA.peerId}",
                original_inviter="${nodeA.peerId}",
            )
            wait_for_dns(node, "${nodeB.hostname}", "${nodeB.vpnIp}")
        node_c.wait_until_succeeds(
            "ping -4 -I pv0 -c 5 -W 2 ${nodeB.hostname}", timeout=120
        )
  '';
}
