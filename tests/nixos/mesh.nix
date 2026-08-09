{
  self,
  pkgs,
  package,
}:
let
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.44.0.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.44.0.2";
  };

  nodeModule =
    name: local: remote:
    { ... }:
    {
      imports = [ self.nixosModules.default ];

      system.stateVersion = "25.11";
      virtualisation.vlans = [ 1 ];
      networking.firewall.enable = false;
      environment.systemPackages = [
        package
        pkgs.iproute2
        pkgs.iputils
        pkgs.jq
      ];

      services.p2p-vpn.instances.${name} = {
        enable = true;
        networkName = "nixos-vm-mesh";
        localPeer = local.peerId;
        privateKey = local.privateKey;
        vpnIp = local.vpnIp;
        peers.${remote.peerId}.vpnIp = remote.vpnIp;
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
    + "--require-packet-plane-listener";
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-mesh";

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

    with subtest("generated configs stay minimal"):
        node_a.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-mesh\" "
            "and .network.local_peer == \"${nodeA.peerId}\" "
            "and .network.vpn_ip == \"${nodeA.vpnIp}\" "
            "and (.network | has(\"discovery\") | not) "
            "and (.network | has(\"relay\") | not) "
            "and (.peers == [{\"id\":\"${nodeB.peerId}\",\"vpn_ip\":\"${nodeB.vpnIp}\"}])"
            "' /etc/p2p-vpn/node-a.json"
        )
        node_b.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-mesh\" "
            "and .network.local_peer == \"${nodeB.peerId}\" "
            "and .network.vpn_ip == \"${nodeB.vpnIp}\" "
            "and (.network | has(\"discovery\") | not) "
            "and (.network | has(\"relay\") | not) "
            "and (.peers == [{\"id\":\"${nodeA.peerId}\",\"vpn_ip\":\"${nodeA.vpnIp}\"}])"
            "' /etc/p2p-vpn/node-b.json"
        )

    with subtest("minimal configs discover healthy peers"):
        node_a.succeed("${health "node-a"} | tee /tmp/node-a-health")
        node_b.succeed("${health "node-b"} | tee /tmp/node-b-health")
        node_a.succeed("grep -q '^daemon_health_ready true$' /tmp/node-a-health")
        node_b.succeed("grep -q '^daemon_health_ready true$' /tmp/node-b-health")

    with subtest("default pv0 carries route-owned traffic"):
        node_a.succeed("ip -brief addr show pv0")
        node_b.succeed("ip -brief addr show pv0")
        node_a.succeed("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}")
        node_b.succeed("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}")

    with subtest("packet plane negotiates on direct LAN path"):
        node_a.succeed(
            "p2p-vpn daemon-health "
            "--socket /run/p2p-vpn-node-a/control.sock "
            "--timeout-seconds 5 "
            "--wait-seconds 60 "
            "--require-packet-plane-session"
        )
        node_a.succeed(
            "p2p-vpn daemon-state "
            "--socket /run/p2p-vpn-node-a/control.sock "
            "| tee /tmp/node-a-state"
        )
        node_a.succeed("grep -q '^packet_plane_sessions 1$' /tmp/node-a-state")
        node_a.succeed("grep -q 'selected_path direct_' /tmp/node-a-state")
  '';
}
