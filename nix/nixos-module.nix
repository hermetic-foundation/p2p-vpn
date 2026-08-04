{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.p2p-vpn;
  inherit (lib)
    attrValues
    concatMap
    filterAttrs
    mapAttrs'
    mkEnableOption
    mkIf
    mkOption
    nameValuePair
    optionals
    types
    unique
    ;

  packageForSystem = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  enabledInstances = filterAttrs (_: instance: instance.enable) cfg.instances;
  firewallInstances = attrValues (filterAttrs (_: instance: instance.enable && instance.openFirewall) cfg.instances);

  instanceOptions =
    { name, ... }:
    {
      options = {
        enable = mkEnableOption "the ${name} p2p-vpn instance";

        configFile = mkOption {
          type = types.str;
          example = "/etc/p2p-vpn/${name}.json";
          description = ''
            Runtime path to the p2p-vpn JSON config. Use a path outside the
            Nix store when the config contains private keys or membership
            material.
          '';
        };

        metricsIntervalSeconds = mkOption {
          type = types.nullOr types.ints.positive;
          default = null;
          example = 10;
          description = ''
            When set, pass --metrics-interval-seconds to print periodic
            forwarding metrics into the systemd journal.
          '';
        };

        controlSocket = mkOption {
          type = types.nullOr types.str;
          default = "/run/p2p-vpn-${name}/control.sock";
          example = "/run/p2p-vpn/${name}.sock";
          description = ''
            Unix socket path for local daemon status, state, and orderly
            shutdown requests. Set to null to disable the local control socket.
          '';
        };

        extraArgs = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "--dry-run" ];
          description = "Additional arguments appended to p2p-vpn up.";
        };

        openFirewall = mkOption {
          type = types.bool;
          default = false;
          description = "Open the configured TCP and UDP listen ports in the NixOS firewall.";
        };

        tcpPorts = mkOption {
          type = types.listOf types.port;
          default = [ ];
          example = [ 4001 ];
          description = "TCP ports to open when openFirewall is true.";
        };

        udpPorts = mkOption {
          type = types.listOf types.port;
          default = [ ];
          example = [ 4001 ];
          description = "UDP ports to open when openFirewall is true.";
        };
      };
    };

  serviceForInstance =
    name: instance:
    nameValuePair "p2p-vpn-${name}" {
      description = "p2p-vpn mesh VPN instance ${name}";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      path = [ pkgs.iproute2 ];

      serviceConfig = {
        Type = "simple";
        ExecStart = lib.escapeShellArgs (
          [
            "${cfg.package}/bin/p2p-vpn"
            "up"
            "--config"
            instance.configFile
          ]
          ++ optionals (instance.metricsIntervalSeconds != null) [
            "--metrics-interval-seconds"
            (toString instance.metricsIntervalSeconds)
          ]
          ++ optionals (instance.controlSocket != null) [
            "--control-socket"
            instance.controlSocket
          ]
          ++ instance.extraArgs
        );
        Restart = "on-failure";
        RestartSec = "5s";
        KillSignal = "SIGTERM";
        TimeoutStopSec = "30s";
        AmbientCapabilities = [
          "CAP_NET_ADMIN"
          "CAP_NET_RAW"
        ];
        CapabilityBoundingSet = [
          "CAP_NET_ADMIN"
          "CAP_NET_RAW"
        ];
        DeviceAllow = [ "/dev/net/tun rw" ];
        RuntimeDirectory = "p2p-vpn-${name}";
        RuntimeDirectoryMode = "0750";
        NoNewPrivileges = true;
      };
    };
in
{
  options.services.p2p-vpn = {
    package = mkOption {
      type = types.package;
      default = packageForSystem;
      defaultText = "self.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "p2p-vpn package to run.";
    };

    instances = mkOption {
      type = types.attrsOf (types.submodule instanceOptions);
      default = { };
      example = {
        node-a = {
          enable = true;
          configFile = "/etc/p2p-vpn/node-a.json";
          metricsIntervalSeconds = 10;
          openFirewall = true;
          tcpPorts = [ 4001 ];
          udpPorts = [ 4001 ];
        };
      };
      description = "Named p2p-vpn daemon instances.";
    };
  };

  config = mkIf (enabledInstances != { }) {
    assertions = [
      {
        assertion = pkgs.stdenv.isLinux;
        message = "services.p2p-vpn is only supported on Linux because it requires TUN devices and iproute2.";
      }
    ];

    systemd.services = mapAttrs' serviceForInstance enabledInstances;

    networking.firewall.allowedTCPPorts = unique (
      concatMap (instance: instance.tcpPorts) firewallInstances
    );
    networking.firewall.allowedUDPPorts = unique (
      concatMap (instance: instance.udpPorts) firewallInstances
    );

    boot.kernelModules = [ "tun" ];
  };
}
