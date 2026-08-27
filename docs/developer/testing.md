# Testing

Use Nix commands when possible.

They provide the expected Rust toolchain and native dependencies.

## Fast Loop

```sh
nix run .#check-fast
```

## Rust Tests

```sh
nix develop -c cargo test
```

Focused examples:

```sh
nix develop -c cargo test queue::tests
nix develop -c cargo test route::tests
nix develop -c cargo test packet_plane
nix develop -c cargo test overlay
```

## Format

```sh
nix develop -c cargo fmt -- --check
```

## Clippy

```sh
nix develop -c cargo clippy --all-targets -- \
  -D clippy::correctness \
  -D clippy::suspicious \
  -D clippy::perf
```

The release gate fails high-signal Clippy groups.

Style, complexity, and pedantic Clippy lints stay advisory.

This keeps CI stable across nixpkgs Clippy updates.

## Nix Checks

```sh
nix flake check
```

## Operational Release Gate

Run the local Hyprspace-replacement gate:

```sh
nix run .#check-operational
```

List the gate without running it:

```sh
nix run .#check-operational -- --list
```

Skip VM checks during iteration:

```sh
nix run .#check-operational -- --skip-vms
```

Static checks build as one batch.

VM checks use one Nix job so concurrent QEMU load cannot consume recovery deadlines.

Coverage:

| Scenario | Checks |
| --- | --- |
| Rust package | `package`, `fmt`, `clippy` |
| Release archive | `releaseArchiveSanity` |
| NixOS service | `nixos-module` |
| Standalone consumer | `nixos-consumer-flake` |
| Module lifecycle | `nixos-vm-module-lifecycle` |
| Minimal LAN | `nixos-vm-minimal-lan` |
| Offline pairing | `nixos-vm-pairing` |
| Code pairing on LAN | `nixos-vm-code-pairing-lan` |
| Code pairing over relay | `nixos-vm-code-pairing-relay` |
| Membership convergence | `nixos-vm-membership-convergence` |
| QUIC datagram | `nixos-vm-quic-datagram` |
| QUIC stream | `nixos-vm-quic-stream` |
| Forced relay | `nixos-vm-forced-relay` |
| Network move | `nixos-vm-network-move` |
| Public tooling | Public relay and VPN repro structure checks |
| Evidence validation | `public-vpn-evidence-check`, `public-vpn-move-evidence-check` |

This gate is local.

It does not replace the real hotspot/VPN evidence run.

## NixOS Module Check

Run the module evaluation check:

```sh
nix build .#checks.x86_64-linux.nixos-module
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Native mode | A one-line instance compiles useful runtime defaults. |
| JSON mode | `configFile` remains a strict passthrough mode. |
| Identity | Automatic and explicit identity paths are distinct. |
| Secrets | Explicit secret files use systemd credentials. |
| Instances | Interface, libp2p, and packet-plane defaults are deterministic. |
| Assertions | Conflicts, mixed modes, inline secrets, and unsafe paths fail. |
| Firewall | Effective TCP, UDP, and packet-plane ports are opened. |

## Standalone Consumer Flake

Run the external-consumer check:

```sh
nix build .#checks.x86_64-linux.nixos-consumer-flake
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Flake output | `nixosModules.default` imports from another flake. |
| Minimal declaration | `instances.lab.enable = true` evaluates. |
| Full closure | The consumer NixOS system closure builds. |
| Service | The upstream package and generated config are wired. |

## NixOS VM Module Lifecycle

Run the lifecycle check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-module-lifecycle
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Automatic identity | First start creates a persistent owner-only key. |
| Restart | Identity and generated runtime settings remain stable. |
| Multiple instances | Services, state, interfaces, and ports are isolated. |
| Shutdown | systemd stops the daemon through its control socket. |

## NixOS VM Mesh

