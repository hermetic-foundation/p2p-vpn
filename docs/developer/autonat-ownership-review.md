# AutoNAT Ownership Diagnostic

## Status

The final four-capture matrix passes. Repeated refusals on one connection plateau
after first use in this isolated stack. The later [timer intervention](global-timer-allocation-review.md)
attributes the 224-byte post-runtime-drop difference to process-global timer
capacity. Full-daemon memory ownership remains a separate investigation.

## Frozen Workload

This test-only diagnostic follows the [event correlation](autonat-allocation-investigation.md).
It measures two AutoNAT-only swarms together, not per-peer production daemon memory.
No production settings, dependencies or private dependency internals are changed.

| Control | Value |
| --- | --- |
| Isolation | Fresh user/network namespace; only loopback; reject host namespace |
| Transport | Local TCP, Noise and Yamux; no public addresses or discovery |
| Runtime | Two Tokio workers; existing instrumented System allocator |
| Modes | Explicit `P2P_VPN_REVIEW_AUTONAT_MODE=probe` or `control` |
| Probe mode | Ten outbound requests and ten refusals on each side; one established connection |
| Control mode | Ten one-second idle windows; zero probes |
| Between rounds | 100 ms polling to drain completion events |
| Server policy | Default global-IP restriction refuses loopback dial-back requests |
| Test-only timing | Client probe interval one second; control/server interval one hour; server-reuse throttle zero |
| Deadlines | 25-second internal diagnostic; 30-second outer watchdog |
| Capture order | Control, probe, probe, control; fresh process each |
| Output budget | 128 KiB per capture; no heap traces or recurring full state dumps |

The client registers exactly one explicit local AutoNAT server in both modes.
The server never probes. No build overlaps a capture. Timing controls accelerate
the diagnostic only; this is not a production CPU or retry-rate comparison.

## Measurements

1. Initialize the known global futures timer before accounting.
2. Preallocate observer rows; snapshot before runtime construction.
3. Sample after connection establishment and after each of ten rounds.
4. Explicitly disconnect; require both swarms to observe no connected peer.
5. Drop both swarms; allow 100 ms for asynchronous cleanup; sample again.
6. Drop the Tokio runtime; snapshot before serializing evidence.

Record signed requested-byte/block deltas, actual elapsed time and refusal counts.
Compare first use with later rounds and compare connection/swarm/runtime release.
Do not assert an arbitrary zero baseline: process-global initialization is separate
from growing per-request ownership and must remain visible.

## Execution

Build the feature-enabled library test once with the existing offline cached tools.
Run `allocation_review::runtime::autonat::measure_autonat_refusal_ownership` alone,
with `--ignored --exact --nocapture --test-threads=1`, inside the guarded namespace.

The global allocator requires a fresh process for each mode. Calibration and the
host-namespace rejection check precede capture.

```sh
P2P_VPN_REVIEW_AUTONAT_MODE=probe \
timeout --signal=TERM --kill-after=5s 30 \
prlimit --fsize=131072:131072 -- \
unshare --user --map-root-user --net \
sh -c 'ip link set lo up && exec "$1" \
  allocation_review::runtime::autonat::measure_autonat_refusal_ownership \
  --ignored --exact --nocapture --test-threads=1' sh "$TEST_BINARY"
```

## Final Results

Values are requested live bytes above the pre-runtime snapshot, including both
swarms and runtime overhead. Both captures of each mode agree from round one
onward; the immediate connected snapshot differs as recorded below.
No build or other review workload overlapped the matrix.

| Checkpoint | Control bytes / blocks | Probe bytes / blocks | Probe minus control, bytes |
| --- | ---: | ---: | ---: |
| Connected | 123,649 / 273 | 123,649-123,769 / 273-274 | 0-120 |
| Every round, 1 through 10 | 123,459 / 269 | 132,681 / 282 | 9,222 |
| Connections closed | 73,481 / 184 | 78,525 / 190 | 5,044 |
| Swarms dropped plus 100 ms | 56,810 / 109 | 57,034 / 109 | 224 |
| Runtime dropped | 25,048 / 70 | 25,272 / 70 | 224 |

- Probe captures each observe ten requests, ten outbound refusals and ten inbound refusals.
- Controls each observe zero probes; all modes retain the original connection through round ten.
- No net byte/block growth occurs between rounds one and ten in any final capture.
- Connection retirement removes part of the differential; swarm drop removes most of the remainder.
- The 224-byte residual has no net block-count difference; its precise allocation owner remains unknown.
- The first probe capture's immediate connected snapshot has 120 extra bytes/one block; the second matches controls. Do not treat this concurrent snapshot as a settled baseline.
- No matching diagnostic process remained after capture; each log stays below the 128 KiB cap.

[Portable samples](autonat-ownership-samples.json) contain every checkpoint,
actual elapsed times, source fingerprints and the four capture log hashes.
Executable SHA-256:
`295187d82d65abb9ee739dcf108ec6b79c90c680f2adeb88d8d5c9d132ccdf1a`.

Two preliminary runs used the earlier settling loop. Their logs remain under
`/tmp/p2p-vpn-autonat-ownership-{control,probe}-1.log`; they are not matrix entries.
The final loop rejects unexpected behaviour/connection-close events while settling.

## Validation

| Gate | Result |
| --- | --- |
| Final executable allocator calibration | Pass |
| Host-namespace negative guard | Expected exit 101 before network construction |
| Feature-enabled library tests | 1,149 passed; 14 opt-in tests ignored |
| Offline locked workspace | 1,507 passed; 40 opt-in tests ignored |
| Required Clippy groups | Workspace/all targets with feature pass; advisory warnings remain |
| Formatting | Cached rustfmt check passes |
| Cached Nix source check | Evaluated script passes outside sandbox |
| Android-native build | Not run: Linux-only test module behind `cfg(test)` and `allocation-review`; no shared production change |
| Storage before builds | 9,024,980 KiB under `/tmp/p2p-vpn-*`, below 10 GiB |

Logs use `/tmp/p2p-vpn-autonat-ownership-` with suffixes `final-build.log`,
`final-calibration.log`, `final-host-guard.log`, `units.log`, `workspace.log`,
`clippy.log`, `source-check.log` and `final-{control,probe}-{1,2}.log`.

## Remaining Attribution

The isolated stack shows first-use retention rather than per-refusal growth on
one connection. It does not identify the production daemon's exact 1,460-byte step:
the complete stack, active protocols and preexisting capacities differ.

The [matched timer prewarm comparison](global-timer-allocation-review.md#matched-autonat-intervention)
now explains the 224-byte teardown difference. Next, distinguish reconnect
high-water storage from per-connection retained state in the full daemon.
