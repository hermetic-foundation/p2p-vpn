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
    concatMapAttrs
    filterAttrs
    map
    mapAttrs'
    mapAttrsToList
    mkEnableOption
    mkIf
    mkOption
    nameValuePair
    optional
    optionalAttrs
    optionals
    types
    unique
    ;

  packageForSystem = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  enabledInstances = filterAttrs (_: instance: instance.enable) cfg.instances;
  firewallInstances = attrValues (
    filterAttrs (_: instance: instance.enable && instance.openFirewall) cfg.instances
  );
  settingsInstances = filterAttrs (
    _: instance: instance.enable && instance.settings != null
  ) cfg.instances;
  storeGeneratedInstances = filterAttrs (
    _: instance:
    instance.enable
    && instance.configFile == null
    && instance.settings == null
    && instance.privateKeyFile == null
  ) cfg.instances;

  generatedConfigFile = name: "/etc/p2p-vpn/${name}.json";
  runtimeGeneratedConfigFile = name: "/run/p2p-vpn-${name}/config.json";
  effectiveConfigFile =
    name: instance:
    if instance.configFile != null then
      instance.configFile
    else if instance.settings != null || instance.privateKeyFile == null then
      generatedConfigFile name
    else
      runtimeGeneratedConfigFile name;

  routeObject = prefix: { inherit prefix; };
  compactPeerConfig =
    id: peer:
    {
      inherit id;
    }
    // optionalAttrs (peer.name != null) { inherit (peer) name; }
    // optionalAttrs (peer.ip != null) { inherit (peer) ip; }
    // optionalAttrs (peer.addresses != [ ]) { inherit (peer) addresses; }
    // optionalAttrs (peer.routes != [ ]) { routes = map routeObject peer.routes; };

  generatedSettings =
    instance:
    {
      network = {
        name = instance.networkName;
        local_peer = instance.localPeer;
      }
      // optionalAttrs (instance.privateKey != null) { private_key = instance.privateKey; }
      // optionalAttrs (instance.routes != [ ]) { routes = map routeObject instance.routes; };
      peers = mapAttrsToList compactPeerConfig instance.peers;
    };

  runtimeTemplateFile =
    name: instance:
    pkgs.writeText "p2p-vpn-${name}-runtime-template.json" (
      builtins.toJSON (generatedSettings instance)
    );
  runtimeConfigScript =
    name: instance:
    pkgs.writeShellScript "p2p-vpn-${name}-write-config" ''
      set -eu
      ${pkgs.jq}/bin/jq \
        --rawfile private_key ${lib.escapeShellArg instance.privateKeyFile} \
        '.network.private_key = ($private_key | rtrimstr("\n"))' \
        ${runtimeTemplateFile name instance} \
        > ${lib.escapeShellArg (runtimeGeneratedConfigFile name)}
    '';

  instanceOptions =
    { name, ... }:
    {
      options = {
        enable = mkEnableOption "the ${name} p2p-vpn instance";

        configFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/etc/p2p-vpn/${name}.json";
          description = ''
            Runtime path to an existing p2p-vpn JSON config.

            Use this instead of settings when the config contains private keys,
            membership keys, or other secret material.
          '';
        };

        settings = mkOption {
          type = types.nullOr (types.attrsOf types.anything);
          default = null;
          example = {
            network = {
              name = "lab";
              local_peer = "LOCAL_PEER_ID";
              private_key = "BASE64_PRIVATE_KEY";
            };
            peers = [
              {
                id = "REMOTE_PEER_ID";
                ip = "192.168.0.203";
              }
            ];
          };
          description = ''
            Declarative p2p-vpn JSON config.

            The module writes this to /etc/p2p-vpn/<instance>.json. Values are
            copied into the Nix store, so do not use this for private keys on
            real deployments.
          '';
        };

        networkName = mkOption {
          type = types.str;
          default = "lab";
          example = "lab";
          description = ''
            Overlay network name for generated minimal configs.

            Peers must use the same network name.
          '';
        };

        localPeer = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "12D3KooWLocalPeer";
          description = ''
            Local libp2p peer ID for generated minimal configs.

            Required when neither configFile nor settings is set.
          '';
        };

        privateKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "BASE64_PRIVATE_KEY";
          description = ''
            Base64 local identity private key for generated configs.

            This is copied into the Nix store. Prefer privateKeyFile for real
            deployments.
          '';
        };

        privateKeyFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/run/secrets/p2p-vpn/lab.key";
          description = ''
            Runtime file containing the base64 local identity private key.

            The module reads this file at service start and writes the final
            runtime JSON into the instance runtime directory.
          '';
        };

        routes = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "10.44.0.1/32" ];
          description = ''
            Overlay prefixes originated by this node in generated minimal
            configs.
          '';
        };

        peers = mkOption {
          type = types.attrsOf (
            types.submodule (
              { ... }:
              {
                options = {
                  name = mkOption {
                    type = types.nullOr types.str;
                    default = null;
                    example = "node-b";
                    description = "Optional peer label.";
                  };

                  ip = mkOption {
                    type = types.nullOr types.str;
                    default = null;
                    example = "192.168.0.203";
                    description = ''
                      Optional direct peer IP for the default TCP libp2p port.

                      Omit this for discovery-only operation.
                    '';
                  };

                  addresses = mkOption {
                    type = types.listOf types.str;
                    default = [ ];
                    example = [ "/ip4/192.168.0.203/tcp/4001/p2p/12D3KooWPeer" ];
                    description = ''
                      Optional explicit libp2p multiaddrs for this peer.

                      Use this only for custom ports, DNS, or relayed paths.
                    '';
                  };

                  routes = mkOption {
                    type = types.listOf types.str;
                    default = [ ];
                    example = [ "10.44.0.2/32" ];
                    description = ''
                      Overlay prefixes this peer may originate.
                    '';
                  };
                };
              }
            )
          );
          default = { };
          example = {
            "12D3KooWRemotePeer" = {
              routes = [ "10.44.0.2/32" ];
            };
          };
          description = ''
            Authorized overlay peers for generated minimal configs.

            Attribute names are peer IDs. Values contain optional overrides.
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
          description = ''
            Open the configured libp2p and packet-plane listen ports in the
            NixOS firewall.
          '';
        };

        tcpPorts = mkOption {
          type = types.listOf types.port;
          default = [ 4001 ];
          example = [ 4001 ];
          description = "Libp2p TCP transport ports to open when openFirewall is true.";
        };

        udpPorts = mkOption {
          type = types.listOf types.port;
          default = [ 5353 ];
          example = [ 4001 ];
          description = ''
            Libp2p UDP/QUIC transport ports to open when openFirewall is true.
            The default opens mDNS discovery.
          '';
        };

        packetPlaneUdpPorts = mkOption {
          type = types.listOf types.port;
          default = [ ];
          example = [ 51820 ];
          description = ''
            Owned packet-plane UDP datagram listener ports to open when
            openFirewall is true. These should match
            network.packet_plane.listen entries in the runtime JSON config.
          '';
        };

        packetPlaneQuicPorts = mkOption {
          type = types.listOf types.port;
          default = [ ];
          example = [ 51821 ];
          description = ''
            Owned packet-plane QUIC listener ports to open when openFirewall is
            true. These should match network.packet_plane.quic_listen entries
            in the runtime JSON config.
          '';
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
        ExecStartPre = optional (instance.privateKeyFile != null) (runtimeConfigScript name instance);
        ExecStart = lib.escapeShellArgs (
          [
            "${cfg.package}/bin/p2p-vpn"
            "up"
            "--config"
            (effectiveConfigFile name instance)
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
        ExecStop = optionals (instance.controlSocket != null) [
          (lib.escapeShellArgs [
            "${cfg.package}/bin/p2p-vpn"
            "daemon-shutdown"
            "--socket"
            instance.controlSocket
          ])
        ];
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
        DevicePolicy = "closed";
        DeviceAllow = [ "/dev/net/tun rw" ];
        RuntimeDirectory = "p2p-vpn-${name}";
        RuntimeDirectoryMode = "0750";
        NoNewPrivileges = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        PrivateTmp = true;
        ProtectClock = true;
        ProtectHome = "read-only";
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectSystem = "strict";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_NETLINK"
          "AF_UNIX"
        ];
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
        UMask = "0077";
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
          udpPorts = [ 5353 ];
          packetPlaneUdpPorts = [ 51820 ];
          packetPlaneQuicPorts = [ 51821 ];
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
    ]
    ++ concatMap (
      name:
      let
        instance = enabledInstances.${name};
      in
      [
        {
          assertion =
            instance.configFile != null
            || instance.settings != null
            || (instance.localPeer != null
              && (instance.privateKey != null || instance.privateKeyFile != null));
          message = "services.p2p-vpn.instances.${name} requires configFile, settings, or generated config fields localPeer plus privateKey/privateKeyFile.";
        }
        {
          assertion = instance.configFile == null || instance.settings == null;
          message = "services.p2p-vpn.instances.${name} cannot set both configFile and settings.";
        }
        {
          assertion = instance.privateKey == null || instance.privateKeyFile == null;
          message = "services.p2p-vpn.instances.${name} cannot set both privateKey and privateKeyFile.";
        }
        {
          assertion =
            (instance.configFile == null && instance.settings == null)
            || (instance.localPeer == null
              && instance.privateKey == null
              && instance.privateKeyFile == null
              && instance.routes == [ ]
              && instance.peers == { });
          message = "services.p2p-vpn.instances.${name} generated config fields cannot be combined with configFile or settings.";
        }
      ]
    ) (builtins.attrNames enabledInstances);

    environment.etc = concatMapAttrs (name: instance: {
      "p2p-vpn/${name}.json".source = pkgs.writeText "p2p-vpn-${name}.json" (
        builtins.toJSON instance.settings
      );
    }) settingsInstances
    // concatMapAttrs (name: instance: {
      "p2p-vpn/${name}.json".source = pkgs.writeText "p2p-vpn-${name}.json" (
        builtins.toJSON (generatedSettings instance)
      );
    }) storeGeneratedInstances;

    systemd.services = mapAttrs' serviceForInstance enabledInstances;

    networking.firewall.allowedTCPPorts = unique (
      concatMap (instance: instance.tcpPorts) firewallInstances
    );
    networking.firewall.allowedUDPPorts = unique (
      concatMap (
        instance: instance.udpPorts ++ instance.packetPlaneUdpPorts ++ instance.packetPlaneQuicPorts
      ) firewallInstances
    );

    boot.kernelModules = [ "tun" ];
  };
}
