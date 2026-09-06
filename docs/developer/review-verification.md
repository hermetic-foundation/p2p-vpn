# Review Verification Coverage

## Scope

Audited on 2026-09-06, including Android network workflow at `6dbb680c`.
This is the current acceptance map for
the [reliability review](refactor-review.md), not a production certification.
Earlier milestones remain historical evidence, not automatic proof for later changes.

## Current Evidence

| Area | Evidence | Limitation |
| --- | --- | --- |
| Workspace | Last full run: 1,216 passed, 18 opt-in tests ignored. Diagnostics run separately. | Native Linux toolchain; not an Android device run. |
| Namespace integration | All 11 pass at `6dbb680c`, in 213.88 seconds, including network move and relay-to-direct promotion. | Controlled topology, not public NAT traversal; elapsed time is not a performance benchmark. |
| Static analysis | Required correctness, suspicious, and performance Clippy groups pass. | Existing non-fatal style warnings remain. |
| Formatting | Changed Rust files pass rustfmt; whitespace checks pass. | Not proof of the complete flake `fmt` target. |
| Nix source parity | `rust-test-sources` built successfully. | Verifies packaged test inclusion, not execution. |
| Nix consumer evaluation | `nixos-consumer-flake-eval` built; all 15 configuration contracts pass. | Does not build the consumer OS or execute the service. |
| Membership VM | Earlier four-node run passed 18 subtests. | Predates later ownership changes. |
| Storage repair VM | Current-at-test binary recovered automatically after permission repair. | Predates `0acbd725`; tests startup rejection, not ENOSPC or power loss. |
| Android | [24 network-workflow steps](android-network-workflow-review.md) pass at `6dbb680c`. The latest [multi-network rerun](android-multi-network-review.md) passes 32 steps then receives 4/5 reverse IPv4 replies after APK replacement. | OS underlays were available in this run; cause of loss remains unresolved. Neither the earlier cellular failure nor the historical 68-step pass closes this gate. |
| Resources | Two controlled idle samples per compared revision. | Small static topology; see [measurement limits](idle-resource-comparison.md). |
| Inventory evaluation | [Joint/separate diagnostic](inventory-evaluation-measurement.md) passed at 8, 32, and 128 records. | Single unoptimized run; not daemon throughput or memory evidence. |
| Retained membership | [Forwarder comparison](forwarder-resource-comparison.md): 12 fresh-process samples at 8, 128, and 256 records. | Larger current samples show higher RSS growth; not exact map allocation cost or release-profile CPU evidence. |
| Refresh window | Full-evaluation equivalence across time boundaries; pending notifications and failed updates covered. 256-record follow-up passed. | Debug follow-up measures two skipped refreshes, not a faster ledger evaluator or production CPU. |
| Live inventory | Expiry regression and current emulator lifecycle pass; lists/snapshots share committed membership and audit time. | The emulator run does not simulate hostile replies or clock rollback; those have separate unit/review coverage. |
| Membership-page authority | Remote-expiry regression reproduced and fixed; local-expiry recovery and existing resignation/revocation tests pass. | Page authority only; not a new packet or mutation exception. |
| Membership response dispatch | Wrong-type response leak reproduced and fixed; owner cleanup, retry boundary, and newer-request isolation pass. | Injected usable-connection event, not live hostile-peer transport ordering. |
| Membership sync retirement | Revoked first/final/restart replies stop; expiry and static-peer removal release pending owners; local recovery is preserved. | Application ownership, not transport-request cancellation. |
| Stale sync responses | Two loopback TCP connections; real completed response followed by controlled retirement reproduces the owner leak and verifies cleanup/newer-ID isolation. | Controlled application ordering, not physical WAN race-frequency evidence. |
| Sync history | Completion and retry maps each cap at 1,024 peers; overflow preserves backoff; authorization pruning and disconnect retention pass. | Entry-count/deadline evidence, not RSS or live overload measurements. |
| Pairing cancellation | Both roles preserve completion and persisted state across late cancellation and restore; all 48 session tests pass. | Preventive fix, not repair of existing invalid snapshots; [transaction review](pairing-transaction-review.md) remains open. |
| Acceptance retry | Four response-dispatch cases cover Submit/Poll persistence and route failures; bounded retry eligibility is restored. | Injected local errors, not partial rollback, restart recovery, or physical retry delivery. |
| Live join expiry | Prepared remote approval survives expiry/checkpoint/restore; unprepared expiry still clears it. All 50 session tests pass. | Session-state evidence; cancellation and replacement can still invalidate recovery. |
| Historical enrollments | Both roles retain acknowledgement and native artifacts after replacement/restore; RPC status shares session-owned readiness. | Session/RPC evidence, not physical sequential pairing or incompatible-authority startup compaction. |
| Startup ownership | Linux and Android pass installed snapshots; expiry deletion and metadata guards pass. Android reactivation asserts its reserved snapshot. | External library integrations must opt into the additive snapshot handoff to avoid config-derived compatibility behavior. |

## Exported Flake Checks

Inventory source:

```bash
nix eval --offline --json .#checks.x86_64-linux --apply builtins.attrNames
```

The 33 exported names below include aliases. A passing manual equivalent does
not imply that the corresponding Nix derivation was built successfully.