Run the two-node VM mesh check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-mesh
```

Equivalent explicit minimal-config alias:

```sh
nix build .#checks.x86_64-linux.nixos-vm-minimal-lan
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Minimal config | Peer IDs plus route ownership are enough. |
| Generated JSON | No peer addresses, relay block, or discovery override. |
| Discovery | No explicit peer dial addresses are configured. |
| TUN setup | Both nodes create default `pv0`. |
| Data plane | Bidirectional ping crosses the overlay. |
| Packet plane | A direct LAN packet-plane session is negotiated. |

## NixOS VM QUIC Datagram

Run the QUIC datagram preference check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-quic-datagram
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Test config | libp2p QUIC stream and owned QUIC datagram endpoints are explicit. |
| Packet plane | UDP packet-plane endpoints are disabled. |
| Data plane | Bidirectional ping crosses `pv0`. |
| Path proof | Selected path is `direct_quic_datagram`. |
| Preference | QUIC stream path is healthy but unused for packet fallback. |
| Metrics | QUIC datagram packets and QUIC sessions are counted. |

## NixOS VM QUIC Stream

Run the direct QUIC stream fallback check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-quic-stream
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Test config | libp2p QUIC listen address is explicit. |
| Packet plane | Owned UDP and owned QUIC packet planes are disabled. |
| Data plane | Bidirectional ping crosses `pv0`. |
| Path proof | Selected path is `direct_quic_stream`. |
| Connection pin | `daemon-paths` reports a live `connection_id`. |
| Metrics | QUIC stream fallback packets are counted. |
| Fallback order | TCP and relay fallback counters stay at zero. |

## NixOS VM Forced Relay

Run the three-node forced-relay check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-forced-relay
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Topology | Data nodes cannot reach each other directly. |
| Relay | Both data nodes can reach the relay. |
| Discovery | Public discovery is disabled for isolation. |
| Minimal config | `vpnIp` plus peer IDs carry overlay IPs. |
| Data plane | Bidirectional ping crosses `pv0`. |
| Relay proof | Data nodes record relay circuit usage. |
| Candidate hygiene | Overlay IPs are not advertised as underlay endpoints. |

## NixOS VM Network Move

Run the move-recovery check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-network-move
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Minimal config | Generated JSON has no relay routes or discovery override. |
| Initial LAN | mDNS discovers a direct path. |
| Auto relay | Both moving peers accept relay reservations from defaults. |
| Move away | The moved node loses LAN reachability. |
| Relay fallback | `pv0` traffic recovers through discovered relay paths. |
| No config change | Generated config hashes stay unchanged across move and return. |
| Return to LAN | The selected path promotes back to direct. |

The 2026-08-10 VM run passed with minimal NixOS configs.

Observed path events:

| Event | Evidence |
| --- | --- |
| Direct start | Initial `pv0` traffic used a direct LAN path. |
| Relay fallback | Relay paths and circuit counters appeared after movement. |
| Direct recovery | State returned to `selected_path direct_*` after LAN return. |

## NixOS VM Pairing

Run the native pairing check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-pairing
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Minimal baseline | Both systems begin with native Nix instances. |
| Identity | Accept reuses the joiner's persistent module identity. |
| Output mode | Accept emits Nix only; no JSON file is created. |
| Defaults | The fragment relies on module listeners, ports, MTU, and interface. |
| Evaluation | The fragment merges over `enable = true` through the upstream module. |
| Security | A replayed one-time URI is rejected with diagnostics. |
| Data plane | Bidirectional traffic crosses the paired overlay. |
| Restart | Signed membership restores authorization without re-pairing. |

## NixOS VM Code Pairing on LAN

Run the peerless code-pairing check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-code-pairing-lan -L
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Baseline | Both native instances start with no configured peers. |
| Identity | Services use agenix-style runtime identity paths. |
| Discovery | Joiner finds inviter through mDNS from only the code. |
| Approval | Inviter exposes peer ID, fingerprint, address, and routes. |
| Live install | Both daemons apply signed membership without restart. |
| Data plane | Five bidirectional ICMP packets cross `pv0`. |
| Restart | Both services restore the durable enrollment. |
| Artifacts | Both sides emit matching, secret-free native Nix. |
| Evaluation | Generated fragments merge through the exported module. |
| Acknowledgment | Receipt-bound compaction removes artifact payloads. |
| Rebooted config | Evaluated generated configs carry traffic. |

