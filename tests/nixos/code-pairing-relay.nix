{
  self,
  pkgs,
  package,
}:
let
  networkName = "nixos-vm-code-pairing-relay";
  instance = "live-relay";
  membershipKey = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.49.0.1";
    underlayIp = "192.168.63.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.49.0.2";
    underlayIp = "192.168.64.2";
  };
  relay = {
    peerId = "12D3KooWL7CCANbqH1tUaUdKRk2Vz9Tm2S53b1TBDttv7DcaNjRT";
    privateKey = "CAESQJZUbE6ZIDZ0u3iiRlWf1ILXL/26CNxNQqXC6En+RuhCmOR2sBxazL1e7LCug1gmeK0j7WMsthUkiHEG0WvQL6o=";
    vlanA = "192.168.63.254";
    vlanB = "192.168.64.254";
  };

  relayAddressA = "/ip4/${relay.vlanA}/tcp/4001";
  relayAddressB = "/ip4/${relay.vlanB}/tcp/4001";
  relayCircuitA = "${relayAddressA}/p2p/${relay.peerId}/p2p-circuit";
  relayCircuitB = "${relayAddressB}/p2p/${relay.peerId}/p2p-circuit";

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

  vpnNode =
    local: vlan: relayAddress: relayCircuit: inviter:
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = [ vlan ];
      systemd.tmpfiles.rules = [
        "d /run/agenix 0700 root root -"
        "f /run/agenix/p2p-vpn-${instance}-identity 0400 root root - ${local.privateKey}"
        "f /run/agenix/p2p-vpn-${instance}-membership 0400 root root - ${membershipKey}"
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = local.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.${instance} = {
        enable = true;
        inherit networkName;
        privateKeyFile = "/run/agenix/p2p-vpn-${instance}-identity";
        vpnIp = if inviter then local.vpnIp else null;
        membershipKeyFile = if inviter then "/run/agenix/p2p-vpn-${instance}-membership" else null;
        listenAddresses = [ "/ip4/${local.underlayIp}/tcp/4001" ];
        bootstrapPeers = [
          {
            id = relay.peerId;
            address = relayAddress;
          }
        ];
        discovery = {
          mdns = false;
          kademlia = true;
          kademliaProviderAdvertisement = true;
          dcutr = false;
          autonat = false;
        };
        relayReservations = [ relayCircuit ];
        metricsIntervalSeconds = 30;
        controlSocket = "/run/p2p-vpn-${instance}/control.sock";
      };
    };

  relayNode =
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = [
        1
        2
      ];
      systemd.tmpfiles.rules = [
        "f /run/p2p-vpn-relay-identity 0600 root root - ${relay.privateKey}"
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = relay.vlanA;
          prefixLength = 24;
        }
      ];
      networking.interfaces.eth2.ipv4.addresses = [
        {
          address = relay.vlanB;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.relay = {
        enable = true;
        inherit networkName;
        privateKeyFile = "/run/p2p-vpn-relay-identity";
        listenAddresses = [ "/ip4/0.0.0.0/tcp/4001" ];
        discovery = {
          mdns = false;
          kademlia = true;
          kademliaProviderAdvertisement = false;
          dcutr = false;
          autonat = false;
        };
        relayServer = true;
        metricsIntervalSeconds = 30;
        controlSocket = "/run/p2p-vpn-relay/control.sock";
      };
    };

  socket = "/run/p2p-vpn-${instance}/control.sock";
  pair = command: "p2p-vpn pair ${command} --instance ${instance}";
  state = "p2p-vpn daemon-state --socket ${socket}";
  metrics = controlSocket: "p2p-vpn daemon-status --socket ${controlSocket}";
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-code-pairing-relay";

  nodes = {
    node-a = vpnNode nodeA 1 relayAddressA relayCircuitA true;
    node-b = vpnNode nodeB 2 relayAddressB relayCircuitB false;
    relay = relayNode;
  };

  testScript = ''
    start_all()

    relay.wait_for_unit("p2p-vpn-relay.service")
    node_a.wait_for_unit("p2p-vpn-${instance}.service")
    node_b.wait_for_unit("p2p-vpn-${instance}.service")
    relay.wait_for_file("/run/p2p-vpn-relay/control.sock")
    node_a.wait_for_file("${socket}")
    node_b.wait_for_file("${socket}")

    with subtest("edge underlays are isolated and have no configured route to each other"):
        node_a.fail("ping -c 1 -W 1 ${nodeB.underlayIp}")
        node_b.fail("ping -c 1 -W 1 ${nodeA.underlayIp}")
        node_a.succeed("ping -c 1 -W 1 ${relay.vlanA}")
        node_b.succeed("ping -c 1 -W 1 ${relay.vlanB}")
        node_a.succeed("jq -e '.peers == [] and .network.discovery.mdns == false and .network.discovery.kademlia == true' /run/p2p-vpn-${instance}/config.json")
        node_b.succeed("jq -e '.peers == [] and .network.discovery.mdns == false and .network.discovery.kademlia == true' /run/p2p-vpn-${instance}/config.json")

    with subtest("relay accepts both edge reservations before pairing"):
        relay.wait_until_succeeds(
            "${metrics "/run/p2p-vpn-relay/control.sock"} > /tmp/relay-before-pair "
            "&& awk '/^relay_server_reservations_accepted / && $2 >= 2 { found = 1 } END { exit(found ? 0 : 1) }' /tmp/relay-before-pair",
            timeout=90,
        )
        relay.succeed("grep -q '^relay_server_reservations_denied 0$' /tmp/relay-before-pair")

    with subtest("code pairing uses DHT discovery and requires inviter approval"):
        node_a.succeed("${pair "open"} --expires-in-seconds 900 --format json > /tmp/open.json")
        code = node_a.succeed("jq -r .code /tmp/open.json").strip()
        open_operation = node_a.succeed("jq -r .operation_id /tmp/open.json").strip()
        node_b.succeed(
            "${pair "join"} " + repr(code)
            + " --vpn-ip ${nodeB.vpnIp} --timeout-seconds 900 --no-wait --format json > /tmp/join.json"
        )
        join_operation = node_b.succeed("jq -r .operation_id /tmp/join.json").strip()
        node_a.wait_until_succeeds(
            "${pair "status"} " + open_operation
            + " --format json | tee /tmp/open-status.json"
            + " | jq -e '.phase == \"awaiting_approval\" and .candidate.peer_id == \"${nodeB.peerId}\"'",
            timeout=120,
        )
        node_b.wait_until_succeeds(
            "${pair "status"} " + join_operation
            + " --format json | tee /tmp/join-status.json"
            + " | jq -e '.discovery == \"relay\" and .diagnostics.public_lookups >= 1 "
            + "and .diagnostics.public_providers_found >= 1 and .diagnostics.selected_transport == \"relay\"'",
            timeout=60,
        )
        approval = node_a.succeed("jq -r .candidate.approval_id /tmp/open-status.json").strip()
        node_a.succeed(
            "${pair "approve"} " + open_operation + " " + approval
            + " --vpn-ip ${nodeB.vpnIp} --format json > /tmp/approve.json"
        )
        node_a.succeed("jq -e '.phase == \"completed\" and .discovery == \"relay\" and .diagnostics.selected_transport == \"relay\"' /tmp/approve.json")
        node_b.wait_until_succeeds(
            "${pair "status"} " + join_operation
            + " --format json | tee /tmp/join-completed.json"
            + " | jq -e '.phase == \"completed\" and .artifacts_ready and .discovery == \"relay\" "
            + "and .diagnostics.selected_transport == \"relay\"'",
            timeout=120,
        )

    with subtest("paired overlay traffic remains relay-only"):
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=120)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=120)
        node_a.succeed("${state} > /tmp/node-a-relay-state")
        node_b.succeed("${state} > /tmp/node-b-relay-state")
        node_a.succeed("${metrics socket} > /tmp/node-a-relay-metrics")
        node_b.succeed("${metrics socket} > /tmp/node-b-relay-metrics")
        node_a.succeed("awk '/^code_pairing_relay_messages / && $2 >= 1 { found = 1 } END { exit(found ? 0 : 1) }' /tmp/node-a-relay-metrics")
        node_b.succeed("awk '/^code_pairing_relay_messages / && $2 >= 1 { found = 1 } END { exit(found ? 0 : 1) }' /tmp/node-b-relay-metrics")
        node_a.succeed("grep -q 'selected_path circuit_relay .* relay_paths 1' /tmp/node-a-relay-state")
        node_b.succeed("grep -q 'selected_path circuit_relay .* relay_paths 1' /tmp/node-b-relay-state")

    with subtest("durable relayed enrollment recovers across daemon restarts"):
        node_a.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_b.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_a.wait_for_file("${socket}")
        node_b.wait_for_file("${socket}")
        node_a.wait_until_succeeds("${state} | grep -q '${nodeB.peerId}'", timeout=120)
        node_b.wait_until_succeeds("${state} | grep -q '${nodeA.peerId}'", timeout=120)
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=120)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=120)
  '';
}
