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

Coverage:

| Scenario | Checks |
| --- | --- |
| Rust package | `package`, `fmt`, `clippy` |
| Release archive | `releaseArchiveSanity` |
| NixOS service | `nixos-module` |
| Minimal LAN | `nixos-vm-minimal-lan` |
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
| Service wiring | `ExecStart` uses the generated config path. |
| Secrets | `privateKeyFile` is injected at service start. |
| Minimal config | ID-only peer configs serialize as compact JSON. |
| Defaults | Discovery, relay, packet-plane, queue, and resources are omitted. |
| Auto relay | Typed policy writes `network.relay.auto` only when set. |
| Firewall | Configured TCP, UDP, and packet-plane ports are opened. |

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

## Latest Local Gate

The 2026-08-10 local operational gate passed:

```sh
nix run .#check-operational
```

Coverage:

| Area | Evidence |
| --- | --- |
| Rust package | Package, format, and high-signal Clippy checks passed. |
| Release archive | Archive sanity check passed. |
| NixOS module | Module evaluation and service assertions passed. |
| Minimal LAN | `nixos-vm-minimal-lan` passed. |
| QUIC datagram | `nixos-vm-quic-datagram` passed. |
| QUIC stream | `nixos-vm-quic-stream` passed. |
| Forced relay | `nixos-vm-forced-relay` passed. |
| Network move | `nixos-vm-network-move` passed. |
| Public repro tooling | Structure and evidence checker tests passed. |

Style and pedantic Clippy lints are advisory.

The gate intentionally does not replace real separated-host proof.

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

Coverage:

| Scenario | Assertion |
| --- | --- |
| Offer CLI | Existing node writes a signed pairing URI. |
| Accept CLI | New node exchanges a live pairing request in its namespace. |
| Generated config | Config validates with inviter address hints and route grants. |
| Live inviter state | Inviter starts without the joiner and installs paired routes live. |
| Replay rejection | Reusing the same offer fails and increments replay metrics. |
| Data plane | The generated config boots and carries overlay ping. |

## Preserve Artifacts

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
nix run .#tun-e2e -- -- --ignored --nocapture
```

The test prints the artifact directory.

It includes logs, generated configs, replay commands, and daemon snapshots.

## Recorded Namespace Smoke

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
