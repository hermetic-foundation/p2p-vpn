# Idle Maintenance

## Status

Infrastructure maintenance is implemented. The demand lifecycle stays
unadvertised until its bilateral dormancy, control-delivery, wake, roaming,
and energy gates pass.

## Transport Policy

| Setting | Policy |
| --- | --- |
| Automatic reservations | One by default in Rust and Nix; preserve explicit limits and reservations |
| Native ping | 60-second interval; existing 20-second timeout |
| Native QUIC | Retain its compatible 5-second keepalive and 10-second idle timeout |
| Application paths | Prefer active QUIC; probes and legacy behavior change only through authenticated bilateral dormancy |
| Incoming anchor | Prefer TCP; retain QUIC-only fallback |

Pinned libp2p-quic 0.13.1 passes its keepalive directly to Quinn. Quinn-proto
0.11.16 negotiates the smaller finite idle timeout, but does not shorten the
configured keepalive to fit that timeout.

- Setting native QUIC keepalive to 60 seconds would break quiet connections to short-timeout peers.
- Raising only the local idle timeout does not override the remote timeout.
- Remove unused QUIC connections instead of weakening compatibility.
- A necessary legacy QUIC fallback can still incur five-second maintenance.
- Native ping streams do not themselves opt into Swarm keepalive accounting; retain leases through their native owner.

Source audit: `libp2p-quic/src/config.rs`, Quinn's `reset_keep_alive`, and the
native ping handler in the versions pinned by `Cargo.lock`.

## Infrastructure Retirement

| Gate | Requirement |
| --- | --- |
| Reachability | At least one verified incoming anchor |
| Discovery | Ordinary public discovery is quiet; do not provoke bootstrap redial churn |
| Identity | Current, established, non-retiring connection ID and authenticated peer |
| Initial grace | At least 60 seconds since establishment |
| Scope | Automatic infrastructure, routing-only peers, and cached bootstrap peers; not overlay members |
| Shared work | No queued/in-flight packets, pending native transports, request/reply codec work, or DHT/AutoNAT work |

Protect:

- Native confirmed reservation connection IDs and all automatic reservation setup in progress.
- Explicitly configured relay peers, including overrides requesting redundant reservations.
- Relays used by active circuits, code pairing, or membership/file-pairing probes.
- Nodes serving circuit-relay clients; do not infer absence of server-side circuits from overlay paths.
- Primary and public-pairing DHT queries, handler streams, RPCs, negotiations, and queued events.
- Native AutoNAT probes, including pre-decode codec work and queued completion events; retiring their transport would trigger retries.

## Ownership

- Inspect native work owners; do not add parallel request or connection registries.
- Include disconnected requests, queued terminal notifications, and pre-decode codec work.
- AutoNAT's read-only native getter inspects its existing request owner, probe maps, and pending actions; future timers are not work.
- Protect shared relay transports while any protocol work or native dial might still be selecting a circuit owner.
- Mark only the chosen connection ID retiring, then ask Swarm to close it.
- Preserve addresses, configured seeds, memberships, and identity records.
- Reuse the existing recovery tick; add no polling timer or maintenance packet.
- Deliberate infrastructure retirement must not clear another connection's lease or start overlay recovery.

## Validation

| Scenario | Evidence Required |
| --- | --- |
| Defaults | Minimal JSON/Nix, partial override, explicit disable, explicit redundancy, one-slot failover |
| Sparse TCP | Retained native TCP anchor accepts fresh inbound work beyond the 60-second ping interval |
| QUIC compatibility | Native defaults unchanged; demanded QUIC and QUIC-only relay fallback remain usable |
| No work | Mature surplus infrastructure closes once, with no new dial/query or metadata eviction |
| Busy | Queues, dials, queries, codec streams, and queued replies prevent pruning |
| Dependencies | Reservations, explicit relays, pairing, circuits, serving roles, and overlay peers survive |
| Exact owner | Prune spare QUIC while the TCP reservation remains ready and accepts fresh incoming control |
| Epochs | Young, stale, unestablished, already-retiring, and mismatched connections are rejected |

Connection counts and wire reductions are not battery measurements. Final
acceptance still requires matched, replicated idle cellular mAh/hour results.

## Evidence: November 12, 2026

| Check | Result |
| --- | --- |
| Workspace/all targets | 1,991 passed; zero failed; 49 ignored |
| Focused pruning | Six passed, including the real socket/lease test |
| Native protocol work | Five request/reply tests and three AutoNAT tests passed |
| TCP incoming path | Spare QUIC closes; TCP reservation survives 62 real idle seconds and accepts fresh relayed control |
| Compatibility | Active QUIC, QUIC-only relay fallback, explicit relay overrides, and recovery regressions passed |
| Nix defaults | Seven relay contracts and nine existing QUIC contracts evaluated true; relay JSON check also built |
| Source packaging | Linux and both Android native packages retain the runtime and native patches; Linux test inventories match |
| Clippy | Correctness, suspicious-code, and performance groups passed; advisory warnings remain |
| Formatting | Workspace Rust and touched Nix modules/tests passed |
| Build resources | One existing 6.3 GiB target; two build jobs; incremental compilation off; no downloads |
| Full Nix/APK | Not built: offline source-check dry run requires 493 uncached derivations, including bootstrap compilers |
| Devices and energy | No APK/service deployment, phone-setting change, or new cellular energy measurement |

- The socket fixture injects the quiet-policy snapshot and establishment grace clock.
- Transport closure, native reservation ownership, the 62-second idle dwell, and fresh incoming control use real local sockets.
- AutoNAT regression checks outstanding work with zero pending transports, then drains dial-back handshakes before asserting completion.
- The initial broad run caught an old two-reservation CLI expectation; the corrected full rerun passed.

```sh
cargo test --offline --locked --workspace --all-targets -- --test-threads=2 --quiet
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
cargo fmt --all -- --check
nix eval --offline --raw .#checks.x86_64-linux.nixos-relay-defaults-eval.text
nix eval --offline --raw .#checks.x86_64-linux.nixos-quic-defaults-eval.text
```

## Remaining Gates

- Negotiate bilateral dormancy and stop idle overlay maintenance at both ends.
- Gate nonessential DHT/AutoNAT maintenance on the negotiated quiet state.
- Deliver membership, DNS, and revocation updates to dormant members on events.
- Verify incoming/outgoing wake, legacy peers, active transfers, relay fallback, and roaming.
- Deploy a validated candidate and measure fresh paired/reversed cellular controls.
- Require at least 90% lower idle app-attributed mAh/hour, corroborated by modem energy.

The native QUIC fallback still has five-second upkeep. Neither these source
changes nor socket tests establish any cellular battery improvement.
