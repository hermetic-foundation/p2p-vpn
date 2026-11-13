# Incoming Anchor Ownership

## Status

Partially implemented. The peer lifecycle remains unadvertised until incoming
ownership, event-driven control delivery, and wake/recovery tests are complete.

The [public relay test](relay-idle-2026-11-12.md) proves one TCP reservation
survived 30 minutes and accepted a fresh sender. It is not cellular energy evidence.

## Requirements

| Item | Contract |
| --- | --- |
| Automatic footprint | One accepted reservation by default; no required new configuration |
| Preferred underlay | Plain TCP to the relay; active direct overlay QUIC remains preferred |
| Explicit overrides | Preserve configured reservations and explicit automatic reservation limits |
| Recovery | Independent of peer payload demand; bounded attempts, deadlines, backoff, alternate candidates |
| Reservation lifetime | Native relay client owns renewal and stop/circuit handling |
| Quiet readiness | A confirmed native reservation, owned live listener, and current non-retiring underlay connection |
| Identity/isolation | Bind actual connection ID to the authenticated relay peer in this runtime/underlay epoch |

## Transport Ambiguity

Libp2p relay 0.21.1 currently reuses the first connection to a relay for a new
reservation. A TCP multiaddr or advertised TCP circuit does not establish that
the reservation itself uses TCP; an existing QUIC connection may be reused.

1. Expose confirmed reservation connection IDs from the native client.
2. Allow an existing authenticated connection to be preferred for new reservations.
3. Select/dial the intended transport before issuing the reservation request.
4. Validate readiness against the actual established endpoint, not advertised addresses.

The preference changes only future native connection selection. It must not
move or reset an existing reservation, circuit, or renewal. No new relay wire
protocol, claimed peer identity, or independent reservation ledger is introduced.

## Acquisition

| State | Action | Deadline/Exit |
| --- | --- | --- |
| Candidate | Rank reachable LAN addresses first, then prefer TCP within locality | Existing bounded candidate pool |
| Connecting | Reuse a usable matching direct connection or explicitly dial it | Existing pending setup deadline and native dial timeout |
| Reserving | Prefer the chosen connection, then install the circuit listener | Native acceptance plus owned live listener |
| Available | Retain actual reservation connection and publish valid circuit addresses | Native renewal; connection/listener loss invalidates readiness |
| Recovering | Release failed ownership, rotate candidates, retain bounded backoff | Retry incoming path even while all data peers are dormant |

- Pending explicit dials retain their connection/underlay epoch ownership.
- Late dialing/establishment events cannot resurrect an expired or old-network attempt.
- QUIC-only candidates remain usable fallbacks; diagnostics must not label them quiet TCP anchors.
- A replacement becomes usable before the old usable anchor is retired where capacity permits.

## Idle Maintenance

- Native libp2p ping uses a 60-second interval with the existing 20-second timeout.
- Native QUIC retains its compatible five-second keepalive and ten-second idle timeout.
- Continue answering incoming pings and native relay lease refreshes.
- Active overlay path probes remain responsible for demanded data-path liveness.
- A silent idle-anchor failure can therefore take about 80 seconds to detect.
- Underlay-change events invalidate old ownership immediately and start existing LAN-first recovery.
- Keep no extra QUIC/DHT connections merely because an address is cached.
- Never prune a live reservation, active circuit dependency, pairing path, or outstanding wire/query work.

This changes maintenance cadence, not authentication or the libp2p ping protocol.
Older peers retain legacy data-path behavior; no peer is declared dormant without
the bilateral lifecycle exchange.

## Readiness And Activation

- A peer-only reservation event is insufficient evidence of the owning transport.
- A stale listener, closing connection, or old underlay epoch is not ready.
- Cached capabilities, external addresses, and a public AutoNAT result alone are not incoming proof.
- Losing the anchor does not revoke dormant memberships or start a full peer recovery storm.
- Do not enable lifecycle advertisement before control updates can reach dormant disconnected members.

## Verification

