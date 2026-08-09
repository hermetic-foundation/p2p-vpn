{
  self,
  pkgs,
  package,
}:
let
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.45.0.1";
    underlayIp = "192.168.41.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.45.0.2";
    underlayIp = "192.168.42.2";
  };
  relay = {
    peerId = "12D3KooWL7CCANbqH1tUaUdKRk2Vz9Tm2S53b1TBDttv7DcaNjRT";
    privateKey = "CAESQJZUbE6ZIDZ0u3iiRlWf1ILXL/26CNxNQqXC6En+RuhCmOR2sBxazL1e7LCug1gmeK0j7WMsthUkiHEG0WvQL6o=";
    vlanA = "192.168.41.254";
    vlanB = "192.168.42.254";
  };

  relayForA = "/ip4/${relay.vlanA}/tcp/4001/p2p/${relay.peerId}/p2p-circuit";
  relayForB = "/ip4/${relay.vlanB}/tcp/4001/p2p/${relay.peerId}/p2p-circuit";

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
      ];
    };

  vpnNode =
    name: vlan: local: remote: localRelay: remoteRelay:
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = [ vlan ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = local.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.${name} = {
        enable = true;
        networkName = "nixos-vm-forced-relay";
        localPeer = local.peerId;
        privateKey = local.privateKey;
        vpnIp = local.vpnIp;
        discovery = {
          mdns = false;
          kademlia = false;
          kademliaProviderAdvertisement = false;
          dcutr = false;
          autonat = false;
        };
        relayReservations = [ localRelay ];
        peers.${remote.peerId} = {
          vpnIp = remote.vpnIp;
          addresses = [ "${remoteRelay}/p2p/${remote.peerId}" ];
        };
        metricsIntervalSeconds = 10;
        controlSocket = "/run/p2p-vpn-${name}/control.sock";
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
        networkName = "nixos-vm-forced-relay";
        localPeer = relay.peerId;
        privateKey = relay.privateKey;
        discovery = {
          mdns = false;
          kademlia = false;
          kademliaProviderAdvertisement = false;
          dcutr = false;
          autonat = false;
        };
        relayServer = true;
        metricsIntervalSeconds = 10;
        controlSocket = "/run/p2p-vpn-relay/control.sock";
      };
    };

  health =
    name:
    "p2p-vpn daemon-health "
    + "--socket /run/p2p-vpn-${name}/control.sock "
    + "--timeout-seconds 5 "
    + "--wait-seconds 60 "
    + "--require-validated-peers "
    + "--require-supported-paths";
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-forced-relay";

  nodes = {
    node-a = vpnNode "node-a" 1 nodeA nodeB relayForA relayForA;
    node-b = vpnNode "node-b" 2 nodeB nodeA relayForB relayForB;
    relay = relayNode;
  };

  testScript = ''
    start_all()

    relay.wait_for_unit("multi-user.target")
    node_a.wait_for_unit("multi-user.target")
    node_b.wait_for_unit("multi-user.target")
    relay.wait_for_unit("p2p-vpn-relay.service")
    node_a.wait_for_unit("p2p-vpn-node-a.service")
    node_b.wait_for_unit("p2p-vpn-node-b.service")
    node_a.wait_for_file("/run/p2p-vpn-node-a/control.sock")
    node_b.wait_for_file("/run/p2p-vpn-node-b/control.sock")

    with subtest("underlay cannot bypass relay"):
        node_a.fail("ping -c 1 -W 1 ${nodeB.underlayIp}")
        node_b.fail("ping -c 1 -W 1 ${nodeA.underlayIp}")
        node_a.succeed("ping -c 1 -W 1 ${relay.vlanA}")
        node_b.succeed("ping -c 1 -W 1 ${relay.vlanB}")

    with subtest("minimal vpnIp configs converge through relay"):
        node_a.succeed("${health "node-a"} | tee /tmp/node-a-health")
        node_b.succeed("${health "node-b"} | tee /tmp/node-b-health")
        node_a.succeed("grep -q '^daemon_health_ready true$' /tmp/node-a-health")
        node_b.succeed("grep -q '^daemon_health_ready true$' /tmp/node-b-health")

    with subtest("pv0 traffic crosses circuit relay"):
        node_a.succeed("ip -brief addr show pv0 | tee /tmp/node-a-pv0")
        node_b.succeed("ip -brief addr show pv0 | tee /tmp/node-b-pv0")
        node_a.succeed("grep -q '${nodeA.vpnIp}' /tmp/node-a-pv0")
        node_b.succeed("grep -q '${nodeB.vpnIp}' /tmp/node-b-pv0")
        node_a.succeed("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}")
        node_b.succeed("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}")

    with subtest("relay circuits were used"):
        node_a.succeed(
            "p2p-vpn daemon-state "
            "--socket /run/p2p-vpn-node-a/control.sock "
            "| tee /tmp/node-a-state"
        )
        node_b.succeed(
            "p2p-vpn daemon-state "
            "--socket /run/p2p-vpn-node-b/control.sock "
            "| tee /tmp/node-b-state"
        )
        node_a.succeed("awk '/relay_inbound_circuits_established/ { found = ($2 > 0) } END { exit found ? 0 : 1 }' /tmp/node-a-state")
        node_b.succeed("awk '/relay_outbound_circuits_established/ { found = ($2 > 0) } END { exit found ? 0 : 1 }' /tmp/node-b-state")
        node_a.succeed("grep -q 'relay_paths 1' /tmp/node-a-state")
        node_b.succeed("grep -q 'relay_paths 1' /tmp/node-b-state")
  '';
}
