{
  self,
  pkgs,
  package,
}:
let
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.46.0.1";
    lanIp = "192.168.51.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.46.0.2";
    lanIp = "192.168.51.2";
    movedIp = "192.168.52.2";
  };
  relay = {
    peerId = "12D3KooWL7CCANbqH1tUaUdKRk2Vz9Tm2S53b1TBDttv7DcaNjRT";
    privateKey = "CAESQJZUbE6ZIDZ0u3iiRlWf1ILXL/26CNxNQqXC6En+RuhCmOR2sBxazL1e7LCug1gmeK0j7WMsthUkiHEG0WvQL6o=";
    vlanLan = "192.168.51.254";
    vlanMoved = "192.168.52.254";
  };

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

  nodeAModule =
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = [ 1 ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = nodeA.lanIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.node-a = {
        enable = true;
        networkName = "nixos-vm-network-move";
        localPeer = nodeA.peerId;
        privateKey = nodeA.privateKey;
        vpnIp = nodeA.vpnIp;
        peers.${nodeB.peerId}.vpnIp = nodeB.vpnIp;
        metricsIntervalSeconds = 5;
        controlSocket = "/run/p2p-vpn-node-a/control.sock";
      };
    };

  nodeBModule =
    { ... }:
    {
      imports = [ common ];
      virtualisation.vlans = [
        1
        2
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = nodeB.lanIp;
          prefixLength = 24;
        }
      ];
      networking.interfaces.eth2.ipv4.addresses = [
        {
          address = nodeB.movedIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.node-b = {
        enable = true;
        networkName = "nixos-vm-network-move";
        localPeer = nodeB.peerId;
        privateKey = nodeB.privateKey;
        vpnIp = nodeB.vpnIp;
        peers.${nodeA.peerId}.vpnIp = nodeA.vpnIp;
        metricsIntervalSeconds = 5;
        controlSocket = "/run/p2p-vpn-node-b/control.sock";
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
          address = relay.vlanLan;
          prefixLength = 24;
        }
      ];
      networking.interfaces.eth2.ipv4.addresses = [
        {
          address = relay.vlanMoved;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.relay = {
        enable = true;
        networkName = "nixos-vm-network-move";
        localPeer = relay.peerId;
        privateKey = relay.privateKey;
        relayServer = true;
        metricsIntervalSeconds = 5;
        controlSocket = "/run/p2p-vpn-relay/control.sock";
      };
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
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-network-move";

  nodes = {
    node-a = nodeAModule;
    node-b = nodeBModule;
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
    node_b.succeed("ip link set eth2 down")

    with subtest("generated VPN configs do not hard-code relay routing"):
        node_a.succeed(
            "jq -e '"
            "(.network | has(\"relay\") | not) "
            "and (.network | has(\"discovery\") | not) "
            "and (.peers == [{\"id\":\"${nodeB.peerId}\",\"vpn_ip\":\"${nodeB.vpnIp}\"}])"
            "' /etc/p2p-vpn/node-a.json"
        )
        node_b.succeed(
            "jq -e '"
            "(.network | has(\"relay\") | not) "
            "and (.network | has(\"discovery\") | not) "
            "and (.peers == [{\"id\":\"${nodeA.peerId}\",\"vpn_ip\":\"${nodeA.vpnIp}\"}])"
            "' /etc/p2p-vpn/node-b.json"
        )
        node_a.succeed(
            "sha256sum /etc/p2p-vpn/node-a.json "
            "| awk '{print $1}' > /tmp/node-a-config.sha256"
        )
        node_b.succeed(
            "sha256sum /etc/p2p-vpn/node-b.json "
            "| awk '{print $1}' > /tmp/node-b-config.sha256"
        )

    with subtest("starts on LAN with direct path"):
        node_a.succeed("${health "node-a"}")
        node_b.succeed("${health "node-b"}")
        node_a.wait_until_succeeds("ping -I pv0 -c 3 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 3 -W 2 ${nodeA.vpnIp}", timeout=90)
        node_a.wait_until_succeeds("${state "node-a"} | grep -q 'selected_path direct_'", timeout=90)
        node_a.wait_until_succeeds("${state "node-a"} | awk '/auto_relay_active_reservations/ { found = 1; reservations = $2 } END { exit found && reservations > 0 ? 0 : 1 }'", timeout=90)
        node_b.wait_until_succeeds("${state "node-b"} | awk '/auto_relay_active_reservations/ { found = 1; reservations = $2 } END { exit found && reservations > 0 ? 0 : 1 }'", timeout=90)

    with subtest("moved peer recovers through relay without config changes"):
        node_b.succeed("ip link set eth2 up")
        node_b.succeed("ip link set eth1 down")
        node_a.fail("ping -c 1 -W 1 ${nodeB.lanIp}")
        node_b.succeed("ping -c 1 -W 1 ${relay.vlanMoved}")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=120)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=120)
        node_a.wait_until_succeeds("${state "node-a"} | tee /tmp/node-a-moved-state | grep -q 'relay_paths 1'", timeout=90)
        node_b.wait_until_succeeds("${state "node-b"} | tee /tmp/node-b-moved-state | grep -q 'relay_paths 1'", timeout=90)
        node_a.succeed("awk '/relay_outbound_circuits_established|relay_inbound_circuits_established/ { total += $2 } END { exit total > 0 ? 0 : 1 }' /tmp/node-a-moved-state")
        node_b.succeed("awk '/relay_outbound_circuits_established|relay_inbound_circuits_established/ { total += $2 } END { exit total > 0 ? 0 : 1 }' /tmp/node-b-moved-state")
        node_a.succeed(
            "test \"$(sha256sum /etc/p2p-vpn/node-a.json | awk '{print $1}')\" "
            "= \"$(cat /tmp/node-a-config.sha256)\""
        )
        node_b.succeed(
            "test \"$(sha256sum /etc/p2p-vpn/node-b.json | awk '{print $1}')\" "
            "= \"$(cat /tmp/node-b-config.sha256)\""
        )

    with subtest("returned peer promotes back to LAN direct path"):
        node_b.succeed("ip link set eth1 up")
        node_a.wait_until_succeeds("ping -c 1 -W 1 ${nodeB.lanIp}", timeout=30)
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=120)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=120)
        node_a.wait_until_succeeds("${state "node-a"} | tee /tmp/node-a-returned-state | grep -q 'selected_path direct_'", timeout=120)
        node_b.wait_until_succeeds("${state "node-b"} | tee /tmp/node-b-returned-state | grep -q 'selected_path direct_'", timeout=120)
        node_a.succeed(
            "test \"$(sha256sum /etc/p2p-vpn/node-a.json | awk '{print $1}')\" "
            "= \"$(cat /tmp/node-a-config.sha256)\""
        )
        node_b.succeed(
            "test \"$(sha256sum /etc/p2p-vpn/node-b.json | awk '{print $1}')\" "
            "= \"$(cat /tmp/node-b-config.sha256)\""
        )
  '';
}
