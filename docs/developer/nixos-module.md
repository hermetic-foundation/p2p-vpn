# NixOS Module Design

This document describes the upstream module implementation and test contract.

User configuration lives in [../user/nixos.md](../user/nixos.md).

## Exported Surface

The flake exports:

```nix
nixosModules.default
```

The public option root is:

```text
services.p2p-vpn.instances.<name>
```

Internal read-only values support evaluation tests:

| Option | Purpose |
| --- | --- |
| `generatedConfigs` | Secret-free daemon objects |
| `effectiveInterfaces` | Resolved `pvN` names |
| `effectiveListenAddresses` | Resolved libp2p listeners |
| `identityFiles` | Resolved identity paths |
| `pairingStateFiles` | Resolved encrypted pairing-state paths |
| `membershipStateFiles` | Resolved signed membership-history paths |

These values are not user configuration APIs.

## Configuration Boundary

An enabled instance selects one mode.

| Condition | Mode | Daemon config owner |
| --- | --- | --- |
| `configFile == null` | Native Nix | Module |
| `configFile != null` | JSON | User-provided file |

Native daemon options must remain at defaults in JSON mode.

The assertion prevents partial merging between two schemas.

Systemd-only controls remain available in either mode:

- `metricsIntervalSeconds`
- `controlSocket`
- `extraArgs`
- Firewall controls

## Native Compilation Pipeline

The module compiles one typed instance in five steps:

1. Resolve instance index, ports, interface, and state paths.
2. Convert camel-case Nix options to the daemon schema.
3. Write a secret-free JSON template to the Nix store.
4. Inject runtime credentials into a mode `0600` file under `/run`.
5. Validate the completed file before `ExecStart`.

The runtime JSON is generated output.

It is not read back into Nix and is not a second user configuration source.

## Schema Mapping

| Nix group | Daemon object |
| --- | --- |
| `networkName`, identity, routes | `network` |
| `discovery` | `network.discovery` |
| Relay options | `network.relay` |
| `packetPlane` | `network.packet_plane` |
| `interfaceName`, `mtu` | `interface` |
| `peers` | `peers[]` |
| `queue` | `queue` |
| `resources` | `resources` |

Route strings are normalized to `{ prefix = ...; }` objects.

Peer attributes are converted to an array keyed by their peer ID.

## Defaults

Native instance names are sorted before assigning index `N`.

| Value | Formula |
| --- | --- |
| Interface | `pvN` |
| TCP listener | `4001 + N` |
| QUIC listener | `4001 + N` |
| UDP packet listener | `51820 + N` |
| State | `/var/lib/p2p-vpn/<name>` |
| Runtime | `/run/p2p-vpn-<name>` |

Only native instances consume an index.

JSON instances do not shift native defaults.

Adding a lexically earlier native instance can shift later defaults.

Tests and documentation require users to pin values before such changes.

## Identity Lifecycle

### Automatic Identity

When `privateKeyFile == null`, `ExecStartPre` checks:

```text
/var/lib/p2p-vpn/<name>/private.key
```

If absent, it runs `p2p-vpn keygen` under `umask 077`.

An existing empty key is an error and is never replaced silently.

### External Identity

`privateKeyFile` becomes a systemd `LoadCredential` entry.

The preparation script reads `$CREDENTIALS_DIRECTORY/private.key`.

### Membership Key

`membershipKeyFile` uses the same credential boundary.

Inline compatibility options always fail evaluation.

### Runtime Assembly

The preparation script uses same-directory temporary files and atomic rename.

It runs `p2p-vpn status --config` before installing the final runtime file.

## Pairing Integration

### Running-Daemon Target

`pair ... --instance <name>` resolves:

```text
/run/p2p-vpn-<name>/control.sock
```

The daemon therefore reuses the active module identity.

This includes a `privateKeyFile` delivered through a systemd credential.

### Durable State

Every instance with a control socket receives:

```text
--pairing-state /var/lib/p2p-vpn/<name>/pairing-state.json
```

The daemon encrypts that state with the local identity and network context.

Disabling the control socket also disables the pairing-state argument.

Every native instance also receives:

```text
--membership-state /var/lib/p2p-vpn/<name>/membership-state.json
```

This path remains enabled independently of the local control socket.

The file stores owner-only signed history and is not an encrypted secret envelope.

### Native Artifact

`pair artifacts` emits typed module options on both paired hosts.

Default module settings are omitted from the rendered file.

The renderer preserves only declarative authority:

- Expected local peer ID.
- Signed member records.
- Optional membership-key path.
- Assigned local address and routes.
- Network name when it differs from the instance.

It deliberately omits static `peers` entries.

Static entries would bypass signed-record revocation.

### Secret Materialization

When an authenticated response carries a membership key, the daemon writes:

```text
/var/lib/p2p-vpn/<name>/membership.key
```

The generated module points `membershipKeyFile` at that owner-only file.

Neither secret value is rendered into Nix or runtime diagnostics.

### Restart Recovery

Before normal runtime processing, the daemon:

1. Loads and authenticates encrypted pairing state.
2. Revalidates every durable enrollment.
3. Compacts incompatible applied enrollments against the current declarative authority.
4. Applies prepared forwarding and TUN updates.
5. Loads network- and identity-bound membership history.
6. Reconstructs effective members and kernel routes.
7. Persists finalized signed history atomically.

Prepared enrollments still fail startup when they cannot be recovered safely. An
incompatible enrollment that was already applied is reduced to its replay-safe
receipt instead of preventing the declarative network from starting.

Incoming unknown peers are provisional membership probes.

They cannot use packet, service, or route authority before validation.

After generated Nix is installed, `pair acknowledge` compacts the enrollment.

Nix rebuilds preserve both state files through the stable `StateDirectory`.

Unsupported membership-state versions fail startup instead of being discarded.

## systemd Contract

Each instance creates `p2p-vpn-<name>.service`.

| Property | Value |
| --- | --- |
| Start order | After and wants `network-online.target` |
| Restart | `on-failure`, 5 seconds |
| Stop | Control-socket shutdown when enabled |
| Runtime mode | `0700` |
| State mode | `0700` |
| UMask | `0077` |
| Device policy | Only `/dev/net/tun rw` |
| Capabilities | `CAP_NET_ADMIN`, `CAP_NET_RAW` |

Hardening includes:

- `NoNewPrivileges`
- `ProtectSystem=strict`
- `ProtectHome`
- `ProtectProc=invisible`
- `PrivateTmp`
- Namespace and address-family restrictions

`ProtectKernelTunables` remains disabled for required network behavior.

## Firewall Derivation

When `openFirewall` is true, native mode derives:

- TCP ports from TCP multiaddrs.
- UDP ports from QUIC multiaddrs.
- UDP packet-plane ports from socket listeners.
- UDP `5353` when mDNS is active.

JSON mode cannot be inspected at evaluation time.

Its ports must be supplied through firewall override options.

## Evaluation Assertions

### Platform and Names

- Linux is required.
- Instance names must be safe for paths and units.
- Interface names must satisfy Linux length and character rules.
- State and control paths must be absolute and safe.

### Secret Boundary

- Config and credential paths must be outside `/nix/store`.
- Inline private and membership keys are rejected.
- Native and JSON mode cannot be mixed.

### Isolation

- Interfaces must be unique.
- Control sockets must be unique.
- libp2p listeners must be unique.
- Packet-plane listeners must be unique.
- Local and peer `vpnIp` values must be unique.

### Protocol Shape

- Peer IDs must be non-empty alphanumeric strings.
- Peer addresses cannot be duplicated.
- Auto-relay reservations cannot exceed candidates.
- QUIC packet plane supports at most one listener.
- Provider advertisement requires Kademlia.

Runtime validation remains authoritative for cryptographic and protocol fields.

## Test Ownership

### Public Instance Inspection

`p2p-vpn instance list` scans prepared `/run/p2p-vpn-*/config.json` files.

It emits a separate public-only record containing `instance`, `network`,
`interface`, and `peer_id`. Secret configuration fields are never serialized.

`p2p-vpn instance show NAME` resolves the same standard runtime path directly.

The lifecycle VM proves multi-instance discovery, deterministic ordering, text
output, JSON output, and peer-ID derivation.

| Check | Contract |
| --- | --- |
| `nixos-module` | Minimal, full, multi-instance, JSON, and invalid evaluation |
| `nixos-consumer-flake` | Independent flake imports only the exported module |
| `nixos-vm-module-lifecycle` | Identity persistence, ports, interfaces, restart isolation |
| `nixos-vm-minimal-lan` | Two minimal native nodes carry traffic |
| `nixos-vm-pairing` | Nix-only pairing, identity reuse, restart recovery |
| `nixos-vm-code-pairing-lan` | Code discovery, approval, artifacts, evaluation, traffic |
| `nixos-vm-code-pairing-relay` | DHT locator and isolated relay-only code pairing |
| `nixos-vm-network-move` | LAN, relay fallback, and LAN promotion |
| `nixos-vm-forced-relay` | Circuit-relay data-plane fallback |
| QUIC VM checks | Owned QUIC datagram and stream paths |

Run the module checks:

```sh
nix build .#checks.x86_64-linux.nixos-module --no-link -L
nix build .#checks.x86_64-linux.nixos-consumer-flake --no-link -L
nix build .#checks.x86_64-linux.nixos-vm-module-lifecycle --no-link -L
nix build .#checks.x86_64-linux.nixos-vm-pairing --no-link -L
nix build .#checks.x86_64-linux.nixos-vm-code-pairing-lan --no-link -L
nix build .#checks.x86_64-linux.nixos-vm-code-pairing-relay --no-link -L
```

Run the complete local gate:

```sh
nix run .#check-operational
```

## Adding an Option

Use this checklist:

1. Add a typed option with a conservative default.
2. Map it in `generatedSettings` only when needed.
3. Include it in the JSON-mode default guard.
4. Add evaluation coverage for default and custom values.
5. Add a VM assertion when behavior is operational.
6. Update user option tables and this mapping.

Do not add an option that embeds secret material in a Nix value.

Do not parse a user JSON file into native settings.