## NixOS VM Code Pairing over Relay

Run the isolated relay check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-code-pairing-relay -L
```

Topology:

```text
node A -- VLAN 1 -- relay -- VLAN 2 -- node B
```

The edge nodes have no underlay route to each other.

Each edge knows only its side of the relay.

Coverage:

| Scenario | Assertion |
| --- | --- |
| Baseline | Edge configs contain no overlay peers. |
| Isolation | Direct edge-to-edge underlay ping fails. |
| Relay | Both reservation requests are accepted. |
| DHT | Inviter republishes and joiner resolves the code locator. |
| Approval | Candidate remains blocked until local approval. |
| Pairing path | Status and metrics report relay transport. |
| Data path | Five bidirectional ICMP packets cross the relay. |
| Path state | Both peers select `circuit_relay`. |
| Restart | Both services recover enrollment and traffic. |

## NixOS VM Membership Convergence

Run the four-node convergence check:

```sh
nix build --no-link \
  .#checks.x86_64-linux.nixos-vm-membership-convergence \
  -L
```

Topology:

```text
node A --admits-- node B --admits-- node C
   \              |                 /
    +----- shared relay infrastructure -----+
```

The relay has the same network name but no membership grant.

It remains infrastructure and cannot originate overlay traffic.

Coverage:

| Scenario | Assertion |
| --- | --- |
| Minimal config | All edge configurations start with empty static peer lists. |
| Delegated admission | Root `A` admits `B`; authorized member `B` then admits `C`. |
| Full convergence | `B` and `C` learn and validate each other automatically. |
| Derived routes | Both install the other's built-in `/32` route on `pv0`. |
| Direct LAN | Bidirectional traffic selects direct paths. |
| Isolated restart | `B` restores `C` and its route while `A` and `C` are offline. |
| Simultaneous restart | All three edge daemons recover the full mesh. |
| Network move | `C` moves to an isolated VLAN without a config change. |
| Relay fallback | `B` and `C` recover through `circuit_relay`. |
| Cold relay restart | Relay-only traffic survives all edge daemon restarts. |
| LAN return | Direct paths replace relay paths automatically. |
| Persistence | Owner-only state files retain all three signed records. |
| Recovery bounds | Relay limits remain available and dial counters never exceed `128`. |
| DNS convergence | Short and canonical names resolve on every edge node. |
| Indirect DNS traffic | Never-paired `A` and `C` ping one another by name. |
| DNS lifecycle | Expiry, conflict, rename, revocation, and restart fail closed. |

The test compares generated config hashes before and after movement.

No static peer, underlay address, or manual route is introduced during recovery.

## Physical Four-Member LAN Audit

The 2026-08-26 audit used four admitted NixOS hosts.

| Host | Overlay IPv4 |
| --- | --- |
| `midi-framework-laptop` | `100.64.63.174` |
| `midi-thinkpad-250` | `100.64.39.219` |
| `midi-thinkpad-120` | `100.64.124.157` |
| `midi-desktop-1` | `100.64.50.33` |

| Assertion | Result |
| --- | --- |
| Membership | Each host retained four signed records. |
| Routes | Each host installed three derived IPv4 and IPv6 host routes. |
| Full mesh | All 12 directed host pairs connected without pairwise enrollment. |
| Sustained TCP | 240 of 240 pre-restart SSH-port connections passed. |
| Daemon restart | Desktop restored its records and routes from native state. |
| Path recovery | Desktop observed healthy UDP, QUIC-stream, and TCP to every peer. |
| Post-restart TCP | 120 of 120 directed connections passed. |
| First activation | IPv6 `nodad` avoided a tentative-source route race. |
| Service health | Activation and restart both reported zero automatic retries. |

Public IPFS discovery also supplied private-address claims from unrelated peers.

The source-aware filter rejected those claims. Local mDNS candidates remained eligible.

## Offline Pairing Integration

Run direct live pairing:

```sh
nix run .#tun-e2e -- tun_namespace_pair_accept_crosses_live_pairing_overlay -- --ignored --exact --nocapture
```

Run relay-assisted live pairing:

```sh
nix run .#tun-e2e -- tun_namespace_pair_accept_crosses_relayed_live_pairing_overlay -- --ignored --exact --nocapture
```

Preflight a public relay reservation candidate:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --require-relay-reservation \
  --max-validation-candidates 4 \
  --timeout-seconds 45
```