| Exported Check Names | Current Review Evidence |
| --- | --- |
| `rust-test-sources` | Built at the latest runtime milestone. |
| `clippy`, `fmt` | Local checks as described above; full derivation results not established. |
| `package` | Offline workspace build/tests pass; complete package check not established at this revision. |
| `releaseArchive`, `releaseArchiveSanity` | Current archive and sanity outputs not verified. |
| `nixos-consumer-flake-eval` | Built offline; evaluates minimal upstream-module consumer contracts without realizing the OS closure. |
| `nixos-module`, `nixos-consumer-flake` | Current full derivation results not verified. |
| `nixos-vm-smoke` | Current result not verified. |
| `nixos-vm-minimal-lan`, `nixos-vm-mesh` | Aliases of the same VM derivation; current result not verified. |
| `nixos-vm-module-lifecycle` | Generated script compiles; focused cached-VM storage test passes. Full target unrun. |
| `nixos-vm-membership-convergence` | Historical 18-subtest result; current rerun outstanding. |
| `nixos-vm-pairing`, `nixos-vm-code-pairing-lan`, `nixos-vm-code-pairing-relay` | Namespace pairing passes; separate current VM outputs not verified. |
| `nixos-vm-quic-datagram`, `nixos-vm-quic-stream` | Current VM outputs not verified. |
| `nixos-vm-forced-relay`, `nixos-vm-network-move` | Namespace equivalents pass; current VM outputs not verified. |
| `namespace-smoke-preflighted` | All 11 pass at `6dbb680c`. Derivation result not established. |
| `android`, `android-e2e-fixture` | Current offline x86_64 JNI/Gradle validation; full Nix derivation results not established. |
| `android-e2e-structure` | Evaluated check body passed with installed tools earlier, outside a Nix sandbox. |
| `android-device-audit-structure`, `debug-bundle-structure` | Current result not verified. |
| `public-relay-repro-structure`, `public-vpn-capture-structure` | Current result not verified. |
| `public-vpn-repro-structure`, `public-vpn-repro-evidence-structure` | Current result not verified. |
| `public-vpn-evidence-check`, `public-vpn-move-evidence-check` | Synthetic verifier fixtures; current result not verified. Not live WAN tests. |

An offline, substitution-disabled dry run for the two public-VPN verifier checks
planned 967 derivations, including ShellCheck's source dependency chain. No build
was started. This is not an estimate of work required with available binary substitutes.

### Lightweight Consumer Check

```bash
nix build --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link .#checks.x86_64-linux.nixos-consumer-flake-eval
```

- Checks generated defaults, identity/state paths, service arguments, and minimal consumer source.
- Deliberate wrong-network, embedded-secret, and consumer-mechanics mutations were rejected.
- The check planned and built one derivation using cached dependencies, with no downloads.

The existing full consumer check remains unchanged and requires the NixOS system output.
A combined offline, substitution-disabled dry run for it and `nixos-module` planned
595 derivations. Those full checks were not built during this verification pass.

## Android Startup Artifact

Historical build at `f026342f`, using cached Nix Rust/NDK tools and offline Gradle.
The build pass started no device. The subsequent [multi-network run](android-multi-network-review.md)
used this APK in one clean emulator and verified complete cleanup.

The newer [network-workflow report](android-network-workflow-review.md) records
current JNI/APK/fixture hashes and its separate scenario evidence at `6dbb680c`.

| Artifact | SHA-256 |
| --- | --- |
| Native x86_64 library / merged JNI input | `15861fce4de629b81fd5fb3462f121f630b9953afdc7362f8d649b1e0830a395` |
| Stripped library / library extracted from APK | `b152541c64d3067165b45ad4df46056cb2d3c10245fc9e7bd7218ce648143cb5` |
| `android/app/build/outputs/apk/debug/app-debug.apk` | `b5b01fce037a99d1272437e862be675b5f0c195048fc52b92e820a99f50c18c6` |

Matching hashes confirm the APK contains this rebuilt JNI output. Gradle reported
78 tasks: five executed and 73 up-to-date. This is build evidence, not device behavior.
Logs use `/tmp/p2p-vpn-review-startup-snapshot-*`.

## Outstanding Acceptance Work

| Workstream | Required Next Evidence |
| --- | --- |
| Recovery ownership | Finish timer/event and stale-completion review beyond the extracted targeted-query owner. |
| Session lifecycle | [Membership-sync review](membership-sync-review.md) cases are fixed. Reconcile broader session-lifecycle review and final platform evidence. |
| Android lifecycle ownership | [Three event-ownership findings](android-event-ownership-review.md) are fixed with JVM and emulator coverage. Health-poll native-failure recovery still needs platform coverage. |
| Pairing orchestration | Assess transaction ownership across preparation, persistence, and finalization. |
| Address resources | Identify admission is bounded. [Internal growth is reproduced](kademlia-retention-review.md) in both DHT modes; enforcement and query-memory measurements remain. |
| Resource comparison | Signed-ledger RSS sampled through 256 records; timer refresh reuses valid evaluations. Isolate retained allocations and establish release-profile/daemon impact. |
| Platform validation | Repeat affected Android and VM gates on final shared-runtime code. |
| Android underlay failure | Capture OS cellular availability/validation independently of the app tracker, then resolve and rerun the failed multi-network transition. |
| Android update traffic | Investigate the retained 4/5 reverse IPv4 result after APK replacement, including packet timing and path changes. |
| Packet stream ownership | [Selected TCP dispatch](packet-stream-ownership-review.md) is corrected with exact-handler regression coverage. Pinned closure outcomes and rebuilt Android validation remain open; Android loss causality is unproven. |
| Packaging/tooling | Resolve or explicitly account for unverified exported checks without uncontrolled source builds. |
| Documentation | Reconcile architecture and user workflows with final behavior and evidence. |

No confirmed correctness or security issue is deferred by this table. Any proposed
deferral still requires user agreement. A missing result is not a failure, but it
cannot support a completion claim.
