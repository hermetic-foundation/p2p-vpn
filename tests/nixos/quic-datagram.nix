{
  self,
  pkgs,
  package,
}:
let
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.48.0.1";
    underlayIp = "192.168.62.1";
    quicPacketEndpoint = "192.168.62.1:51821";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.48.0.2";
    underlayIp = "192.168.62.2";
    quicPacketEndpoint = "192.168.62.2:51821";
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
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = local.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.${name} = {
        enable = true;
        settings = {
          network = {
            name = "nixos-vm-quic-datagram";
            local_peer = local.peerId;
            private_key = local.privateKey;
            vpn_ip = local.vpnIp;
            listen_addresses = [ "/ip4/0.0.0.0/udp/4001/quic-v1" ];
            discovery = {
              mdns = false;
              kademlia = false;
              kademlia_provider_advertisement = false;
              dcutr = false;
              autonat = false;
            };
            packet_plane = {
              listen = [ ];
              external_endpoints = [ ];
              quic_listen = [ local.quicPacketEndpoint ];
              quic_external_endpoints = [ local.quicPacketEndpoint ];
            };
          };
          peers = [
            {
              id = remote.peerId;
              vpn_ip = remote.vpnIp;
              addresses = [ (quicAddress remote) ];
            }
          ];
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
    + "--require-supported-paths "
    + "--require-packet-plane-quic-listener "
    + "--require-packet-plane-quic-session";

  state =
    name:
    "p2p-vpn daemon-state "
    + "--socket /run/p2p-vpn-${name}/control.sock";

  paths =
    name:
    "p2p-vpn daemon-paths "
    + "--socket /run/p2p-vpn-${name}/control.sock";
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-quic-datagram";

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

    with subtest("test configs expose both QUIC transports"):
        node_a.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-quic-datagram\" "
            "and .network.listen_addresses == [\"/ip4/0.0.0.0/udp/4001/quic-v1\"] "
            "and .network.packet_plane.listen == [] "
            "and .network.packet_plane.quic_listen == [\"${nodeA.quicPacketEndpoint}\"] "
            "and .network.packet_plane.quic_external_endpoints == [\"${nodeA.quicPacketEndpoint}\"] "
            "and .peers[0].addresses == [\"${quicAddress nodeB}\"]"
            "' /etc/p2p-vpn/node-a.json"
        )
        node_b.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-quic-datagram\" "
            "and .network.listen_addresses == [\"/ip4/0.0.0.0/udp/4001/quic-v1\"] "
            "and .network.packet_plane.listen == [] "
            "and .network.packet_plane.quic_listen == [\"${nodeB.quicPacketEndpoint}\"] "
            "and .network.packet_plane.quic_external_endpoints == [\"${nodeB.quicPacketEndpoint}\"] "
            "and .peers[0].addresses == [\"${quicAddress nodeA}\"]"
            "' /etc/p2p-vpn/node-b.json"
        )

    with subtest("pv0 traffic prefers QUIC datagram over QUIC stream"):
        node_a.succeed("${health "node-a"}")
        node_b.succeed("${health "node-b"}")
        node_a.succeed("ip -brief addr show pv0")
        node_b.succeed("ip -brief addr show pv0")
        node_a.wait_until_succeeds("${paths "node-a"} | tee /tmp/node-a-paths | grep -q 'selected path: .* direct_quic_datagram '", timeout=60)
        node_b.wait_until_succeeds("${paths "node-b"} | tee /tmp/node-b-paths | grep -q 'selected path: .* direct_quic_datagram '", timeout=60)
        node_a.succeed("${state "node-a"} | tee /tmp/node-a-before")
        node_b.succeed("${state "node-b"} | tee /tmp/node-b-before")
        node_a.succeed("grep -q '^outbound_direct_quic_stream_fallback_packets 0$' /tmp/node-a-before")
        node_b.succeed("grep -q '^outbound_direct_quic_stream_fallback_packets 0$' /tmp/node-b-before")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)
        node_a.wait_until_succeeds("${state "node-a"} | tee /tmp/node-a-state | grep -q '^outbound_quic_datagram_packets [1-9]'", timeout=60)
        node_b.wait_until_succeeds("${state "node-b"} | tee /tmp/node-b-state | grep -q '^outbound_quic_datagram_packets [1-9]'", timeout=60)
        node_a.succeed("grep -q '^packet_plane_sessions 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^packet_plane_sessions 0$' /tmp/node-b-state")
        node_a.succeed("grep -q '^packet_plane_quic_sessions 1$' /tmp/node-a-state")
        node_b.succeed("grep -q '^packet_plane_quic_sessions 1$' /tmp/node-b-state")
        node_a.succeed("grep -q '^healthy_direct_quic_datagram_paths 1$' /tmp/node-a-state")
        node_b.succeed("grep -q '^healthy_direct_quic_datagram_paths 1$' /tmp/node-b-state")
        node_a.succeed("grep -q '^healthy_direct_quic_stream_paths 1$' /tmp/node-a-state")
        node_b.succeed("grep -q '^healthy_direct_quic_stream_paths 1$' /tmp/node-b-state")
        node_a.succeed("grep -q '^outbound_direct_quic_stream_fallback_packets 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^outbound_direct_quic_stream_fallback_packets 0$' /tmp/node-b-state")
        node_a.succeed("grep -q '^outbound_direct_tcp_stream_fallback_packets 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^outbound_direct_tcp_stream_fallback_packets 0$' /tmp/node-b-state")
        node_a.succeed("grep -q '^outbound_relay_stream_fallback_packets 0$' /tmp/node-a-state")
        node_b.succeed("grep -q '^outbound_relay_stream_fallback_packets 0$' /tmp/node-b-state")
  '';
}
