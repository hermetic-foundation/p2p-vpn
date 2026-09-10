{
  pkgs,
  lib,
  module,
  system,
}:
let
  evaluate =
    instances:
    (lib.nixosSystem {
      inherit system;
      modules = [
        module
        {
          system.stateVersion = "25.11";
          services.p2p-vpn.instances = instances;
        }
      ];
    }).config;
  defaults = evaluate {
    alpha.enable = true;
    beta.enable = true;
  };
  overrides = evaluate {
    custom = {
      enable = true;
      packetPlane.listen = [ ];
      packetPlane.quicListen = [ "0.0.0.0:54000" ];
    };
    disabled = {
      enable = true;
      packetPlane.quicListen = [ ];
    };
    stream = {
      enable = true;
      packetPlane.listen = [ ];
    };
  };
  closed = evaluate {
    alpha = {
      enable = true;
      openFirewall = false;
    };
  };
  collision = evaluate {
    alpha.enable = true;
    beta = {
      enable = true;
      packetPlane.listen = [ "0.0.0.0:52820" ];
    };
  };
  packet = config: name: config.services.p2p-vpn.generatedConfigs.${name}.network.packet_plane;
  contracts = {
    alpha = (packet defaults "alpha").quic_listen == [ "0.0.0.0:52820" ];
    beta = (packet defaults "beta").quic_listen == [ "0.0.0.0:52821" ];
    defaultFirewall = lib.all (port: builtins.elem port defaults.networking.firewall.allowedUDPPorts) [
      51820
      51821
      52820
      52821
    ];
    explicitListener = (packet overrides "custom").quic_listen == [ "0.0.0.0:54000" ];
    explicitDisable = (packet overrides "disabled").quic_listen == [ ];
    streamOnly =
      (packet overrides "stream").quic_listen == [ ] && (packet overrides "stream").listen == [ ];
    overrideFirewall =
      builtins.elem 54000 overrides.networking.firewall.allowedUDPPorts
      && lib.all (port: !(builtins.elem port overrides.networking.firewall.allowedUDPPorts)) [
        52820
        52821
        52822
      ];
    closedFirewall = !(builtins.elem 52820 closed.networking.firewall.allowedUDPPorts);
    collisionRejected = lib.any (
      entry: !entry.assertion && lib.hasInfix "packet-plane" entry.message
    ) collision.assertions;
  };
  verified = lib.mapAttrs (
    name: passed:
    assert lib.assertMsg passed "automatic QUIC module evaluation failed: ${name}";
    passed
  ) contracts;
in
pkgs.writeText "p2p-vpn-quic-defaults-eval.json" (builtins.toJSON verified)
