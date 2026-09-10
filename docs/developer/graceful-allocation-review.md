# Graceful Allocation Review

## Purpose

Measure real namespace daemons after the runner, Tokio runtime and configuration
scope return. Earlier pressure/churn captures killed the process and therefore
could not demonstrate graceful release of its runtime-owned allocations.

## Opt-In Fixture

| Setting | Value |
| --- | --- |
| Feature | `allocation-review` |
| Environment | `P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN=1` |
| Supported workloads | Direct UDP, TCP queue pressure, lifecycle churn |
| Default | Unchanged kill/reap cleanup when the option is absent |
| Shutdown | Existing control-socket request; exact acknowledgement required |
| Request / exit limits | Two seconds / five seconds per child |
| Resource checks | TUN absent after runtime drop; control socket absent after successful child exit |
| Checkpoints | Before child scope; after return; after a fixed 100 ms settling delay |

The option requires retained evidence and rejects unsupported values or a build
without allocation instrumentation. Reproduction scripts retain the option.
Failure still triggers the existing owned-child cleanup; it is not a passing exit.

## Measurement Records

- `allocation_sample`: existing runner-start, periodic and runner-return records.
- `allocation_lifecycle_sample`: child-scope records with their own monotonic clock.
- Subtract `before_child` from `child_returned` and `child_settled` for residual bytes/blocks.
- Do not combine elapsed times from the two record prefixes into one timeline.

Residual process-global initialization is possible. A normal exit and removed
interface do not prove zero retained heap; residual allocations still require
comparison with the runtime/global-timer findings and attribution where needed.

## Frozen Validation

1. Run parsing, exit-status/deadline and observation-wrapper tests.
2. Recalibrate the changed integration executable before measurement.
3. Run a direct-UDP admission with a 100-second outer / existing 90-second inner limit.
4. If admission passes, run packet- and byte-profile five-round captures with original 480/450-second limits.
5. Preserve strict traffic, queue, timing and artifact gates; do not extend failed deadlines.
6. Record source/binary hashes, checkpoint deltas, normal exits and resource disappearance.

No builds overlap measurements. The 10 GiB temporary-storage cap and existing
8 MiB JSON / 2 MiB node-log caps remain unchanged. No physical devices, services
or public-network campaign are involved.

## Baseline Preservation

The earlier calibrated integration executable is retained at
`/tmp/p2p-vpn-review-tun-7bd69360`, with SHA-256
`7bd693606049cc7df7de8d5294ab91fe2bab8dca81fdbfdd4b3c77c747a5ad04`.

## Status

The corrected direct admission and both five-round
[graceful pressure captures](graceful-pressure-results.md) pass. Reconnect teardown
and residual attribution remain open; earlier process-kill captures retain their limits.

## Reproduced Failure

The first direct admission failed after 15.08 seconds, exit 101. The control
request was acknowledged, but node A failed `TUN interface survived runtime drop`.
The parent then used its existing failure cleanup; node B has no graceful pass.

| Artifact | Value |
| --- | --- |
| Integration binary SHA-256 | `7f80e7a50ee72399cfd8e1d613ae9f77a2fc59b10b4dc8f75c6c27ba0418cc7e` |
| Outer log | `/tmp/p2p-vpn-graceful-direct-1.log` |
| Outer log SHA-256 | `499d2176d8ddab4f350b41c4d24e4765e7d5647915ca70896b73caf2d47f69ed` |
| Evidence directory | `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.87069e4afac000cb` |
| Node A log SHA-256 | `2d5cc177d8b4f2ee70994b6ca95e2477908ff67b13778687e53bcf67b04789b5` |
| Last runner sample | 185,995 live requested bytes; 710 live blocks |

The detached Linux reader blocked in `read()` and checked channel closure only
after receiving another packet. It retained the TUN descriptor and metrics after
the runner returned. This is a lifecycle defect, not the earlier debugger overhead.

## Candidate Correction

- Add an optional packet-reader cancellation callback without changing existing constructors.
- Use Linux nonblocking I/O with readiness waits and an explicit cancellation wakeup.
- Close the receiving channel before cancellation, then join cancellable workers.
- Preserve blocking writer behavior through readiness waits; reject incomplete packets.
- Keep Android's existing platform-owned stop mechanism; no physical-device change.

