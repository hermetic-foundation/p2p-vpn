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

  quicAddress = node: "/ip4/${node.underlayIp}/udp/4001/quic-v1/p2p/${node.peerId}";

  common =
    { ... }:
    {
      imports = [ self.nixosModules.default ];

      system.stateVersion = "25.11";
      virtualisation.vlans = [ 1 ];
      networking.firewall.enable = false;
      networking.useDHCP = false;
      environment.systemPackages = [
        package
        pkgs.iproute2
        pkgs.iputils
        pkgs.jq
      ];
    };

  nodeModule =
    name: local: remote:
    { ... }:
    {
      imports = [ common ];
      systemd.tmpfiles.rules = [
        "f /run/p2p-vpn-test-${name}.key 0600 root root - ${local.privateKey}"
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = local.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.${name} = {
        enable = true;
        networkName = "nixos-vm-quic-stream";
        localPeer = local.peerId;
        privateKeyFile = "/run/p2p-vpn-test-${name}.key";
        vpnIp = local.vpnIp;
        listenAddresses = [ "/ip4/0.0.0.0/udp/4001/quic-v1" ];
        discovery = {
          mdns = false;
          kademlia = false;
          kademliaProviderAdvertisement = false;
          dcutr = false;
          autonat = false;
        };
        packetPlane = {
          listen = [ ];
          externalEndpoints = [ ];
          quicListen = [ ];
          quicExternalEndpoints = [ ];
        };
        peers.${remote.peerId} = {
          vpnIp = remote.vpnIp;
          addresses = [ (quicAddress remote) ];
        };
        metricsIntervalSeconds = 1;
        controlSocket = "/run/p2p-vpn-${name}/control.sock";
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

  state = name: "p2p-vpn daemon-state " + "--socket /run/p2p-vpn-${name}/control.sock";

  paths = name: "p2p-vpn daemon-paths " + "--socket /run/p2p-vpn-${name}/control.sock";
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-quic-stream";

  nodes = {
    node-a = nodeModule "node-a" nodeA nodeB;
    node-b = nodeModule "node-b" nodeB nodeA;
  };

  testScript = ''
    start_all()

    node_a.wait_for_unit("multi-user.target")
    node_b.wait_for_unit("multi-user.target")
    node_a.wait_for_unit("p2p-vpn-node-a.service")
    node_b.wait_for_unit("p2p-vpn-node-b.service")
    node_a.wait_for_file("/run/p2p-vpn-node-a/control.sock")
    node_b.wait_for_file("/run/p2p-vpn-node-b/control.sock")

    with subtest("test configs force libp2p QUIC stream fallback"):
        node_a.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-quic-stream\" "
            "and .network.listen_addresses == [\"/ip4/0.0.0.0/udp/4001/quic-v1\"] "
            "and .network.packet_plane.listen == [] "
            "and .network.packet_plane.quic_listen == [] "
            "and .network.discovery.mdns == false "
            "and .network.discovery.kademlia == false "
            "and .peers[0].addresses == [\"${quicAddress nodeB}\"]"
            "' /run/p2p-vpn-node-a/config.json"
        )
        node_b.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-quic-stream\" "
            "and .network.listen_addresses == [\"/ip4/0.0.0.0/udp/4001/quic-v1\"] "
            "and .network.packet_plane.listen == [] "
            "and .network.packet_plane.quic_listen == [] "
            "and .network.discovery.mdns == false "
            "and .network.discovery.kademlia == false "
            "and .peers[0].addresses == [\"${quicAddress nodeA}\"]"
            "' /run/p2p-vpn-node-b/config.json"
        )

    with subtest("pv0 traffic uses direct QUIC stream fallback"):
        node_a.succeed("${health "node-a"}")
        node_b.succeed("${health "node-b"}")
        node_a.succeed("ip -brief addr show pv0")
        node_b.succeed("ip -brief addr show pv0")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)
        node_a.wait_until_succeeds("${paths "node-a"} | tee /tmp/node-a-paths | grep -q 'selected path: .* direct_quic_stream '", timeout=60)
        node_b.wait_until_succeeds("${paths "node-b"} | tee /tmp/node-b-paths | grep -q 'selected path: .* direct_quic_stream '", timeout=60)
        node_a.succeed("grep -q 'connection_id [0-9]' /tmp/node-a-paths")
        node_b.succeed("grep -q 'connection_id [0-9]' /tmp/node-b-paths")
        node_a.wait_until_succeeds("${state "node-a"} | tee /tmp/node-a-state | grep -q '^outbound_direct_quic_stream_fallback_packets [1-9]'", timeout=60)
        node_b.wait_until_succeeds("${state "node-b"} | tee /tmp/node-b-state | grep -q '^outbound_direct_quic_stream_fallback_packets [1-9]'", timeout=60)
        node_a.succeed("grep -q '^packet_plane_sessions 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^packet_plane_sessions 0$' /tmp/node-b-state")
        node_a.succeed("grep -q '^healthy_direct_quic_stream_paths 1$' /tmp/node-a-state")
        node_b.succeed("grep -q '^healthy_direct_quic_stream_paths 1$' /tmp/node-b-state")
        node_a.succeed("grep -q '^outbound_direct_tcp_stream_fallback_packets 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^outbound_direct_tcp_stream_fallback_packets 0$' /tmp/node-b-state")
        node_a.succeed("grep -q '^outbound_relay_stream_fallback_packets 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^outbound_relay_stream_fallback_packets 0$' /tmp/node-b-state")
  '';
}
