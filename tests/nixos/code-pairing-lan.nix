{
  self,
  pkgs,
  package,
}:
let
  system = pkgs.stdenv.hostPlatform.system;
  networkName = "nixos-vm-code-pairing-lan";
  instance = "live";
  membershipKey = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
  nodeA = {
    peerId = "12D3KooWLgQHZofqKG3dcgJhSjMX5szux1v2xgbDZaReqw7qnEKr";
    privateKey = "CAESQNzMbqxrZLUOXHgvRi+GcFXQE0HkxXeivjBL7s+lpls3oWZN2qwxhfFaTxEj+lTry+D3vQbk8up80HLC7VI4f2E=";
    vpnIp = "10.48.0.1";
    underlayIp = "192.168.62.1";
  };
  nodeB = {
    peerId = "12D3KooWNCwpbady3Gq6zuVJDPH3RRByh39UrKLtDfpibeMB9rAA";
    privateKey = "CAESQIu/iBs6BjqSmRmeCDb8xvs+fx4FnGVq+lZFcB69Y2nWuBUScvSDZruGp7jMKwXQenwFzzlEmqMY5OZgBCSK7Zc=";
    vpnIp = "10.48.0.2";
    underlayIp = "192.168.62.2";
  };

  evaluatePairedNix = pkgs.writeShellApplication {
    name = "evaluate-p2p-vpn-code-pairing";
    runtimeInputs = [ pkgs.nix ];
    text = ''
      if [ "$#" -ne 4 ]; then
        echo "usage: evaluate-p2p-vpn-code-pairing MODULE INSTANCE VPN_IP OUTPUT" >&2
        exit 2
      fi

      paired_module="$1"
      instance_name="$2"
      vpn_ip="$3"
      output="$4"
      # The expression is single-quoted; values enter through explicit arguments.
      # shellcheck disable=SC2016
      nix-instantiate \
        --eval \
        --strict \
        --json \
        --argstr pairedModule "$paired_module" \
        --argstr instance "$instance_name" \
        --argstr vpnIp "$vpn_ip" \
        --expr '
          ({ pairedModule, instance, vpnIp }:
          let
            pkgs = import ${pkgs.path} { system = "${system}"; };
            lib = pkgs.lib;
            upstreamModule = import ${self.outPath}/nix/nixos-module.nix {
              self = {
                packages.${system}.default = ${package};
              };
            };
            nativeModule = {
              services.p2p-vpn.instances = builtins.listToAttrs [
                {
                  name = instance;
                  value = {
                    enable = true;
                    privateKeyFile = "/run/agenix/p2p-vpn-" + instance + "-identity";
                    membershipKeyFile = "/run/agenix/p2p-vpn-" + instance + "-membership";
                    inherit vpnIp;
                    mtu = 1360;
                    listenAddresses = [ "/ip4/0.0.0.0/tcp/4555" ];
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
                    metricsIntervalSeconds = 30;
                    controlSocket = "/run/p2p-vpn-" + instance + "/control.sock";
                  };
                }
              ];
            };
            evaluated = import ${pkgs.path}/nixos/lib/eval-config.nix {
              system = "${system}";
              modules = [
                upstreamModule
                nativeModule
                (builtins.toPath pairedModule)
                { system.stateVersion = "25.11"; }
              ];
            };
            failedAssertions = builtins.map (item: item.message) (
              builtins.filter (
                item:
                !item.assertion
                && lib.hasPrefix "services.p2p-vpn" item.message
              ) evaluated.config.assertions
            );
            service = builtins.getAttr "p2p-vpn-''${instance}"
              evaluated.config.systemd.services;
          in
          {
            inherit failedAssertions;
            generatedConfig = builtins.getAttr instance
              evaluated.config.services.p2p-vpn.generatedConfigs;
            identityFile = builtins.getAttr instance
              evaluated.config.services.p2p-vpn.identityFiles;
            pairingStateFile = builtins.getAttr instance
              evaluated.config.services.p2p-vpn.pairingStateFiles;
            effectiveInterface = builtins.getAttr instance
              evaluated.config.services.p2p-vpn.effectiveInterfaces;
            service = service.serviceConfig;
          })
        ' > "$output"
    '';
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
        pkgs.coreutils
        pkgs.iproute2
        pkgs.iputils
        pkgs.jq
      ];
    };

  vpnNode =
    local: inviter:
    { ... }:
    {
      imports = [ common ];
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
        metricsIntervalSeconds = 30;
        controlSocket = "/run/p2p-vpn-${instance}/control.sock";
      };
    };

  socket = "/run/p2p-vpn-${instance}/control.sock";
  pair = command: "p2p-vpn pair ${command} --instance ${instance}";
  health =
    controlSocket:
    "p2p-vpn daemon-health --socket ${controlSocket} --timeout-seconds 5 --wait-seconds 90 --require-validated-peers --require-supported-paths";
  state = controlSocket: "p2p-vpn daemon-state --socket ${controlSocket}";
  runtimePath = pkgs.lib.makeBinPath [ pkgs.iproute2 ];
