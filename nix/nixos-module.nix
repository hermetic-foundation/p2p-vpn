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
    attrNames
    concatMap
    filterAttrs
    hasPrefix
    map
    mapAttrs
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
  enabledNames = attrNames enabledInstances;
  nativeInstances = filterAttrs (_: instance: nixMode instance) enabledInstances;
  nativeNames = attrNames nativeInstances;
  instanceIndex = name: lib.lists.findFirstIndex (candidate: candidate == name) 0 nativeNames;
  instancePort = name: 4001 + instanceIndex name;
  instancePacketPort = name: 51820 + instanceIndex name;
  runtimeDirectory = name: "/run/p2p-vpn-${name}";
  runtimeConfigFile = name: "${runtimeDirectory name}/config.json";
  instanceStateDirectory = name: instance: "${instance.stateDirectory}/${name}";
  defaultPrivateKeyFile = name: instance: "${instanceStateDirectory name instance}/private.key";
  nixMode = instance: instance.configFile == null;
  automaticIdentity = instance: nixMode instance && instance.privateKeyFile == null;
  effectiveConfigFile =
    name: instance: if nixMode instance then runtimeConfigFile name else instance.configFile;
  effectiveInterfaceName =
    name: instance:
    if instance.interfaceName != null then
      instance.interfaceName
    else
      "pv${toString (instanceIndex name)}";
  effectiveListenAddresses =
    name: instance:
    if instance.listenAddresses != null then
      instance.listenAddresses
    else
      let
        port = toString (instancePort name);
      in
      [
        "/ip4/0.0.0.0/tcp/${port}"
        "/ip4/0.0.0.0/udp/${port}/quic-v1"
      ];
  effectivePacketPlaneListen =
    name: instance:
    if instance.packetPlane.listen != null then
      instance.packetPlane.listen
    else
      [ "0.0.0.0:${toString (instancePacketPort name)}" ];
  nativeListenAddresses = concatMap (
    name: effectiveListenAddresses name nativeInstances.${name}
  ) nativeNames;
  nativePacketPlaneListeners = concatMap (
    name:
    let
      instance = nativeInstances.${name};
    in
    effectivePacketPlaneListen name instance
    ++ optionals (instance.packetPlane.quicListen != null) instance.packetPlane.quicListen
  ) nativeNames;
  overlayHostAddresses =
    instance:
    optional (instance.vpnIp != null) instance.vpnIp
    ++ concatMap (peer: optional (peer.vpnIp != null) peer.vpnIp) (builtins.attrValues instance.peers);

  routeType = types.oneOf [
    types.str
    (types.submodule {
      options = {
        prefix = mkOption {
          type = types.str;
          example = "10.44.0.0/24";
          description = "Overlay route prefix.";
        };
        metric = mkOption {
          type = types.ints.unsigned;
          default = 0;
          example = 100;
          description = "Overlay route metric.";
        };
      };
    })
  ];
  routeObject = route: if builtins.isString route then { prefix = route; } else route;

  membershipRecordType = types.submodule {
    options = {
      payload = mkOption {
        type = types.submodule {
          options = {
            version = mkOption {
              type = types.ints.unsigned;
              default = 1;
              description = "Membership record wire version.";
            };
            networkName = mkOption {
              type = types.str;
              description = "Signed overlay name.";
            };
            memberPeer = mkOption {
              type = types.str;
              description = "Granted or revoked peer ID.";
            };
            memberPublicKey = mkOption {
              type = types.str;
              description = "Base64 protobuf public key for the member.";
            };
            issuerPeer = mkOption {
              type = types.str;
              description = "Signing peer ID.";
            };
            issuerPublicKey = mkOption {
              type = types.str;
              description = "Base64 protobuf public key for the issuer.";
            };
            membershipEpoch = mkOption {
              type = types.ints.unsigned;
              default = 1;
              description = "Membership generation used for conflict resolution.";
            };
            sequence = mkOption {
              type = types.ints.unsigned;
              default = 0;
              description = "Record sequence within the membership epoch.";
            };
            revoked = mkOption {
              type = types.bool;
              default = false;
              description = "Whether this record revokes the member.";
            };
            roles = mkOption {
              type = types.listOf (
                types.enum [
                  "overlay_member"
                  "route_authority"
                ]
              );
              default = [ ];
              description = "Signed membership roles.";
            };
            routeGrants = mkOption {
              type = types.listOf routeType;
              default = [ ];
              description = "Signed route grants.";
            };
            issuedAtUnixSeconds = mkOption {
              type = types.ints.unsigned;
              description = "Record issue time.";
            };
            expiresAtUnixSeconds = mkOption {
              type = types.nullOr types.ints.unsigned;
              default = null;
              description = "Optional record expiry time.";
            };
          };
        };
        description = "Signed membership payload.";
      };
      signature = mkOption {
        type = types.str;
        description = "Base64 issuer signature.";
      };
    };
  };

  membershipRecordObject = record: {
    payload = {
      inherit (record.payload)
        version
        sequence
        revoked
        roles
        ;
      network_name = record.payload.networkName;
      member_peer = record.payload.memberPeer;
      member_public_key = record.payload.memberPublicKey;
      issuer_peer = record.payload.issuerPeer;
      issuer_public_key = record.payload.issuerPublicKey;
      membership_epoch = record.payload.membershipEpoch;
      route_grants = map routeObject record.payload.routeGrants;
      issued_at_unix_seconds = record.payload.issuedAtUnixSeconds;
    }
    // optionalAttrs (record.payload.expiresAtUnixSeconds != null) {
      expires_at_unix_seconds = record.payload.expiresAtUnixSeconds;
    };
    inherit (record) signature;
  };

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
  relayResourceSettings = resources: {
    max_reservations = resources.maxReservations;
    max_reservations_per_peer = resources.maxReservationsPerPeer;
    reservation_duration_secs = resources.reservationDurationSeconds;
    max_circuits = resources.maxCircuits;
    max_circuits_per_peer = resources.maxCircuitsPerPeer;
    max_circuit_duration_secs = resources.maxCircuitDurationSeconds;
    max_circuit_bytes = resources.maxCircuitBytes;
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
  packetPlaneSettings =
    packetPlane:
    optionalAttrs (packetPlane.listen != null) { inherit (packetPlane) listen; }
    // optionalAttrs (packetPlane.externalEndpoints != null) {
      external_endpoints = packetPlane.externalEndpoints;
    }
    // optionalAttrs (packetPlane.quicListen != null) { quic_listen = packetPlane.quicListen; }
    // optionalAttrs (packetPlane.quicExternalEndpoints != null) {
      quic_external_endpoints = packetPlane.quicExternalEndpoints;
    }
    // optionalAttrs (packetPlane.sessionTtlSeconds != null) {
      session_ttl_seconds = packetPlane.sessionTtlSeconds;
    }
    // optionalAttrs (packetPlane.maxReplayWindowsPerSession != null) {
      max_replay_windows_per_session = packetPlane.maxReplayWindowsPerSession;
    };
  packetPlaneIsDefault =
    packetPlane:
    packetPlane.listen == null
    && packetPlane.externalEndpoints == null
    && packetPlane.quicListen == null
    && packetPlane.quicExternalEndpoints == null
    && packetPlane.sessionTtlSeconds == null
    && packetPlane.maxReplayWindowsPerSession == null;
  queueSettings = queue: {
    max_packets_per_peer = queue.maxPacketsPerPeer;
    max_bytes_per_peer = queue.maxBytesPerPeer;
    max_packet_age_millis = queue.maxPacketAgeMillis;
  };
  resourceSettings = resources: {
    max_concurrent_packet_streams = resources.maxConcurrentPacketStreams;
    max_concurrent_control_streams = resources.maxConcurrentControlStreams;
    max_inbound_packets_per_peer_per_second = resources.maxInboundPacketsPerPeerPerSecond;
    max_pairing_requests_per_peer_per_second = resources.maxPairingRequestsPerPeerPerSecond;
    max_pending_incoming_connections = resources.maxPendingIncomingConnections;
    max_pending_outgoing_connections = resources.maxPendingOutgoingConnections;
    max_established_incoming_connections = resources.maxEstablishedIncomingConnections;
    max_established_outgoing_connections = resources.maxEstablishedOutgoingConnections;
    max_established_connections_per_peer = resources.maxEstablishedConnectionsPerPeer;
    max_established_connections = resources.maxEstablishedConnections;
  };

  generatedSettings =
    name: instance:
    {
      network = (
        {
          name = instance.networkName;
          listen_addresses = effectiveListenAddresses name instance;
        }
        // optionalAttrs (instance.localPeer != null) { local_peer = instance.localPeer; }
        // optionalAttrs (instance.vpnIp != null) { vpn_ip = instance.vpnIp; }
        // optionalAttrs (instance.routes != [ ]) { routes = map routeObject instance.routes; }
        // optionalAttrs (instance.externalAddresses != [ ]) {
          external_addresses = instance.externalAddresses;
        }
        // optionalAttrs (instance.previousMembershipTags != [ ]) {
          previous_membership_tags = instance.previousMembershipTags;
        }
        // optionalAttrs (instance.memberRecords != [ ]) {
          member_records = map membershipRecordObject instance.memberRecords;
        }
        // optionalAttrs (instance.bootstrapPeers != [ ]) {
          bootstrap_peers = map bootstrapPeerObject instance.bootstrapPeers;
        }
        // optionalAttrs (instance.discovery != null) {
          discovery = discoverySettings instance.discovery;
        }
        //
          optionalAttrs
            (
              instance.relayServer
              || instance.relayReservations != [ ]
              || instance.autoRelay != null
              || instance.relayResources != null
            )
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
                }
                // optionalAttrs (instance.relayResources != null) {
                  resources = relayResourceSettings instance.relayResources;
                };
            }
        // {
          packet_plane = packetPlaneSettings instance.packetPlane // {
            listen = effectivePacketPlaneListen name instance;
          };
        }
      );
      interface = {
        name = effectiveInterfaceName name instance;
        inherit (instance) mtu;
      };
      peers = mapAttrsToList compactPeerConfig instance.peers;
    }
    // optionalAttrs (instance.queue != null) { queue = queueSettings instance.queue; }
    // optionalAttrs (instance.resources != null) {
      resources = resourceSettings instance.resources;
    };

  runtimeTemplateFile =
    name: instance:
    pkgs.writeText "p2p-vpn-${name}-runtime-template.json" (
      builtins.toJSON (generatedSettings name instance)
    );
  runtimeConfigScript =
    name: instance:
    let
      keyFile = defaultPrivateKeyFile name instance;
      target = runtimeConfigFile name;
      runtime = runtimeDirectory name;
    in
    pkgs.writeShellScript "p2p-vpn-${name}-prepare-config" (
      ''
        set -eu
        umask 077
        config="$(mktemp ${lib.escapeShellArg "${runtime}/config.XXXXXX"})"
        next=""
        cleanup() {
          rm -f "$config"
          if [ -n "$next" ]; then
            rm -f "$next"
          fi
        }
        trap cleanup EXIT HUP INT TERM
        cp ${runtimeTemplateFile name instance} "$config"
      ''
      + (
        if automaticIdentity instance then
          ''
            private_key_file=${lib.escapeShellArg keyFile}
            if [ ! -s "$private_key_file" ]; then
              if [ -e "$private_key_file" ]; then
                echo "p2p-vpn identity exists but is empty: $private_key_file" >&2
                exit 1
              fi
              ${cfg.package}/bin/p2p-vpn keygen --output "$private_key_file"
            fi
          ''
        else
          ''
            : "''${CREDENTIALS_DIRECTORY:?systemd credential directory is unavailable}"
            private_key_file="$CREDENTIALS_DIRECTORY/private.key"
          ''
      )
      + ''
        test -r "$private_key_file" || {
          echo "p2p-vpn identity is not readable: $private_key_file" >&2
          exit 1
        }
        next="$(mktemp ${lib.escapeShellArg "${runtime}/config.XXXXXX"})"
        ${pkgs.jq}/bin/jq \
          --rawfile private_key "$private_key_file" \
          '.network.private_key = ($private_key | rtrimstr("\n"))' \
          "$config" > "$next"
        mv "$next" "$config"
        next=""
      ''
      + optionalString (instance.membershipKeyFile != null) ''
        : "''${CREDENTIALS_DIRECTORY:?systemd credential directory is unavailable}"
        membership_key_file="$CREDENTIALS_DIRECTORY/membership.key"
        test -r "$membership_key_file" || {
          echo "p2p-vpn membership key is not readable: $membership_key_file" >&2
          exit 1
        }
        next="$(mktemp ${lib.escapeShellArg "${runtime}/config.XXXXXX"})"
        ${pkgs.jq}/bin/jq \
          --rawfile membership_key "$membership_key_file" \
          '.network.membership_key = ($membership_key | rtrimstr("\n"))' \
          "$config" > "$next"
        mv "$next" "$config"
        next=""
      ''
      + ''
        ${cfg.package}/bin/p2p-vpn status --config "$config" >/dev/null
        install -m 0600 "$config" ${lib.escapeShellArg target}
      ''
    );
  jsonConfigCheckScript =
    name: instance:
    pkgs.writeShellScript "p2p-vpn-${name}-check-json-config" ''
      set -eu
      test -r ${lib.escapeShellArg instance.configFile} || {
        echo "p2p-vpn JSON config is not readable: ${instance.configFile}" >&2
        exit 1
      }
      ${cfg.package}/bin/p2p-vpn status --config ${lib.escapeShellArg instance.configFile} >/dev/null
    '';

  packetPlaneOptions = {
    listen = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      example = [ "0.0.0.0:51820" ];
      description = ''
        Owned UDP packet-plane listeners.

        Null selects a deterministic per-instance listener beginning at port
        51820. An empty list disables the listener.
      '';
    };
    externalEndpoints = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      example = [ "203.0.113.10:51820" ];
      description = "Advertised UDP packet-plane endpoints.";
    };
    quicListen = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      example = [ "0.0.0.0:51821" ];
      description = "Owned QUIC DATAGRAM packet-plane listener. At most one is supported.";
    };
    quicExternalEndpoints = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      example = [ "203.0.113.10:51821" ];
      description = "Advertised QUIC DATAGRAM packet-plane endpoints.";
    };
    sessionTtlSeconds = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      example = 600;
      description = "Packet-plane session lifetime. Null uses the daemon default.";
    };
    maxReplayWindowsPerSession = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      example = 1024;
      description = "Maximum replay windows retained per packet-plane session.";
    };
  };

  instanceOptions =
    { name, ... }:
    {
      options = {
        enable = mkEnableOption "the ${name} p2p-vpn instance";

        configFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/run/secrets/p2p-vpn/lab.json";
          description = ''
            Complete user-owned JSON configuration file.

            Setting this selects JSON mode. All native Nix configuration
            options must remain at their defaults.
          '';
        };
        stateDirectory = mkOption {
          type = types.str;
          default = "/var/lib/p2p-vpn";
          description = "Parent directory for persistent per-instance state.";
        };
        networkName = mkOption {
          type = types.str;
          default = name;
          description = "Overlay name. Peers in one overlay use the same value.";
        };
        localPeer = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Optional expected local peer ID. The private key remains authoritative.";
        };
        privateKeyFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/run/secrets/p2p-vpn/lab.key";
          description = ''
            Explicit runtime identity key source.

            When null, the module generates and persists an identity under
            stateDirectory on first service start.
          '';
        };
        membershipKeyFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/run/secrets/p2p-vpn/lab.membership-key";
          description = "Optional runtime overlay membership-key source.";
        };
        privateKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          internal = true;
          description = "Removed insecure compatibility option.";
        };
        membershipKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          internal = true;
          description = "Removed insecure compatibility option.";
        };
        previousMembershipTags = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Temporary accepted membership tags during key rotation.";
        };
        memberRecords = mkOption {
          type = types.listOf membershipRecordType;
          default = [ ];
          description = "Signed membership grants and revocations.";
        };
        vpnIp = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "10.44.0.1";
          description = "Stable overlay host address originated by this node.";
        };
        routes = mkOption {
          type = types.listOf routeType;
          default = [ ];
          description = "Additional overlay prefixes originated by this node.";
        };
        listenAddresses = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          description = ''
            Libp2p listen multiaddrs.

            Null selects collision-free TCP and QUIC defaults. An empty list
            disables inbound libp2p listeners.
          '';
        };
        externalAddresses = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Explicit libp2p addresses advertised to remote peers.";
        };
        bootstrapPeers = mkOption {
          type = types.listOf (
            types.submodule {
              options = {
                id = mkOption {
                  type = types.str;
                  description = "Bootstrap peer ID.";
                };
                address = mkOption {
                  type = types.str;
                  description = "Bootstrap multiaddr without a trailing peer ID.";
                };
              };
            }
          );
          default = [ ];
          description = "Explicit bootstrap peers. Empty uses public runtime defaults.";
        };
        discovery = mkOption {
          type = types.nullOr (
            types.submodule {
              options = {
                mdns = mkOption {
                  type = types.bool;
                  default = true;
                  description = "Enable LAN mDNS discovery.";
                };
                kademlia = mkOption {
                  type = types.bool;
                  default = true;
                  description = "Enable Kademlia discovery.";
                };
                kademliaProviderAdvertisement = mkOption {
                  type = types.bool;
                  default = true;
                  description = "Advertise this overlay through Kademlia provider records.";
                };
                kademliaProtocol = mkOption {
                  type = types.str;
                  default = "/ipfs/kad/1.0.0";
                  description = "Kademlia protocol name.";
                };
                dcutr = mkOption {
                  type = types.bool;
                  default = true;
                  description = "Enable direct connection upgrade through relay.";
                };
                autonat = mkOption {
                  type = types.bool;
                  default = true;
                  description = "Enable AutoNAT reachability probing.";
                };
              };
            }
          );
          default = null;
          description = "Discovery override. Null uses public and LAN daemon defaults.";
        };
        relayServer = mkOption {
          type = types.bool;
          default = false;
          description = "Provide libp2p circuit-relay service for other nodes.";
        };
        relayReservations = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Explicit relay reservation multiaddrs.";
        };
        autoRelay = mkOption {
          type = types.nullOr (
            types.submodule {
              options = {
                maxCandidates = mkOption {
                  type = types.ints.unsigned;
                  default = 16;
                  description = "Maximum automatic relay candidates retained.";
                };
                maxReservations = mkOption {
                  type = types.ints.unsigned;
                  default = 2;
                  description = "Maximum active automatic relay reservations.";
                };
                retryIntervalSeconds = mkOption {
                  type = types.ints.positive;
                  default = 30;
                  description = "Automatic relay reservation retry interval.";
                };
              };
            }
          );
          default = null;
          description = "Automatic relay policy override.";
        };
        relayResources = mkOption {
          type = types.nullOr (
            types.submodule {
              options = {
                maxReservations = mkOption {
                  type = types.ints.positive;
                  default = 128;
                  description = "Maximum relay reservations.";
                };
                maxReservationsPerPeer = mkOption {
                  type = types.ints.positive;
                  default = 4;
                  description = "Maximum relay reservations per peer.";
                };
                reservationDurationSeconds = mkOption {
                  type = types.ints.positive;
                  default = 3600;
                  description = "Relay reservation lifetime.";
                };
                maxCircuits = mkOption {
                  type = types.ints.positive;
                  default = 16;
                  description = "Maximum concurrent relay circuits.";
                };
                maxCircuitsPerPeer = mkOption {
                  type = types.ints.positive;
                  default = 4;
                  description = "Maximum concurrent relay circuits per peer.";
                };
                maxCircuitDurationSeconds = mkOption {
                  type = types.ints.positive;
                  default = 120;
                  description = "Maximum relay circuit lifetime.";
                };
                maxCircuitBytes = mkOption {
                  type = types.ints.positive;
                  default = 131072;
                  description = "Maximum bytes forwarded through one relay circuit.";
                };
              };
            }
          );
          default = null;
          description = "Circuit-relay server resource policy.";
        };
        packetPlane = mkOption {
          type = types.submodule { options = packetPlaneOptions; };
          default = { };
          description = "Owned UDP and QUIC DATAGRAM packet-plane overrides.";
        };
        interfaceName = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "pv0";
          description = "TUN interface name. Null selects a collision-free pvN name.";
        };
        mtu = mkOption {
          type = types.ints.positive;
          default = 1280;
          description = "TUN interface MTU.";
        };
        peers = mkOption {
          type = types.attrsOf (
            types.submodule {
              options = {
                name = mkOption {
                  type = types.nullOr types.str;
                  default = null;
                  description = "Optional peer label.";
                };
                ip = mkOption {
                  type = types.nullOr types.str;
                  default = null;
                  description = "Optional direct peer IP using the default TCP port.";
                };
                vpnIp = mkOption {
                  type = types.nullOr types.str;
                  default = null;
                  description = "Stable overlay host address this peer may originate.";
                };
                addresses = mkOption {
                  type = types.listOf types.str;
                  default = [ ];
                  description = "Explicit direct or relayed peer multiaddrs.";
                };
                routes = mkOption {
                  type = types.listOf routeType;
                  default = [ ];
                  description = "Overlay prefixes this peer may originate.";
                };
              };
            }
          );
          default = { };
          description = "Authorized overlay peers keyed by peer ID.";
        };
        queue = mkOption {
          type = types.nullOr (
            types.submodule {
              options = {
                maxPacketsPerPeer = mkOption {
                  type = types.ints.positive;
                  default = 256;
                  description = "Maximum queued packets per peer.";
                };
                maxBytesPerPeer = mkOption {
                  type = types.ints.positive;
                  default = 524288;
                  description = "Maximum queued bytes per peer.";
                };
                maxPacketAgeMillis = mkOption {
                  type = types.ints.positive;
                  default = 3000;
                  description = "Maximum queued packet age.";
                };
              };
            }
          );
          default = null;
          description = "Per-peer packet queue override.";
        };
        resources = mkOption {
          type = types.nullOr (
            types.submodule {
              options = {
                maxConcurrentPacketStreams = mkOption {
                  type = types.ints.positive;
                  default = 256;
                  description = "Maximum concurrent packet streams.";
                };
                maxConcurrentControlStreams = mkOption {
                  type = types.ints.positive;
                  default = 64;
                  description = "Maximum concurrent control streams.";
                };
                maxInboundPacketsPerPeerPerSecond = mkOption {
                  type = types.ints.positive;
                  default = 4096;
                  description = "Per-peer inbound packet rate limit.";
                };
                maxPairingRequestsPerPeerPerSecond = mkOption {
                  type = types.ints.positive;
                  default = 4;
                  description = "Per-peer pairing request rate limit.";
                };
                maxPendingIncomingConnections = mkOption {
                  type = types.ints.positive;
                  default = 64;
                  description = "Maximum pending incoming connections.";
                };
                maxPendingOutgoingConnections = mkOption {
                  type = types.ints.positive;
                  default = 64;
                  description = "Maximum pending outgoing connections.";
                };
                maxEstablishedIncomingConnections = mkOption {
                  type = types.ints.positive;
                  default = 256;
                  description = "Maximum established incoming connections.";
                };
                maxEstablishedOutgoingConnections = mkOption {
                  type = types.ints.positive;
                  default = 256;
                  description = "Maximum established outgoing connections.";
                };
                maxEstablishedConnectionsPerPeer = mkOption {
                  type = types.ints.positive;
                  default = 8;
                  description = "Maximum established connections per peer.";
                };
                maxEstablishedConnections = mkOption {
                  type = types.ints.positive;
                  default = 512;
                  description = "Maximum established connections.";
                };
              };
            }
          );
          default = null;
          description = "Connection, stream, and admission resource overrides.";
        };
        metricsIntervalSeconds = mkOption {
          type = types.nullOr types.ints.positive;
          default = null;
          description = "Periodic journal metrics interval.";
        };
        controlSocket = mkOption {
          type = types.nullOr types.str;
          default = "/run/p2p-vpn-${name}/control.sock";
          description = "Local daemon control socket. Null disables it.";
        };
        extraArgs = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Additional p2p-vpn up arguments.";
        };
        openFirewall = mkOption {
          type = types.bool;
          default = true;
          description = "Open the instance's default or explicitly listed transport ports.";
        };
        tcpPorts = mkOption {
          type = types.nullOr (types.listOf types.port);
          default = null;
          description = "TCP firewall ports. Null derives the native Nix listener default.";
        };
        udpPorts = mkOption {
          type = types.nullOr (types.listOf types.port);
          default = null;
          description = "UDP firewall ports. Null derives QUIC and mDNS defaults in native Nix mode.";
        };
        packetPlaneUdpPorts = mkOption {
          type = types.listOf types.port;
          default = [ ];
          description = "Additional owned UDP packet-plane firewall ports.";
        };
        packetPlaneQuicPorts = mkOption {
          type = types.listOf types.port;
          default = [ ];
          description = "Additional owned QUIC DATAGRAM packet-plane firewall ports.";
        };
      };
    };

  nixSettingsAreDefault =
    name: instance:
    instance.networkName == name
    && instance.localPeer == null
    && instance.privateKeyFile == null
    && instance.membershipKeyFile == null
    && instance.privateKey == null
    && instance.membershipKey == null
    && instance.previousMembershipTags == [ ]
    && instance.memberRecords == [ ]
    && instance.vpnIp == null
    && instance.routes == [ ]
    && instance.listenAddresses == null
    && instance.externalAddresses == [ ]
    && instance.bootstrapPeers == [ ]
    && instance.discovery == null
    && !instance.relayServer
    && instance.relayReservations == [ ]
    && instance.autoRelay == null
    && instance.relayResources == null
    && packetPlaneIsDefault instance.packetPlane
    && instance.interfaceName == null
    && instance.mtu == 1280
    && instance.peers == { }
    && instance.queue == null
    && instance.resources == null;

  multiaddrPorts =
    protocol: addresses:
    builtins.filter (port: port != 0) (
      concatMap (
        address:
        let
          matched = builtins.match ".*/${protocol}/([0-9]+)(/.*)?$" address;
        in
        optional (matched != null) (lib.toInt (builtins.elemAt matched 0))
      ) addresses
    );
  socketPorts =
    addresses:
    builtins.filter (port: port != 0) (
      concatMap (
        address:
        let
          matched = builtins.match ".*:([0-9]+)$" address;
        in
        optional (matched != null) (lib.toInt (builtins.elemAt matched 0))
      ) addresses
    );

  effectiveTcpPorts =
    name: instance:
    if !instance.openFirewall then
      [ ]
    else if instance.tcpPorts != null then
      instance.tcpPorts
    else if nixMode instance then
      multiaddrPorts "tcp" (effectiveListenAddresses name instance)
    else
      [ ];
  effectiveUdpPorts =
    name: instance:
    if !instance.openFirewall then
      [ ]
    else
      let
        transportPorts =
          if instance.udpPorts != null then
            instance.udpPorts
          else if nixMode instance then
            multiaddrPorts "udp" (effectiveListenAddresses name instance)
          else
            [ ];
        packetUdpPorts =
          if nixMode instance then socketPorts (effectivePacketPlaneListen name instance) else [ ];
        packetQuicPorts =
          if nixMode instance && instance.packetPlane.quicListen != null then
            socketPorts instance.packetPlane.quicListen
          else
            [ ];
        mdnsEnabled = nixMode instance && (instance.discovery == null || instance.discovery.mdns);
      in
      transportPorts
      ++ optional mdnsEnabled 5353
      ++ packetUdpPorts
      ++ packetQuicPorts
      ++ instance.packetPlaneUdpPorts
      ++ instance.packetPlaneQuicPorts;

  serviceForInstance =
    name: instance:
    let
      defaultState = instance.stateDirectory == "/var/lib/p2p-vpn";
      configPreparation =
        if nixMode instance then runtimeConfigScript name instance else jsonConfigCheckScript name instance;
    in
    nameValuePair "p2p-vpn-${name}" {
      description = "p2p-vpn mesh VPN instance ${name}";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      path = [ pkgs.iproute2 ];

      serviceConfig = {
        Type = "simple";
        ExecStartPre = [ configPreparation ];
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
        LoadCredential =
          optionals (nixMode instance && instance.privateKeyFile != null) [
            "private.key:${instance.privateKeyFile}"
          ]
          ++ optionals (nixMode instance && instance.membershipKeyFile != null) [
            "membership.key:${instance.membershipKeyFile}"
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
        RuntimeDirectoryMode = "0700";
        NoNewPrivileges = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        PrivateTmp = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = false;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_NETLINK"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
        UMask = "0077";
      }
      // optionalAttrs (nixMode instance && defaultState) {
        StateDirectory = [ "p2p-vpn/${name}" ];
        StateDirectoryMode = "0700";
      }
      // optionalAttrs (nixMode instance && !defaultState) {
        ReadWritePaths = [ (instanceStateDirectory name instance) ];
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
      example.lab = {
        enable = true;
        peers."REMOTE_PEER_ID" = { };
      };
      description = "Named p2p-vpn daemon instances.";
    };
    generatedConfigs = mkOption {
      type = types.attrsOf types.anything;
      readOnly = true;
      internal = true;
      description = "Secret-free native configurations generated for evaluation tests.";
    };
    effectiveInterfaces = mkOption {
      type = types.attrsOf types.str;
      readOnly = true;
      internal = true;
      description = "Effective per-instance TUN interface names.";
    };
    effectiveListenAddresses = mkOption {
      type = types.attrsOf (types.listOf types.str);
      readOnly = true;
      internal = true;
      description = "Effective per-instance libp2p listeners.";
    };
    identityFiles = mkOption {
      type = types.attrsOf types.str;
      readOnly = true;
      internal = true;
      description = "Effective native-mode identity paths.";
    };
  };

  config = mkIf (enabledInstances != { }) {
    assertions = [
      {
        assertion = pkgs.stdenv.hostPlatform.isLinux;
        message = "services.p2p-vpn requires Linux TUN support.";
      }
      {
        assertion = builtins.length nativeNames <= (65535 - 51820 + 1);
        message = "services.p2p-vpn has too many instances for automatic listener ports.";
      }
      {
        assertion =
          let
            names = mapAttrsToList effectiveInterfaceName nativeInstances;
          in
          builtins.length names == builtins.length (unique names);
        message = "services.p2p-vpn instances must use unique TUN interface names.";
      }
      {
        assertion =
          let
            sockets = builtins.filter (socket: socket != null) (
              mapAttrsToList (_: instance: instance.controlSocket) enabledInstances
            );
          in
          builtins.length sockets == builtins.length (unique sockets);
        message = "services.p2p-vpn instances must use unique control sockets.";
      }
      {
        assertion = builtins.length nativeListenAddresses == builtins.length (unique nativeListenAddresses);
        message = "services.p2p-vpn native instances must use unique libp2p listen addresses.";
      }
      {
        assertion =
          builtins.length nativePacketPlaneListeners == builtins.length (unique nativePacketPlaneListeners);
        message = "services.p2p-vpn native instances must use unique packet-plane listener endpoints.";
      }
    ]
    ++ concatMap (
      name:
      let
        instance = enabledInstances.${name};
        interface = effectiveInterfaceName name instance;
      in
      [
        {
          assertion = builtins.match "^[A-Za-z0-9][A-Za-z0-9_.-]*$" name != null;
          message = "services.p2p-vpn instance name `${name}` contains unsafe path or unit characters.";
        }
        {
          assertion = !nixMode instance || builtins.match "^[A-Za-z0-9_.-]{1,15}$" interface != null;
          message = "services.p2p-vpn.instances.${name} resolves to invalid Linux interface name `${interface}`.";
        }
        {
          assertion = instance.networkName != "";
          message = "services.p2p-vpn.instances.${name}.networkName cannot be empty.";
        }
        {
          assertion =
            !nixMode instance
            || builtins.all (peerId: builtins.match "^[A-Za-z0-9]+$" peerId != null) (attrNames instance.peers);
          message = "services.p2p-vpn.instances.${name}.peers must use non-empty alphanumeric libp2p peer IDs as attribute names.";
        }
        {
          assertion =
            !nixMode instance
            ||
              builtins.length (overlayHostAddresses instance)
              == builtins.length (unique (overlayHostAddresses instance));
          message = "services.p2p-vpn.instances.${name} must not assign the same vpnIp to more than one local or remote peer.";
        }
        {
          assertion =
            !nixMode instance
            || (
              builtins.match "^/[A-Za-z0-9._+/-]+$" instance.stateDirectory != null
              && !(lib.elem ".." (lib.splitString "/" instance.stateDirectory))
            );
          message = "services.p2p-vpn.instances.${name}.stateDirectory must be an absolute path with safe path characters and no `..`.";
        }
        {
          assertion =
            instance.configFile == null
            || (hasPrefix "/" instance.configFile && !hasPrefix builtins.storeDir instance.configFile);
          message = "services.p2p-vpn.instances.${name}.configFile must be an absolute runtime path outside the Nix store.";
        }
        {
          assertion = instance.configFile == null || nixSettingsAreDefault name instance;
          message = "services.p2p-vpn.instances.${name} must use either configFile JSON mode or native Nix settings, never both.";
        }
        {
          assertion = instance.privateKey == null;
          message = "services.p2p-vpn.instances.${name}.privateKey was removed because it exposes identity keys in the Nix store; use automatic identity or privateKeyFile.";
        }
        {
          assertion = instance.membershipKey == null;
          message = "services.p2p-vpn.instances.${name}.membershipKey was removed because it exposes secrets in the Nix store; use membershipKeyFile.";
        }
        {
          assertion =
            instance.privateKeyFile == null
            || (hasPrefix "/" instance.privateKeyFile && !hasPrefix builtins.storeDir instance.privateKeyFile);
          message = "services.p2p-vpn.instances.${name}.privateKeyFile must be an absolute runtime path outside the Nix store.";
        }
        {
          assertion =
            instance.membershipKeyFile == null
            || (
              hasPrefix "/" instance.membershipKeyFile && !hasPrefix builtins.storeDir instance.membershipKeyFile
            );
          message = "services.p2p-vpn.instances.${name}.membershipKeyFile must be an absolute runtime path outside the Nix store.";
        }
        {
          assertion = instance.controlSocket == null || hasPrefix "/" instance.controlSocket;
          message = "services.p2p-vpn.instances.${name}.controlSocket must be absolute or null.";
        }
        {
          assertion =
            instance.autoRelay == null
            || instance.autoRelay.maxReservations <= instance.autoRelay.maxCandidates;
          message = "services.p2p-vpn.instances.${name}.autoRelay.maxReservations cannot exceed maxCandidates.";
        }
        {
          assertion =
            instance.packetPlane.quicListen == null || builtins.length instance.packetPlane.quicListen <= 1;
          message = "services.p2p-vpn.instances.${name}.packetPlane.quicListen supports at most one listener.";
        }
        {
          assertion =
            instance.discovery == null
            || instance.discovery.kademlia
            || !instance.discovery.kademliaProviderAdvertisement;
          message = "services.p2p-vpn.instances.${name} cannot advertise Kademlia providers while Kademlia is disabled.";
        }
      ]
      ++ concatMap (
        peerId:
        let
          peer = instance.peers.${peerId};
        in
        [
          {
            assertion =
              !nixMode instance || builtins.length peer.addresses == builtins.length (unique peer.addresses);
            message = "services.p2p-vpn.instances.${name}.peers.${peerId}.addresses must not contain duplicates.";
          }
        ]
      ) (attrNames instance.peers)
    ) enabledNames;

    environment.systemPackages = [ cfg.package ];
    services.p2p-vpn.generatedConfigs = mapAttrs generatedSettings nativeInstances;
    services.p2p-vpn.effectiveInterfaces = mapAttrs effectiveInterfaceName nativeInstances;
    services.p2p-vpn.effectiveListenAddresses = mapAttrs effectiveListenAddresses nativeInstances;
    services.p2p-vpn.identityFiles = mapAttrs (
      name: instance:
      if instance.privateKeyFile != null then
        instance.privateKeyFile
      else
        defaultPrivateKeyFile name instance
    ) nativeInstances;
    systemd.services = mapAttrs' serviceForInstance enabledInstances;
    systemd.tmpfiles.rules = concatMap (
      name:
      let
        instance = enabledInstances.${name};
      in
      optionals (nixMode instance && instance.stateDirectory != "/var/lib/p2p-vpn") [
        "d ${instanceStateDirectory name instance} 0700 root root -"
      ]
    ) enabledNames;
    networking.firewall.allowedTCPPorts = unique (
      concatMap (name: effectiveTcpPorts name enabledInstances.${name}) enabledNames
    );
    networking.firewall.allowedUDPPorts = unique (
      concatMap (name: effectiveUdpPorts name enabledInstances.${name}) enabledNames
    );
    boot.kernelModules = [ "tun" ];
  };
}
