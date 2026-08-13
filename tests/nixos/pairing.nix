{
  self,
  pkgs,
  package,
}:
let
  system = pkgs.stdenv.hostPlatform.system;
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
        evaluatePairedNix
        pkgs.iproute2
        pkgs.iputils
        pkgs.jq
      ];
    };

  nodeAModule =
    { ... }:
    {
      imports = [ common ];
      systemd.tmpfiles.rules = [
        "f /run/p2p-vpn-test-node-a.key 0600 root root - ${nodeA.privateKey}"
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = nodeA.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.node-a = {
        enable = true;
        networkName = "nixos-vm-pairing";
        privateKeyFile = "/run/p2p-vpn-test-node-a.key";
        vpnIp = nodeA.vpnIp;
        listenAddresses = [ "/ip4/${nodeA.underlayIp}/tcp/4001" ];
        discovery = {
          mdns = true;
          kademlia = false;
          kademliaProviderAdvertisement = false;
          dcutr = false;
          autonat = false;
        };
        autoRelay = {
          maxCandidates = 0;
          maxReservations = 0;
          retryIntervalSeconds = 30;
        };
        metricsIntervalSeconds = 1;
        controlSocket = "/run/p2p-vpn-node-a/control.sock";
      };
    };

  nodeBModule =
    { ... }:
    {
      imports = [ common ];
      systemd.tmpfiles.rules = [
        "d /var/lib/p2p-vpn/nixos-vm-pairing 0700 root root -"
        "f /var/lib/p2p-vpn/nixos-vm-pairing/private.key 0600 root root - ${nodeB.privateKey}"
      ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = nodeB.underlayIp;
          prefixLength = 24;
        }
      ];

      services.p2p-vpn.instances.nixos-vm-pairing = {
        enable = true;
        vpnIp = nodeB.vpnIp;
        listenAddresses = [ "/ip4/${nodeB.underlayIp}/tcp/4001" ];
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

  state = name: "p2p-vpn daemon-state " + "--socket /run/p2p-vpn-${name}/control.sock";

  status = name: "p2p-vpn daemon-status " + "--socket /run/p2p-vpn-${name}/control.sock";

  routes = name: "p2p-vpn daemon-routes " + "--socket /run/p2p-vpn-${name}/control.sock";

  evaluatePairedNix = pkgs.writeShellApplication {
    name = "evaluate-p2p-vpn-pairing";
    runtimeInputs = [ pkgs.nix ];
    text = ''
      if [ "$#" -ne 3 ]; then
        echo "usage: evaluate-p2p-vpn-pairing MODULE INSTANCE OUTPUT" >&2
        exit 2
      fi

      paired_module="$1"
      instance="$2"
      output="$3"
      # The expression is intentionally single-quoted; values enter through --argstr.
      # shellcheck disable=SC2016
      nix-instantiate \
        --eval \
        --strict \
        --json \
        --argstr pairedModule "$paired_module" \
        --argstr instance "$instance" \
        --expr '
          ({ pairedModule, instance }:
          let
            pkgs = import ${pkgs.path} { system = "${system}"; };
            lib = pkgs.lib;
            upstreamModule = import ${self.outPath}/nix/nixos-module.nix {
              self = {
                packages.${system}.default = ${package};
              };
            };
            minimalModule = {
              services.p2p-vpn.instances = builtins.listToAttrs [
                {
                  name = instance;
                  value.enable = true;
                }
              ];
            };
            evaluated = import ${pkgs.path}/nixos/lib/eval-config.nix {
              system = "${system}";
              modules = [
                upstreamModule
                minimalModule
                (builtins.toPath pairedModule)
                {
                  system.stateVersion = "25.11";
                }
              ];
            };
            failedAssertions = builtins.map (item: item.message) (
              builtins.filter (
                item:
                !item.assertion
                && lib.hasPrefix "services.p2p-vpn" item.message
              ) evaluated.config.assertions
            );
          in
          {
            inherit failedAssertions;
            generatedConfig =
              builtins.getAttr instance
                evaluated.config.services.p2p-vpn.generatedConfigs;
            identityFile =
              builtins.getAttr instance
                evaluated.config.services.p2p-vpn.identityFiles;
            service =
              (builtins.getAttr "p2p-vpn-''${instance}"
                evaluated.config.systemd.services).serviceConfig;
          })
        ' > "$output"
    '';
  };

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
    node_b.wait_for_unit("p2p-vpn-nixos-vm-pairing.service")
    node_a.wait_for_file("/run/p2p-vpn-node-a/control.sock")
    node_b.wait_for_file("/run/p2p-vpn-nixos-vm-pairing/control.sock")
    node_b.succeed(
        "sha256sum /var/lib/p2p-vpn/nixos-vm-pairing/private.key "
        "| cut -d' ' -f1 > /tmp/node-b-key.sha"
    )

    with subtest("inviter starts without a preconfigured joiner"):
        node_a.succeed(
            "jq -e '"
            ".network.name == \"nixos-vm-pairing\" "
            "and .network.vpn_ip == \"${nodeA.vpnIp}\" "
            "and .network.listen_addresses == [\"/ip4/${nodeA.underlayIp}/tcp/4001\"] "
            "and .peers == []"
            "' /run/p2p-vpn-node-a/config.json"
        )
        node_a.succeed("${state "node-a"} | tee /tmp/node-a-initial-state")
        node_a.fail("grep -q '${nodeB.peerId}' /tmp/node-a-initial-state")

    with subtest("pair offer creates inspectable one-time URI"):
        node_a.succeed(
            "p2p-vpn pair offer "
            "--nixos-instance node-a "
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

    with subtest("new node accepts into Nix and reuses its module identity"):
        offer_uri = node_a.succeed("cat /tmp/node-a.pair").strip()
        node_b.succeed("printf '%s\n' " + repr(offer_uri) + " > /tmp/node-a.pair")
        node_b.succeed(
            "p2p-vpn pair accept /tmp/node-a.pair "
            "--nixos-output /tmp/node-b.nix "
            "--nixos-instance nixos-vm-pairing "
            "--nixos-only "
            "--interface pv0 "
            "--vpn-ip ${nodeB.vpnIp} "
            "--timeout-seconds 30 "
            "| tee /tmp/node-b-pair-accept"
        )
        node_b.succeed("test -s /tmp/node-b.nix")
        node_b.succeed("grep -q '^wrote /tmp/node-b.nix$' /tmp/node-b-pair-accept")
        node_b.succeed("grep -q '^kept /var/lib/p2p-vpn/nixos-vm-pairing/private.key$' /tmp/node-b-pair-accept")
        node_b.succeed("grep -q '^paired with: ${nodeA.peerId}$' /tmp/node-b-pair-accept")
        node_b.succeed("grep -q 'services.p2p-vpn.instances.\"nixos-vm-pairing\"' /tmp/node-b.nix")
        node_b.fail("grep -q 'privateKeyFile' /tmp/node-b.nix")
        node_b.fail("grep -q 'listenAddresses' /tmp/node-b.nix")
        node_b.fail("grep -q 'packetPlane' /tmp/node-b.nix")
        node_b.fail("grep -q 'interfaceName' /tmp/node-b.nix")
        node_b.fail("grep -q 'configFile' /tmp/node-b.nix")
        node_b.fail("test -e p2p-vpn.json")
        node_b.succeed(
            "test $(sha256sum /var/lib/p2p-vpn/nixos-vm-pairing/private.key | cut -d' ' -f1) "
            "= $(cat /tmp/node-b-key.sha)"
        )

    with subtest("generated Nix evaluates through the upstream module"):
        node_b.succeed(
            "evaluate-p2p-vpn-pairing "
            "/tmp/node-b.nix "
            "nixos-vm-pairing "
            "/tmp/node-b-nix-evaluation.json"
        )
        node_b.succeed(
            "jq -e '"
            ".failedAssertions == [] "
            "and .identityFile == \"/var/lib/p2p-vpn/nixos-vm-pairing/private.key\" "
            "and .generatedConfig.network.name == \"nixos-vm-pairing\" "
            "and .generatedConfig.network.local_peer == \"${nodeB.peerId}\" "
            "and .generatedConfig.network.vpn_ip == \"${nodeB.vpnIp}\" "
            "and .generatedConfig.network.listen_addresses == ["
            "\"/ip4/0.0.0.0/tcp/4001\", "
            "\"/ip4/0.0.0.0/udp/4001/quic-v1\"] "
            "and .generatedConfig.network.packet_plane.listen == [\"0.0.0.0:51820\"] "
            "and (.generatedConfig.network.member_records | length) >= 1 "
            "and .generatedConfig.peers[0].id == \"${nodeA.peerId}\" "
            "and (.service.LoadCredential | length) == 0"
            "' /tmp/node-b-nix-evaluation.json"
        )
        node_b.fail("jq -e '.generatedConfig.network.private_key' /tmp/node-b-nix-evaluation.json")
        node_b.succeed(
            "jq --rawfile private_key /var/lib/p2p-vpn/nixos-vm-pairing/private.key "
            "'.generatedConfig "
            "| .network.private_key = ($private_key | rtrimstr(\"\\n\"))' "
            "/tmp/node-b-nix-evaluation.json "
            "> /tmp/node-b-from-nix.json"
        )
        node_b.succeed("chmod 0600 /tmp/node-b-from-nix.json")
        node_b.succeed("p2p-vpn status --config /tmp/node-b-from-nix.json >/dev/null")
        node_b.succeed("p2p-vpn routes --config /tmp/node-b-from-nix.json | tee /tmp/node-b-nix-routes")
        node_b.succeed("grep -q 'route: ${nodeA.vpnIp}/32 owner peer ${nodeA.peerId}' /tmp/node-b-nix-routes")

    with subtest("replayed URI is rejected with diagnostics"):
        node_b.fail(
            "p2p-vpn pair accept /tmp/node-a.pair "
            "--nixos-output /tmp/node-b-replay.nix "
            "--nixos-instance nixos-vm-pairing "
            "--nixos-only "
            "--interface pv0 "
            "--vpn-ip ${nodeB.vpnIp} "
            "--timeout-seconds 10 "
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
        node_b.succeed("systemctl stop p2p-vpn-nixos-vm-pairing.service")
        node_b.succeed("mkdir -p /run/p2p-vpn-node-b")
        node_b.succeed(
            "systemd-run "
            "--unit=p2p-vpn-node-b "
            "--property=Type=simple "
            "--property=Restart=no "
            "--setenv=PATH=${runtimePath} "
            "${package}/bin/p2p-vpn up "
            "--config /tmp/node-b-from-nix.json "
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

    with subtest("signed membership restores after inviter restart"):
        node_a.succeed("systemctl restart p2p-vpn-node-a.service")
        node_a.wait_for_unit("p2p-vpn-node-a.service")
        node_a.wait_until_succeeds(
            "${state "node-a"} | tee /tmp/node-a-restarted-state | grep -q '${nodeB.peerId}'",
            timeout=90
        )
        node_a.succeed("${health "node-a"} | tee /tmp/node-a-restarted-health")
        node_a.succeed("grep -q '^daemon_health_ready true$' /tmp/node-a-restarted-health")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)
  '';
}