| Layer | Required Evidence |
| --- | --- |
| Native client | Existing QUIC plus TCP connections; preference chooses TCP without closing QUIC; confirmed ID matches actual owner |
| Ownership | Unconfirmed, expired, closing, wrong-peer, and stale-epoch connections are rejected |
| Acquisition | Explicit transport dial before reservation, bounded timeout, cancellation, retry and alternate candidates |
| Compatibility | Default native relay behavior unchanged unless preference is requested; explicit user settings preserved |
| Recovery | Reservation/listener loss repairs automatically without application data |
| Functional | Fresh incoming payload, active QUIC, relay fallback, membership/DNS/revocation, and roaming |
| Energy | Fresh paired cellular measurements; connection/byte counts are not a 10x battery result |

## Implementation Evidence

| Component | Status |
| --- | --- |
| Native reservation owner | Patched relay 0.21.1 exposes confirmed connection IDs from its existing maps |
| Native connection preference | Explicit selection for future reservations/circuits; existing leases and renewal owners stay unchanged |
| Mixed transport regression | `tests/relay_ownership.rs` checks QUIC-first default, TCP selection, renewal, fresh incoming payload, and owner closure |
| Attempt epochs | Start observations preserve initial epoch, retirement, and establishment time; late success cannot admit stale/retired paths |
| Runtime acquisition/readiness | Two-stage setup; the listener record retains its selected connection ID, which must be natively confirmed |
| Explicit overrides | Automatic attempts exclude explicitly configured relay peer IDs, including while explicit setup is pending |
| Automatic footprint | Rust and native Nix default to one; explicit disable/redundancy settings and one-slot failover are tested |
| Candidate exhaustion | Dedicated bounded native query ownership replenishes an empty pool independently of mesh quietness |
| Sparse heartbeat and pruning | Native ping is sparse; mature surplus infrastructure closes behind a verified incoming anchor and native work fences |
| Dormant control delivery and activation | Pending |
| Cellular energy acceptance | Not measured for this lifecycle |

### Automatic Setup

| Step | Runtime Behavior |
| --- | --- |
| Rank | Keep the retained route alongside validated advertisements; LAN first, TCP before QUIC within locality |
| Reuse | Require matching authenticated peer, current usable epoch, established direct connection, target host and transport |
| Connect | Otherwise own one explicit dial token; use existing bounded dial/source-port policy |
| Reserve | Prefer that native connection before installing the reservation listener |
| Confirm | Require native confirmation, peer acceptance, and the exact owned listener; log the actual transport |
| Expire | Keep the 30-second setup deadline; fence unfinished owned dials against late completion |
| Retry | Preserve configured backoff and candidate-failure eviction; caller-owned connections remain intact |

- The existing 10-second recovery driver runs independently of application traffic.
- Connection and Identify events can advance setup without waiting for its next tick.
- AutoNAT updates reachability/discovery, but cannot bypass transport/epoch selection.
- Repeated ticks do not allocate another dial or extend an attempt's deadline.
- Setup expiry does not close established connections that may carry other native work.
- Terminal failed dials do not accumulate retirement markers during later expiry.
- Remote public Identify advertisements cannot inject a new private address into ranking.
- After TCP failure/backoff, reuse a current authenticated QUIC connection when available.
- Retain up to four validated alternatives per candidate; one native dial tries them sequentially.
- Include QUIC before redundant TCP routes within a locality; preserve LAN-first ordering.
- Cold fallback uses the same owned dial ID and deadline, with no pre-existing QUIC connection.
- Transport fallback keeps the attempt deadline and clears per-peer attempt ownership on expiry.

The [idle maintenance contract](demand-lifecycle-idle-maintenance.md) records
sparse ping and surplus-connection pruning. Overlay dormancy remains gated.
The [incoming recovery contract](demand-lifecycle-relay-recovery.md) records
bounded query ownership, cold transport fallback, and empty-pool replenishment.

The mixed-transport test deliberately advertises a TCP circuit while keeping
an existing QUIC connection. Without explicit selection, the confirmed owner
is QUIC. The accessor makes this mismatch observable without trusting the address.

Epoch regressions exercise repeated start events before/after a network change,
retired pending/established attempts, normal fresh admission, and production
event dispatch. Old failure events must not reapply new-network recovery backoff.

```sh
cargo test --offline --locked --test relay_ownership
cargo test --offline --locked --lib late_start
cargo test --offline --locked --lib obsolete_outgoing_failure
cargo test --offline --locked --lib incoming_anchor
```