Discovery-only public pairing needs this to pass.

Run the live public relay pairing smoke:

```sh
P2P_VPN_LIVE_RELAY_MULTIADDR=/ip4/203.0.113.10/udp/4001/quic-v1/p2p/12D3... \
nix develop -c cargo test \
  live_pair_accept_uses_public_relay_for_discovery_only_offer \
  -- --ignored --nocapture
```

Accepted relay inputs:

| Variable | Shape |
| --- | --- |
| `P2P_VPN_LIVE_RELAY_MULTIADDR` | One direct relay multiaddr. |
| `P2P_VPN_LIVE_RELAY_MULTIADDRS` | Comma- or newline-separated relay multiaddrs. |
| `P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS` | Optional timeout. Default is `45`. |

The timeout covers relay reservation and live pairing exchange.

Relay multiaddrs must include `/p2p/RELAY`.

They must not include `/p2p-circuit`.

The smoke test returns `ok` without env vars.

That skip only proves the harness compiles.

A real public proof requires at least one reachable relay candidate.

Coverage:

| Scenario | Assertion |
| --- | --- |
| Direct offer | `pair accept` writes a config with direct inviter hints. |
| Relay offer | Discovery-only URI embeds relay reservation hints. |
| Relay accept | Joiner contacts inviter through `/p2p-circuit`. |
| Generated config | Relayed inviter address is preserved after accept. |
| Data plane | Ping crosses the generated overlay config. |
| Daemon state | Inviter accepts the pairing request live. |
| Public relay smoke | Optional ignored test exercises discovery-only accept through a real relay. |

## Latest Local Gate

The 2026-08-23 local operational gate passed.

The 2026-08-25 membership convergence check and full Rust suite also passed.

```sh
nix run .#check-operational
```

Coverage:

| Area | Evidence |
| --- | --- |
| Rust package | Package, format, and high-signal Clippy checks passed. |
| Release archive | Archive sanity check passed. |
| NixOS module | Module evaluation and service assertions passed. |
| Consumer flake | Standalone import and full system closure passed. |
| Module lifecycle | Automatic identity and multi-instance isolation passed. |
| Minimal LAN | `nixos-vm-minimal-lan` passed. |
| Code pairing LAN | One-time code, inviter approval, traffic, and restart recovery passed. |
| Code pairing relay | DHT discovery, relay transport, approval, and traffic passed. |
| Membership convergence | Independent pairings formed a three-member mesh with authenticated DNS. |
| Durable membership | Offline restart restored learned members and routes. |
| Membership movement | Direct LAN, isolated relay, cold restart, and LAN return passed. |
| Pairing artifacts | Secret-free native Nix output evaluated with agenix paths. |
| Offline pairing | Offer-file compatibility and replay rejection passed. |
| QUIC datagram | `nixos-vm-quic-datagram` passed. |
| QUIC stream | `nixos-vm-quic-stream` passed. |
| Forced relay | `nixos-vm-forced-relay` passed. |
| Network move | `nixos-vm-network-move` passed. |
| Public repro tooling | Structure and evidence checker tests passed. |

Style and pedantic Clippy lints are advisory.

The gate intentionally does not replace real separated-host proof.

Focused network-move reruns also passed from fresh VM state:

