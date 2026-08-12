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
    optionalString
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
  stateBackedInstances = filterAttrs (
    _: instance:
    instance.enable
    && instance.configFile == null
    && instance.settings == null
    && instance.privateKey == null
    && instance.privateKeyFile == null
    && instance.stateConfigFile != null
  ) cfg.instances;
  storeGeneratedInstances = filterAttrs (
    _: instance:
    instance.enable
    && instance.configFile == null
    && instance.settings == null
    && instance.privateKey != null
    && instance.privateKeyFile == null
    && instance.membershipKeyFile == null
  ) cfg.instances;
  stateDirectories = unique (
    mapAttrsToList (_: instance: instance.stateDirectory) stateBackedInstances
  );

  generatedConfigFile = name: "/etc/p2p-vpn/${name}.json";
  runtimeGeneratedConfigFile = name: "/run/p2p-vpn-${name}/config.json";
  effectiveConfigFile =
    name: instance:
    if instance.configFile != null then
      instance.configFile
    else if
      instance.settings == null
      && instance.privateKey == null
      && instance.privateKeyFile == null
      && instance.stateConfigFile != null
    then
      instance.stateConfigFile
    else if
      instance.settings != null || (instance.privateKeyFile == null && instance.membershipKeyFile == null)
    then
      generatedConfigFile name
    else
      runtimeGeneratedConfigFile name;

  routeType = types.oneOf [
    types.str
    (types.submodule (
      { ... }:
      {
        options = {
          prefix = mkOption {
            type = types.str;
            example = "10.44.0.1/32";
            description = "Overlay route prefix.";
          };

          metric = mkOption {
            type = types.ints.unsigned;
            default = 0;
            example = 100;
            description = "Overlay route metric.";
          };
        };
      }
    ))
  ];
  routeObject = route: if builtins.isString route then { prefix = route; } else route;
  bootstrapPeerObject = peer: {
    inherit (peer) id address;
  };
  discoverySettings = discovery: {
    inherit (discovery)
      mdns
      kademlia
      dcutr
      autonat
      ;
    kademlia_provider_advertisement = discovery.kademliaProviderAdvertisement;
    kademlia_protocol = discovery.kademliaProtocol;
  };
  compactPeerConfig =
    id: peer:
    {
      inherit id;
    }
    // optionalAttrs (peer.name != null) { inherit (peer) name; }
    // optionalAttrs (peer.ip != null) { inherit (peer) ip; }
    // optionalAttrs (peer.vpnIp != null) { vpn_ip = peer.vpnIp; }
    // optionalAttrs (peer.addresses != [ ]) { inherit (peer) addresses; }
    // optionalAttrs (peer.routes != [ ]) { routes = map routeObject peer.routes; };

  generatedSettings =
    instance:
    {
      network = (
        {
          name = instance.networkName;
        }
        // optionalAttrs (instance.localPeer != null) { local_peer = instance.localPeer; }
        // optionalAttrs (instance.privateKey != null) { private_key = instance.privateKey; }
        // optionalAttrs (instance.vpnIp != null) { vpn_ip = instance.vpnIp; }
        // optionalAttrs (instance.routes != [ ]) { routes = map routeObject instance.routes; }
        // optionalAttrs (instance.membershipKey != null) {
          membership_key = instance.membershipKey;
        }
        // optionalAttrs (instance.memberRecords != [ ]) { member_records = instance.memberRecords; }
        // optionalAttrs (instance.bootstrapPeers != [ ]) {
          bootstrap_peers = map bootstrapPeerObject instance.bootstrapPeers;
        }
        // optionalAttrs (instance.discovery != null) {
          discovery = discoverySettings instance.discovery;
        }
        //
          optionalAttrs
            (instance.relayServer || instance.relayReservations != [ ] || instance.autoRelay != null)
            {
              relay =
                optionalAttrs instance.relayServer { server = true; }
                // optionalAttrs (instance.relayReservations != [ ]) {
                  reservations = instance.relayReservations;
                }
                // optionalAttrs (instance.autoRelay != null) {
                  auto = {
                    max_candidates = instance.autoRelay.maxCandidates;
                    max_reservations = instance.autoRelay.maxReservations;
                    retry_interval_seconds = instance.autoRelay.retryIntervalSeconds;
                  };
                };
            }
      );
      peers = mapAttrsToList compactPeerConfig instance.peers;
    }
    // optionalAttrs (instance.interfaceName != "pv0" || instance.mtu != 1280) {
      interface =
        optionalAttrs (instance.interfaceName != "pv0") { name = instance.interfaceName; }
        // optionalAttrs (instance.mtu != 1280) { inherit (instance) mtu; };
    };

  runtimeTemplateFile =
    name: instance:
    pkgs.writeText "p2p-vpn-${name}-runtime-template.json" (
      builtins.toJSON (generatedSettings instance)
    );
  runtimeConfigScript =
    name: instance:
    pkgs.writeShellScript "p2p-vpn-${name}-write-config" (
      ''
        set -eu
        config="$(mktemp)"
        cp ${runtimeTemplateFile name instance} "$config"
      ''
      + optionalString (instance.privateKeyFile != null) ''
        next="$(mktemp)"
        ${pkgs.jq}/bin/jq \
          --rawfile private_key ${lib.escapeShellArg instance.privateKeyFile} \
          '.network.private_key = ($private_key | rtrimstr("\n"))' \
          "$config" > "$next"
        mv "$next" "$config"
      ''
      + optionalString (instance.membershipKeyFile != null) ''
        next="$(mktemp)"
        ${pkgs.jq}/bin/jq \
          --rawfile membership_key ${lib.escapeShellArg instance.membershipKeyFile} \
          '.network.membership_key = ($membership_key | rtrimstr("\n"))' \
          "$config" > "$next"
        mv "$next" "$config"
      ''
      + ''
        install -m 0600 "$config" ${lib.escapeShellArg (runtimeGeneratedConfigFile name)}
      ''
    );

  instanceOptions =
    { name, config, ... }:
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

        stateDirectory = mkOption {
          type = types.str;
          default = "/var/lib/p2p-vpn";
          example = "/var/lib/p2p-vpn";
          description = ''
            Persistent directory for paired runtime configs.

            The module creates this directory when an enabled instance uses
            stateConfigFile.
          '';
        };

        stateConfigFile = mkOption {
          type = types.nullOr types.str;
          default = "${config.stateDirectory}/${name}.json";
          example = "/var/lib/p2p-vpn/${name}.json";
          description = ''
            Persistent paired JSON config path used when no config source is
            declared.

            This lets consumers enable an instance before pairing. The systemd
            unit waits until the file exists.

            Set this to null to require configFile, settings, privateKey, or
            privateKeyFile.
          '';
        };

        settings = mkOption {
          type = types.nullOr (types.attrsOf types.anything);
          default = null;
          example = {
            network = {
              name = "lab";
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

            Omit this when privateKey or privateKeyFile is set. The daemon
            derives the local peer ID from the private key at runtime.

            Set this only to assert that a private key belongs to an expected
            peer ID.
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

        membershipKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "BASE64_MEMBERSHIP_KEY";
          description = ''
            Shared overlay membership key for generated configs.

            This is copied into the Nix store. Prefer membershipKeyFile for
            real deployments.
          '';
        };

        membershipKeyFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/run/secrets/p2p-vpn/lab.membership-key";
          description = ''
            Runtime file containing the base64 shared membership key.

            The module reads this file at service start.
          '';
        };

        memberRecords = mkOption {
          type = types.listOf (types.attrsOf types.anything);
          default = [ ];
          example = [ ];
          description = ''
            Signed membership records for generated configs.

            Pairing can emit these records into Nix because they are signed
            authorization records, not private keys.
          '';
        };

        bootstrapPeers = mkOption {
          type = types.listOf (
            types.submodule (
              { ... }:
              {
                options = {
                  id = mkOption {
                    type = types.str;
                    example = "12D3KooWBootstrap";
                    description = "Bootstrap peer ID.";
                  };

                  address = mkOption {
                    type = types.str;
                    example = "/ip4/203.0.113.10/tcp/4001";
                    description = "Bootstrap peer multiaddr without the trailing peer ID.";
                  };
                };
              }
            )
          );
          default = [ ];
          example = [
            {
              id = "12D3KooWBootstrap";
              address = "/ip4/203.0.113.10/tcp/4001";
            }
          ];
          description = ''
            Bootstrap peers for generated configs.
          '';
        };

        interfaceName = mkOption {
          type = types.str;
          default = "pv0";
          example = "pv0";
          description = "TUN interface name for generated configs.";
        };

        mtu = mkOption {
          type = types.ints.positive;
          default = 1280;
          example = 1280;
          description = "TUN interface MTU for generated configs.";
        };

        vpnIp = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "10.44.0.1";
          description = ''
            Stable overlay host IP originated by this node.

            The daemon compiles IPv4 values as /32 routes and IPv6 values as
            /128 routes. CIDR strings are accepted when an explicit prefix is
            needed.
          '';
        };

        routes = mkOption {
          type = types.listOf routeType;
          default = [ ];
          example = [
            "10.44.0.1/32"
            {
              prefix = "10.44.0.0/24";
              metric = 100;
            }
          ];
          description = ''
            Overlay prefixes originated by this node in generated minimal
            configs.
          '';
        };

        relayServer = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = ''
            Enable the libp2p circuit relay server for this instance.

            Normal VPN nodes should leave this disabled.
          '';
        };

        relayReservations = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "/ip4/203.0.113.10/tcp/4001/p2p/RELAY_PEER_ID/p2p-circuit" ];
          description = ''
            Relay reservation multiaddrs for generated configs.

            Use this for forced relay tests or explicit relay fallback.
          '';
        };

        autoRelay = mkOption {
          type = types.nullOr (
            types.submodule (
              { ... }:
              {
                options = {
                  maxCandidates = mkOption {
                    type = types.ints.unsigned;
                    default = 16;
                    example = 32;
                    description = "Maximum discovered relay candidates to retain.";
                  };

                  maxReservations = mkOption {
                    type = types.ints.unsigned;
                    default = 2;
                    example = 4;
                    description = "Maximum automatic relay reservations to keep active.";
                  };

                  retryIntervalSeconds = mkOption {
                    type = types.ints.positive;
                    default = 30;
                    example = 60;
                    description = "Delay before retrying a failed automatic relay reservation.";
                  };
                };
              }
            )
          );
          default = null;
          example = {
            maxCandidates = 16;
            maxReservations = 2;
            retryIntervalSeconds = 30;
          };
          description = ''
            Optional automatic relay reservation policy for generated configs.

            Leave unset to use compact runtime defaults. Set this when public
            relay discovery needs tighter or broader reservation behavior.
          '';
        };

        discovery = mkOption {
          type = types.nullOr (
            types.submodule (
              { ... }:
              {
                options = {
                  mdns = mkOption {
                    type = types.bool;
                    default = true;
                    description = "Enable LAN mDNS peer discovery.";
                  };

                  kademlia = mkOption {
                    type = types.bool;
                    default = true;
                    description = "Enable libp2p Kademlia peer discovery.";
                  };

                  kademliaProviderAdvertisement = mkOption {
                    type = types.bool;
                    default = true;
                    description = "Advertise this node as a Kademlia provider for the overlay.";
                  };

                  kademliaProtocol = mkOption {
                    type = types.str;
                    default = "/ipfs/kad/1.0.0";
                    example = "/p2p-vpn/lab/kad/1.0.0";
                    description = "Kademlia protocol name used for generated configs.";
                  };

                  dcutr = mkOption {
                    type = types.bool;
                    default = true;
                    description = "Enable libp2p direct connection upgrade through relay.";
                  };

                  autonat = mkOption {
                    type = types.bool;
                    default = true;
                    description = "Enable libp2p AutoNAT probing.";
                  };
                };
              }
            )
          );
          default = null;
          example = {
            mdns = false;
            kademlia = false;
            kademliaProviderAdvertisement = false;
            dcutr = false;
            autonat = false;
          };
          description = ''
            Optional discovery override for generated minimal configs.

            Leave this unset for normal operation. Set individual fields when a
            test or deployment needs deterministic discovery behavior.
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

                  vpnIp = mkOption {
                    type = types.nullOr types.str;
                    default = null;
                    example = "10.44.0.2";
                    description = ''
                      Stable overlay host IP this peer may originate.

                      The daemon compiles IPv4 values as /32 routes and IPv6
                      values as /128 routes. CIDR strings are accepted when an
                      explicit prefix is needed.
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
                    type = types.listOf routeType;
                    default = [ ];
                    example = [
                      "10.44.0.2/32"
                      {
                        prefix = "10.44.0.0/24";
                        metric = 100;
                      }
                    ];
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
              vpnIp = "10.44.0.2";
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
      unitConfig =
        optionalAttrs
          (
            instance.configFile == null
            && instance.settings == null
            && instance.privateKey == null
            && instance.privateKeyFile == null
            && instance.stateConfigFile != null
          )
          {
            ConditionPathExists = effectiveConfigFile name instance;
          };

      serviceConfig = {
        Type = "simple";
        ExecStartPre = optional (instance.privateKeyFile != null || instance.membershipKeyFile != null) (
          runtimeConfigScript name instance
        );
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
        # The daemon creates its TUN device and then applies per-interface
        # net.ipv4.conf.<ifname> sysctls needed for overlay host routing.
        ProtectKernelTunables = false;
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
            || instance.privateKey != null
            || instance.privateKeyFile != null
            || instance.stateConfigFile != null;
          message = "services.p2p-vpn.instances.${name} requires configFile, settings, privateKey, privateKeyFile, or stateConfigFile.";
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
          assertion = instance.membershipKey == null || instance.membershipKeyFile == null;
          message = "services.p2p-vpn.instances.${name} cannot set both membershipKey and membershipKeyFile.";
        }
        {
          assertion =
            (instance.configFile == null && instance.settings == null)
            && (
              instance.privateKey != null
              || instance.privateKeyFile != null
              || (
                instance.localPeer == null
                && instance.membershipKey == null
                && instance.membershipKeyFile == null
                && instance.memberRecords == [ ]
                && instance.bootstrapPeers == [ ]
                && instance.vpnIp == null
                && instance.routes == [ ]
                && instance.interfaceName == "pv0"
                && instance.mtu == 1280
                && !instance.relayServer
                && instance.relayReservations == [ ]
                && instance.autoRelay == null
                && instance.discovery == null
                && instance.peers == { }
              )
            )
            || (
              instance.localPeer == null
              && instance.privateKey == null
              && instance.privateKeyFile == null
              && instance.membershipKey == null
              && instance.membershipKeyFile == null
              && instance.memberRecords == [ ]
              && instance.bootstrapPeers == [ ]
              && instance.vpnIp == null
              && instance.routes == [ ]
              && instance.interfaceName == "pv0"
              && instance.mtu == 1280
              && !instance.relayServer
              && instance.relayReservations == [ ]
              && instance.autoRelay == null
              && instance.discovery == null
              && instance.peers == { }
            );
          message = "services.p2p-vpn.instances.${name} generated config fields require privateKey/privateKeyFile and cannot be combined with configFile or settings.";
        }
      ]
    ) (builtins.attrNames enabledInstances);

    environment.systemPackages = [ cfg.package ];

    environment.etc =
      concatMapAttrs (name: instance: {
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

    systemd.tmpfiles.rules = map (directory: "d ${directory} 0700 root root -") stateDirectories;

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