in
pkgs.testers.nixosTest {
  name = "p2p-vpn-nixos-vm-code-pairing-lan";

  nodes = {
    node-a = vpnNode nodeA true;
    node-b = vpnNode nodeB false;
  };

  testScript = ''
    start_all()

    node_a.wait_for_unit("p2p-vpn-${instance}.service")
    node_b.wait_for_unit("p2p-vpn-${instance}.service")
    node_a.wait_for_file("${socket}")
    node_b.wait_for_file("${socket}")

    with subtest("NixOS services use agenix-style identities and no peers"):
        node_a.succeed("test $(stat -c %a /run/agenix/p2p-vpn-${instance}-identity) = 400")
        node_b.succeed("test $(stat -c %a /run/agenix/p2p-vpn-${instance}-identity) = 400")
        node_a.succeed("jq -e '.network.membership_key == \"${membershipKey}\" and .peers == []' /run/p2p-vpn-${instance}/config.json")
        node_b.succeed("jq -e '(.network.membership_key | not) and .peers == []' /run/p2p-vpn-${instance}/config.json")
        node_a.succeed("p2p-vpn status --config /run/p2p-vpn-${instance}/config.json > /tmp/node-a-config-status")
        node_b.succeed("p2p-vpn status --config /run/p2p-vpn-${instance}/config.json > /tmp/node-b-config-status")
        node_a.succeed("grep -q '^local peer: ${nodeA.peerId}$' /tmp/node-a-config-status")
        node_b.succeed("grep -q '^local peer: ${nodeB.peerId}$' /tmp/node-b-config-status")
    with subtest("code pairing discovers the inviter over LAN and waits for approval"):
        node_a.succeed("${pair "open"} --expires-in-seconds 900 --format json > /tmp/open.json")
        code = node_a.succeed("jq -r .code /tmp/open.json").strip()
        open_operation = node_a.succeed("jq -r .operation_id /tmp/open.json").strip()
        node_b.succeed(
            "${pair "join"} " + repr(code)
            + " --vpn-ip ${nodeB.vpnIp} --timeout-seconds 900 --no-wait --format json > /tmp/join.json"
        )
        join_operation = node_b.succeed("jq -r .operation_id /tmp/join.json").strip()
        node_a.succeed("test $(stat -c %a /var/lib/p2p-vpn/${instance}/pairing-state.json) = 600")
        node_b.succeed("test $(stat -c %a /var/lib/p2p-vpn/${instance}/pairing-state.json) = 600")
        node_a.wait_until_succeeds(
            "${pair "status"} " + open_operation
            + " --format json | tee /tmp/open-status.json"
            + " | jq -e '.phase == \"awaiting_approval\" and .candidate.peer_id == \"${nodeB.peerId}\"'",
            timeout=90,
        )
        node_b.wait_until_succeeds(
            "${pair "status"} " + join_operation
            + " --format json | tee /tmp/join-status.json"
            + " | jq -e '.discovery == \"lan\" and .diagnostics.lan_candidates >= 1'",
            timeout=30,
        )
        approval = node_a.succeed("jq -r .candidate.approval_id /tmp/open-status.json").strip()
        node_b.succeed("sleep 3")
        node_a.succeed(
            "${pair "approve"} " + open_operation + " " + approval
            + " --vpn-ip ${nodeB.vpnIp} --format json > /tmp/approve.json"
        )
        node_a.succeed("jq -e '.phase == \"completed\" and .diagnostics.selected_transport == \"direct\"' /tmp/approve.json")
        node_b.wait_until_succeeds(
            "${pair "status"} " + join_operation
            + " --format json | tee /tmp/join-completed.json"
            + " | jq -e '.phase == \"completed\" and .artifacts_ready and .diagnostics.selected_transport == \"direct\"'",
            timeout=90,
        )

    with subtest("live enrollment carries traffic without configured peers"):
        node_a.succeed("${state socket} | tee /tmp/node-a-live-state | grep -q '${nodeB.peerId}'")
        node_b.succeed("${state socket} | tee /tmp/node-b-live-state | grep -q '${nodeA.peerId}'")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)

    with subtest("durable enrollment recovers across daemon restarts"):
        node_a.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_b.succeed("systemctl restart p2p-vpn-${instance}.service")
        node_a.wait_for_file("${socket}")
        node_b.wait_for_file("${socket}")
        node_a.wait_until_succeeds("${state socket} | grep -q '${nodeB.peerId}'", timeout=90)
        node_b.wait_until_succeeds("${state socket} | grep -q '${nodeA.peerId}'", timeout=90)
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)

    with subtest("both daemons emit secret-free native Nix artifacts"):
        node_a.succeed(
            "${pair "artifacts"} " + open_operation
            + " --output /tmp/node-a.nix --force | tee /tmp/node-a-artifacts.out"
        )
        node_b.succeed(
            "${pair "artifacts"} " + join_operation
            + " --output /tmp/node-b.nix --force | tee /tmp/node-b-artifacts.out"
        )
        receipt_a = node_a.succeed("awk '/^pairing receipt:/ { print $3 }' /tmp/node-a-artifacts.out").strip()
        receipt_b = node_b.succeed("awk '/^pairing receipt:/ { print $3 }' /tmp/node-b-artifacts.out").strip()
        assert receipt_a == receipt_b
        node_a.succeed("grep -q 'memberRecords = ' /tmp/node-a.nix")
        node_b.succeed("grep -q 'memberRecords = ' /tmp/node-b.nix")
        node_b.succeed("grep -q 'membershipKeyFile = lib.mkDefault \"/var/lib/p2p-vpn/${instance}/membership.key\";' /tmp/node-b.nix")
        node_a.fail("grep -Fq -e 'privateKey' -e '${nodeA.privateKey}' -e '${membershipKey}' /tmp/node-a.nix")
        node_b.fail("grep -Fq -e 'privateKey' -e '${nodeB.privateKey}' -e '${membershipKey}' /tmp/node-b.nix")
        node_a.fail("grep -Fq 'peers.' /tmp/node-a.nix")
        node_b.fail("grep -Fq 'peers.' /tmp/node-b.nix")
        node_b.succeed("test $(stat -c %a /var/lib/p2p-vpn/${instance}/membership.key) = 600")

    with subtest("generated Nix merges with agenix paths and module settings"):
        node_a.succeed("evaluate-p2p-vpn-code-pairing /tmp/node-a.nix ${instance} ${nodeA.vpnIp} /tmp/node-a-eval.json")
        node_b.succeed("evaluate-p2p-vpn-code-pairing /tmp/node-b.nix ${instance} ${nodeB.vpnIp} /tmp/node-b-eval.json")
        for node, peer, vpn_ip in [
            (node_a, "${nodeA.peerId}", "${nodeA.vpnIp}"),
            (node_b, "${nodeB.peerId}", "${nodeB.vpnIp}"),
        ]:
            node.succeed(
                "jq -e '"
                ".failedAssertions == [] "
                "and .identityFile == \"/run/agenix/p2p-vpn-${instance}-identity\" "
                "and .pairingStateFile == \"/var/lib/p2p-vpn/${instance}/pairing-state.json\" "
                "and .effectiveInterface == \"pv0\" "
                "and .generatedConfig.network.name == \"${networkName}\" "
                "and .generatedConfig.network.local_peer == \"" + peer + "\" "
                "and .generatedConfig.network.vpn_ip == \"" + vpn_ip + "\" "
                "and .generatedConfig.interface.mtu == 1360 "
                "and .generatedConfig.network.listen_addresses == [\"/ip4/0.0.0.0/tcp/4555\"] "
                "and .generatedConfig.network.discovery.mdns "
                "and (.generatedConfig.network.member_records | length) == 2 "
                "and .generatedConfig.peers == [] "
                "and (.service.LoadCredential | index(\"private.key:/run/agenix/p2p-vpn-${instance}-identity\")) != null "
                "and (.service.LoadCredential | index(\"membership.key:/run/agenix/p2p-vpn-${instance}-membership\")) != null"
                "' /tmp/" + ("node-a" if node == node_a else "node-b") + "-eval.json"
            )
            node.fail("jq -e '.generatedConfig.network.private_key or .generatedConfig.network.membership_key' /tmp/" + ("node-a" if node == node_a else "node-b") + "-eval.json")

    with subtest("acknowledgement compacts durable enrollment"):
        node_a.succeed(
            "${pair "acknowledge"} " + open_operation + " --receipt " + receipt_a
            + " --format json | jq -e '.transcript_sha256 == \"" + receipt_a + "\"'"
        )
        node_b.succeed(
            "${pair "acknowledge"} " + join_operation + " --receipt " + receipt_b
            + " --format json | jq -e '.transcript_sha256 == \"" + receipt_b + "\"'"
        )
        node_a.fail("${pair "artifacts"} " + open_operation + " --output /tmp/replayed-a.nix")
        node_b.fail("${pair "artifacts"} " + join_operation + " --output /tmp/replayed-b.nix")

    with subtest("evaluated native configurations boot and carry overlay traffic"):
        node_a.succeed(
            "jq --rawfile private_key /run/agenix/p2p-vpn-${instance}-identity "
            "--rawfile membership_key /run/agenix/p2p-vpn-${instance}-membership "
            "'.generatedConfig | .network.private_key = ($private_key | rtrimstr(\"\\n\")) "
            "| .network.membership_key = ($membership_key | rtrimstr(\"\\n\"))' "
            "/tmp/node-a-eval.json > /tmp/node-a-generated.json"
        )
        node_b.succeed(
            "jq --rawfile private_key /run/agenix/p2p-vpn-${instance}-identity "
            "--rawfile membership_key /run/agenix/p2p-vpn-${instance}-membership "
            "'.generatedConfig | .network.private_key = ($private_key | rtrimstr(\"\\n\")) "
            "| .network.membership_key = ($membership_key | rtrimstr(\"\\n\"))' "
            "/tmp/node-b-eval.json > /tmp/node-b-generated.json"
        )
        node_a.succeed("chmod 0600 /tmp/node-a-generated.json && p2p-vpn status --config /tmp/node-a-generated.json >/dev/null")
        node_b.succeed("chmod 0600 /tmp/node-b-generated.json && p2p-vpn status --config /tmp/node-b-generated.json >/dev/null")
        node_a.succeed("systemctl stop p2p-vpn-${instance}.service && mkdir -p /run/p2p-vpn-generated-a")
        node_b.succeed("systemctl stop p2p-vpn-${instance}.service && mkdir -p /run/p2p-vpn-generated-b")
        node_a.succeed(
            "systemd-run --unit=p2p-vpn-generated-a --property=Type=simple --property=Restart=no "
            "--setenv=PATH=${runtimePath} ${package}/bin/p2p-vpn up --config /tmp/node-a-generated.json "
            "--metrics-interval-seconds 30 --control-socket /run/p2p-vpn-generated-a/control.sock"
        )
        node_b.succeed(
            "systemd-run --unit=p2p-vpn-generated-b --property=Type=simple --property=Restart=no "
            "--setenv=PATH=${runtimePath} ${package}/bin/p2p-vpn up --config /tmp/node-b-generated.json "
            "--metrics-interval-seconds 30 --control-socket /run/p2p-vpn-generated-b/control.sock"
        )
        node_a.wait_for_file("/run/p2p-vpn-generated-a/control.sock")
        node_b.wait_for_file("/run/p2p-vpn-generated-b/control.sock")
        node_a.succeed("${health "/run/p2p-vpn-generated-a/control.sock"} | grep -q '^daemon_health_ready true$'")
        node_b.succeed("${health "/run/p2p-vpn-generated-b/control.sock"} | grep -q '^daemon_health_ready true$'")
        node_a.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeB.vpnIp}", timeout=90)
        node_b.wait_until_succeeds("ping -I pv0 -c 5 -W 2 ${nodeA.vpnIp}", timeout=90)
  '';
}