| Reservation transport | Recovery evidence |
| --- | --- |
| QUIC | Relay fallback completed in 25.7 seconds; LAN promotion completed in 8.3 seconds. |
| TCP | Ping closed the stale reservation; replacement was accepted in 0.23 seconds. |

## Underlay Candidate Hygiene

Run the focused regression tests:

```sh
nix develop -c cargo test overlay
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Listener expansion | Configured VPN prefixes are not direct dial candidates. |
| Concrete listeners | Concrete VPN listeners are rejected as direct candidates. |
| Observed UDP endpoints | VPN endpoints are rejected as packet-plane candidates. |
| Configured packet endpoints | VPN endpoints are rejected before advertisement. |

This protects network-move recovery.

The daemon must find a new LAN, relay, or public path.

## Public VPN Evidence Check

Validate the two-host evidence checker:

```sh
nix build .#checks.x86_64-linux.public-vpn-evidence-check
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Complete evidence | Health, ping, supported path, packet session, relay, direct, DCUtR, and QUIC checks pass. |
| Path-specific evidence | Direct QUIC datagram, direct QUIC stream, direct TCP stream, and relay stream requirements are checked separately. |
| Missing relay proof | `--require-relay` fails when Host B has no relay evidence. |
| Report output | The generated JSON report marks each named check. |

Use it after a real hotspot or VPN split:

```sh
nix run .#public-vpn-evidence-check -- \
  --host-a HOST_A/vpn-repro-evidence.json \
  --host-b HOST_B/vpn-repro-evidence.json \
  --require-relay \
  --require-config-match \
  --write-report public-vpn-proof.json
```

Add path-specific requirements when the proof must show a transport layer:

| Flag | Evidence |
| --- | --- |
| `--require-direct-quic-datagram` | QUIC packet-plane session or direct QUIC datagram path. |
| `--require-direct-quic-stream` | Healthy direct QUIC stream path or fallback packets. |
| `--require-direct-tcp-stream` | Healthy direct TCP stream path or fallback packets. |
| `--require-relay-stream` | Healthy relay path or relay fallback packets. |

Add `--require-direct --require-dcutr --require-quic-session` for strict
hole-punch evidence.

## Public Network-Move Proof

This is the repeatable real-world gate.

It must use the same minimal configs for every phase.

| Phase | Topology | Check |
| --- | --- | --- |
| LAN baseline | Both hosts on the same LAN | `--require-direct --require-config-match`. |
| Public split | One host on hotspot or VPN | `--require-relay --require-config-match` unless direct public recovery is proven. |
| LAN return | Both hosts on the same LAN again | `--require-direct --require-config-match`. |

Required evidence per phase:

| Evidence | Source |
| --- | --- |
| Host A proof | `HOST_A/vpn-repro-evidence.json` |
| Host B proof | `HOST_B/vpn-repro-evidence.json` |
| Machine report | `public-vpn-evidence-check --write-report` |
| Daemon paths | `daemon-paths-final.json` from both hosts |
| Metrics | `daemon-status-prometheus-final.txt` from both hosts |
| Ping output | `ping.txt` from both hosts |

Capture phases from the same running daemon:

```sh
nix run .#public-vpn-capture -- \
  --artifact-dir PHASE_A \
  --config HOST_A_CONFIG.json \
  --socket RUN_DIR/control.sock \
  --daemon-log RUN_DIR/p2p-vpn-daemon.log \
  --ping-target 10.42.0.2 \
  --phase lan-baseline
```

Use the same config path and socket for every phase on that host.

| Phase | Capture Flags |
| --- | --- |
| LAN baseline | Use default packet-session readiness. |
| Public split | Require relay in the checker, not in capture. |
| LAN return | Use default packet-session readiness. |

Invalid proof:

| Pattern | Reason |
| --- | --- |
| Editing peer addresses between phases | Discovery was not automatic. |
| Adding OS routes by hand | Routing was not owned by p2p-vpn. |
| Restarting with different topology config | Movement recovery was not proven. |
| One-sided evidence only | Bidirectional operation was not proven. |

The goal is complete only after the public split phase proves overlay ping and
path recovery without manual route edits.

