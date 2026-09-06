{
  pkgs,
  config,
  source,
}:
let
  inherit (pkgs) lib;
  generated = config.services.p2p-vpn.generatedConfigs.lab;
  service = config.systemd.services.p2p-vpn-lab.serviceConfig;
  consumer = builtins.readFile source;
  contracts = {
    network = generated.network.name == "lab";
    listeners =
      generated.network.listen_addresses == [
        "/ip4/0.0.0.0/tcp/4001"
        "/ip4/0.0.0.0/udp/4001/quic-v1"
      ];
    packetListener = generated.network.packet_plane.listen == [ "0.0.0.0:51820" ];
    interface =
      generated.interface == {
        mtu = 1280;
        name = "pv0";
      };
    peerless = generated.peers == [ ];
    noEmbeddedPrivateKey = !(generated.network ? private_key);
    identity = config.services.p2p-vpn.identityFiles.lab == "/var/lib/p2p-vpn/lab/private.key";
    pairingState =
      config.services.p2p-vpn.pairingStateFiles.lab == "/var/lib/p2p-vpn/lab/pairing-state.json";
    membershipState =
      config.services.p2p-vpn.membershipStateFiles.lab == "/var/lib/p2p-vpn/lab/membership-state.json";
    stateDirectory = builtins.head service.StateDirectory == "p2p-vpn/lab";
    command = lib.hasInfix "p2p-vpn up --config /run/p2p-vpn-lab/config.json --control-socket /run/p2p-vpn-lab/control.sock --pairing-state /var/lib/p2p-vpn/lab/pairing-state.json" service.ExecStart;
    membershipArgument = lib.hasInfix "--membership-state /var/lib/p2p-vpn/lab/membership-state.json" service.ExecStart;
    upstreamModule = lib.hasInfix "p2p-vpn.nixosModules.default" consumer;
    minimalInstance = lib.hasInfix "services.p2p-vpn.instances.lab.enable = true;" consumer;
    noConsumerMechanics =
      !lib.any (part: lib.hasInfix part consumer) [
        "configFile"
        "privateKey"
        "generatedConfigs"
        "systemd.services"
      ];
  };
  verified = lib.mapAttrs (
    name: passed:
    assert lib.assertMsg passed "p2p-vpn consumer evaluation failed: ${name}";
    passed
  ) contracts;
in
pkgs.writeText "p2p-vpn-consumer-eval.json" (builtins.toJSON verified)