These are local transport and ownership tests, not Android, roaming, whole-VPN,
or battery acceptance evidence. The production lifecycle remains unadvertised.

The production-dispatch scenarios cover TCP acquisition/reuse, QUIC reuse, cold
QUIC fallback, and recovery after a real TCP refusal. They accept fresh incoming relayed control
and reject expired, unowned-listener, wrong-connection, retiring, and stale-epoch proof.

Separate scenarios verify that automatic setup does not replace an explicit QUIC
reservation and excludes explicit relay IDs before their reservations are ready.
The TCP-failure scenario advances the injected retry clock, not native network timeouts.

Separate native-discovery scenarios start from an empty candidate pool and from
a real lost reservation. No application payload is needed to acquire a replacement;
the replacement then accepts a fresh incoming relayed control request.

### Validation: November 12, 2026

| Check | Result |
| --- | --- |
| Workspace/all targets | 1,966 passed; zero failed; 49 ignored, including the acquisition/ownership regressions |
| Focused incoming-anchor tests | 12 passed: selection, deadlines, bounded retries, fallback, explicit overrides, and fresh relayed control |
| Final mixed-transport scenarios | Both passed again after test-helper cleanup |
| Focused epoch/dispatch/backoff | Passed; old events stay fenced and current failures retain normal backoff |
| Clippy correctness/suspicious/performance | Passed for workspace/all targets; pre-existing advisory warnings remain |
| Workspace Rust formatting | `cargo fmt --all -- --check` passed; pinned upstream formatting retained |
| Nix source parity | Linux test inventory/source and relay patch matched; both Android source packages include the same patch |
| Android Nix formatting | Passed |
| Full Nix build | Not run: offline dry run requires 493 uncached derivations, including bootstrap compilers |
| APK/device/cellular tests | Not run for this change; no deployment or measured energy claim |

Validation used the already-installed Nix Rust toolchain and existing shared
target directory. The pinned archive matched its lockfile checksum; no toolchain
or crate downloads were needed. The shared target remains 5.6 GiB; no extra target
directory was created. The vendored relay tree is 368 KiB.

### Validation: Bounded Recovery

| Check | Result |
| --- | --- |
| Workspace/all targets | 1,979 passed; zero failed; 49 ignored |
| Focused incoming/lifecycle tests | 26 passed, including cold fallback, empty-pool discovery, replacement after actual relay loss, and public/private routing compatibility |
| Scheduling | Four 24-hour simulations: public/private primary DHTs, fast completion/expiry; 309 starts each, one query maximum, zero capacity rejections |
| Query cancellation | Ordinary suppression preserves incoming repair; incoming reset/expiry preserves publication and unrelated queries; native queued results are purged |
| Provisional candidates | A candidate cannot cancel the running lookup; no extra lookup starts before readiness, expiry, or candidate exhaustion |
| Identify compatibility | AutoNAT-only private routing bootstrap stays connected; public-primary and dual-protocol peers retain public-pairing addresses without mesh redial entries or VPN authority |
| Native transport | TCP refusal falls back to QUIC under one dial ID; the failed TCP route is not advertised |
| Clippy | Workspace/all targets passed correctness, suspicious-code, and performance groups; advisory warnings remain |
| Rust formatting | Workspace check passed |
| Nix source parity | Linux and both Android source packages contain the exact runtime sources and new recovery modules |
| Shared build storage | 5.6 GiB, reusing the existing target with two build jobs and incremental compilation disabled |
| Full Nix/APK/device/cellular | Not run for this increment; no deployments or energy claims |

```sh
cargo test --offline --locked --workspace --all-targets -- --test-threads=2 --quiet
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
cargo fmt --all -- --check
```

- Run builds and tests serially against the shared target.
- An earlier overlapping build replaced a running test executable; two self-spawning tests hit `ENOENT`.
- Both passed unchanged in the final serial workspace run.
- The first discovery fixture inherited public bootstrap seeds; the final socket tests replace both the bootstrap list and native address book with an isolated local seed.

Remaining activation gates include bilateral dormancy, event-driven dormant
control delivery, wake/roaming validation, and matched replicated cellular
measurements. The lifecycle remains unadvertised.