Validate all phases together:

```sh
nix run .#public-vpn-move-evidence-check -- \
  --lan-baseline-host-a LAN_A/vpn-repro-evidence.json \
  --lan-baseline-host-b LAN_B/vpn-repro-evidence.json \
  --public-split-host-a SPLIT_A/vpn-repro-evidence.json \
  --public-split-host-b SPLIT_B/vpn-repro-evidence.json \
  --lan-return-host-a RETURN_A/vpn-repro-evidence.json \
  --lan-return-host-b RETURN_B/vpn-repro-evidence.json \
  --write-report public-vpn-move-proof.json
```

This also rejects config changes between phases.

Validate the checker itself:

```sh
nix build .#checks.x86_64-linux.public-vpn-move-evidence-check
```

## Namespace E2E

These tests require Linux namespace and TUN support.

Run preflight first:

```sh
nix run .#namespace-preflight
```

Run the full ignored namespace suite:

```sh
nix run .#tun-e2e -- -- --ignored --nocapture
```

Run a focused relay promotion case:

```sh
nix run .#tun-e2e -- \
  tun_namespace_relay_overlay_promotes_to_direct_path \
  -- --ignored --exact --nocapture
```

Run the live pairing smoke case:

```sh
nix run .#tun-e2e -- \
  tun_namespace_pair_accept_crosses_live_pairing_overlay \
  -- --ignored --exact --nocapture
```

Run the code-pairing case:

```sh
nix run .#tun-e2e -- \
  tun_namespace_code_pairing_crosses_peerless_overlay \
  -- --ignored --exact --nocapture
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Offer CLI | Existing node writes a signed pairing URI. |
| Accept CLI | New node exchanges a live pairing request in its namespace. |
| Generated config | Config validates with inviter address hints and route grants. |
| Live inviter state | Inviter starts without the joiner and installs paired routes live. |
| Replay rejection | Reusing the same offer fails and increments replay metrics. |
| Data plane | The generated config boots and carries overlay ping. |

Code-pairing namespace coverage:

| Scenario | Assertion |
| --- | --- |
| Peerless startup | Both daemons start with empty peer lists. |
| Durable state | Mutations use encrypted per-daemon state files. |
| Code CLI | Open and join operate through real Unix control sockets. |
| LAN discovery | Join status reports mDNS candidates. |
| Approval | Joiner remains pending before inviter approval. |
| Enrollment | Both live runtimes validate the new peer. |
| Data plane | Five ICMP packets succeed in both directions. |

## Preserve Artifacts

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
nix run .#tun-e2e -- -- --ignored --nocapture
```

The test prints the artifact directory.

It includes logs, generated configs, replay commands, and daemon snapshots.

## Recorded Namespace Smoke

On 2026-08-22, the focused code-pairing namespace proof passed:

```text
1 passed; 0 failed; finished in 12.42s
```

It started two peerless daemons and carried five ICMP packets each way.

On 2026-08-04, the ignored namespace suite passed:

```text
7 passed; 0 failed; finished in 18.54s
```

Coverage:

- Direct static-peer overlay ping.
- mDNS-discovered packet forwarding.
- DHT/bootstrap-discovered packet forwarding.
- Circuit-relay fallback packet forwarding.
- Signed-invite onboarding over relay.
- Live pair offer/accept onboarding.
- Relay-to-direct promotion with DCUtR.
- Owned QUIC packet-plane forwarding.

## Two-Host LAN Evidence

Artifacts from the 2026-08-05 LAN proof:

```text
/tmp/p2p-vpn-lan-two-host-20260806T003426Z/direct-run-builtin
/tmp/p2p-vpn-lan-two-host-20260806T003426Z/forced-relay/run-firewalled-wide
```

Results:

| Scenario | Result |
| --- | --- |
| Direct LAN | Bidirectional ping passed. |
| Forced relay | Bidirectional `5/5` ping passed. |
| Forced relay path | Data peer selected `circuit_relay`. |