Cancellation is sticky and event-driven, without a recurring idle polling timer.
Descriptor ownership remains with `tun`; no raw descriptor closes or injected
packets are used to wake the reader. `mio` reuses the locked, cached version.

Focused tests cover pre-read/idle cancellation, delivery after `WouldBlock`, and
worker/metrics release with a full packet channel. Shared-Rust validation passes
below; sustained post-fix measurements remain required.

## Corrected Direct Admission

The unchanged direct admission passes in 15.38 seconds with both child exits
successful. Both TUN interfaces disappear after runtime drop; both control sockets
are absent after exit. No deadline or traffic assertion was changed.

| Artifact | Value |
| --- | --- |
| Integration binary | `/tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507` |
| Binary SHA-256 | `6516ee1bcb7b227fac264fabfd6e7a47a86d0f9fdba93e15819bea32953fa6ab` |
| Outer log | `/tmp/p2p-vpn-graceful-direct-2.log` |
| Outer log SHA-256 | `605c9343a66e0d7580a679e41babf6895d939f27077e73b987ff456a3feb67b6` |
| Evidence directory | `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.f3f9226ec44fc0a6` |
| Node A log SHA-256 | `a6e27e9581cb7492f5b96c8eda56adf1598ee3190cd154bb7f9144313a9b0029` |
| Node B log SHA-256 | `9c1b999c373ed7a350d622ad4eef6be7f7e17823b85006da71201eebec0e5425` |
| Calibration | `/tmp/p2p-vpn-tun-cancel-calibration-current.log`: passed |

| Checkpoint | Node A Bytes / Blocks | Node B Bytes / Blocks |
| --- | ---: | ---: |
| Before child | 21,599 / 423 | 21,599 / 423 |
| Child returned | 62,400 / 513 | 62,400 / 513 |
| Child settled | 62,400 / 513 | 62,400 / 513 |
| Settled minus before | 40,801 / 90 | 40,801 / 90 |

These are requested Rust allocations, not RSS or a zero-leak certification.
Equal residuals and a flat 100 ms interval do not attribute the remaining owners.
The subsequent pressure captures are linked above. Reconnect churn and final
resource acceptance still need the corrected runtime and residual attribution.

```sh
env -u P2P_VPN_TUN_E2E_IDLE_SECONDS \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN=1 \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  timeout --signal=TERM --kill-after=10s 100 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507 \
  tun_namespace_ping_crosses_two_node_overlay \
  --ignored --exact --nocapture --test-threads=1
```

## Validation

| Check | Evidence |
| --- | --- |
| Readiness unit tests | Two pass; `/tmp/p2p-vpn-tun-cancel-unit-1.log` |
| Worker ownership regression | Passes in three-test selection; `/tmp/p2p-vpn-tun-cancel-unit-2.log` |
| Locked offline workspace | 1,512 pass; 40 opt-in exclusions; `/tmp/p2p-vpn-tun-cancel-workspace-2.log` |
| Clippy | Workspace/all-targets with `allocation-review`; required correctness/suspicious/performance groups pass |
| Clippy log | `/tmp/p2p-vpn-tun-cancel-clippy.log`; existing style advisories remain |
| Formatting | Changed Rust sources pass rustfmt check |
| Nix source parity | Evaluated locked check body passes with cached tools outside the Nix sandbox |
| Source parity log | `/tmp/p2p-vpn-tun-cancel-source-check.log`; packaged readiness directory also matches |
| Instrumented fixture units | 63 pass; 27 opt-in exclusions; `/tmp/p2p-vpn-tun-cancel-integration-units.log` |
| Android native | Locked/offline x86_64/API 26 build passes; `/tmp/p2p-vpn-tun-cancel-android-build.log` |
| Formal model | No Lean source/model for OS descriptor wakeups in this repository; executable regressions cover this boundary |

Android JNI SHA-256:
`95097ad6c543b135da534ad2b5eeb8140382172dfdda01c9df311d35a843903a`.
This is compilation evidence, not an APK deployment, emulator run or device test.

The first workspace run failed in the randomized address-retention fixture:
two peers derived `100.64.57.52`, and route validation rejected their conflicting
ownership. The unchanged full rerun passed; no collision assertion was weakened.

Preserved failure: `/tmp/p2p-vpn-tun-cancel-workspace.log`. This fixture collision
is separate from the TUN shutdown defect. A rerun does not make its random input
selection deterministic or establish a production routing regression.
