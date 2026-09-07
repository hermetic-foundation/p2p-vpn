# Review Verification Coverage

## Scope

Audited through 2026-09-07, including NixOS lifecycle/LAN/pairing checks at
`cf18fb51`, membership convergence at `4bb8ff1e`, and Android failures at
`fe950142` (isolation), `bac49191` (reboot recovery), and `23e5159d`
(process recovery), followed by the 68-check Android pass at `deedd041`.

Transport checks at `859b29d5` and the subsequent movement-test repair are
recorded in the [transport report](nixos-transport-review.md).

This is the current acceptance map for
the [reliability review](refactor-review.md), not a production certification.
Earlier milestones remain historical evidence, not automatic proof for later changes.

Closeout Goal 1 is complete: probe ownership at `8e5950fa` and durable pairing
cancellation at `8ed95627`. The remaining bounded workstreams below are still
required for the broader review.

## Current Evidence

| Area | Evidence | Limitation |
| --- | --- | --- |
| Workspace | Latest Goal 1 run: 1,247 passed, 23 opt-in tests ignored. [Durable cancellation evidence](pairing-cancellation-plan.md#final-verification) includes cleanup/retry, five mutation cases, expired replacement, and built-in-address protection. | Native Linux toolchain; not an Android device run. |
| Namespace integration | Goal 1 reruns pass peerless code pairing, direct/relay pairing acceptance, UDP, and QUIC datagrams. The historical all-12 pass remains in `/tmp/p2p-vpn-review-queue-pressure-namespace-suite.log`. | Controlled topology; not public NAT or final all-platform acceptance. Historical pressure/isolation limits remain in their linked reports. |
| Static analysis | Required correctness, suspicious, and performance Clippy groups pass. | Existing non-fatal style warnings remain. |
| Formatting | Changed Rust files pass rustfmt; whitespace checks pass. | Not proof of the complete flake `fmt` target. |
| Nix source parity | The unchanged `rust-test-sources` script passes in a sandbox with cached tool inputs; details in the [cancellation report](pairing-cancellation-plan.md#nix-check-tooling). | Default tool-closure build failed fetching a dependency. This verifies source inclusion, not a full package build. |
| Nix consumer evaluation | `nixos-consumer-flake-eval` built; all 15 configuration contracts pass. | Does not build the consumer OS or execute the service. |
| Membership VM | [Exported four-node check built at `4bb8ff1e`](nixos-membership-review.md): all 18 subtests pass in 349.04 seconds. Current packaged runtime and NixOS module; no runtime override. | Controlled VLAN/relay topology; IPv4/A-record assertions. Not public NAT, IPv6, sustained-load, or other VM-gate evidence. |
| NixOS workflows | [Four exported VM checks pass at `cf18fb51`](nixos-workflow-review.md): lifecycle, smoke, minimal LAN, and URI pairing. | Pairing evaluates generated Nix but runs its resulting JSON, not a rebuild/switch. Controlled IPv4 LAN, not public WAN or sustained load. |
| Code-pairing Nix activation | [Strengthened LAN check](nixos-pairing-activation-review.md): all eight subtests pass, including actual system builds/switches on both guests, automatic service restarts, unchanged identities, and restored traffic. | Fixture secrets, not age decryption; local store-path switch, not consumer-flake remote deployment, reboot, or zero-loss proof. |
| Storage repair VM | Full lifecycle check passes at `cf18fb51`; automatic service/DNS recovery after permission repair preserves identity and membership bytes. | Permission failure, not ENOSPC, interrupted writes, power loss, or OS reboot. |
| Transport VMs | QUIC-stream, QUIC-datagram, and forced-relay checks pass at `859b29d5`. Strengthened movement test passes twice with selected relay payloads, unchanged invocation IDs/configs, and direct LAN return. | Fixed an invalid exact-one relay-path assertion after a two-path failure. Corrected runs had one path; controlled IPv4 fixtures, not public NAT or saturation evidence. |
| Android | [Latest multi-network run](android-multi-network-review.md#latest-attempt) at `deedd041`: all 68 checks passed, including process/reboot restoration and sibling failure isolation. The admission fix also has a failing/passing local regression; [native health recovery](android-event-ownership-review.md#native-health-recovery-instrumentation) passes at `4b90f3bc`. | Controlled emulator, not physical carrier/VPN evidence. Readiness retries occurred; earlier packet-loss attribution and sustained overload remain open. |
| Resources | Historical debug comparison plus two current release-profile idle captures at `89709e4f`: 0.133-0.167% of one core per node. | Small static topology; current-only release results are not a release baseline comparison. Connection and drop increments remain visible in [measurement limits](idle-resource-comparison.md#release-profile-follow-up). |
| Inventory evaluation | [Joint/separate diagnostic](inventory-evaluation-measurement.md) passed at 8, 32, and 128 records. | Single unoptimized run; not daemon throughput or memory evidence. |
| Retained membership | [Forwarder comparison](forwarder-resource-comparison.md): original debug samples plus 12 release-profile samples comparing `f24831fa` and `89709e4f` at 8, 128, and 256 records. | No memory reduction established; RSS is not exact map allocation cost or whole-daemon footprint. |
| Refresh window | Full-evaluation equivalence across time boundaries; pending notifications and failed updates covered. Release 256-record refreshes take 40-42 ms versus baseline 123-126 ms. | Three calls, two skipped evaluations; not faster signature verification, sustained-load latency, or whole-daemon CPU. |
| Live inventory | Expiry regression and current emulator lifecycle pass; lists/snapshots share committed membership and audit time. | The emulator run does not simulate hostile replies or clock rollback; those have separate unit/review coverage. |
| Membership-page authority | Remote-expiry regression reproduced and fixed; local-expiry recovery and existing resignation/revocation tests pass. | Page authority only; not a new packet or mutation exception. |
| Membership response dispatch | Wrong-type response leak reproduced and fixed; owner cleanup, retry boundary, and newer-request isolation pass. | Injected usable-connection event, not live hostile-peer transport ordering. |
| Membership sync retirement | Revoked first/final/restart replies stop; expiry and static-peer removal release pending owners; local recovery is preserved. | Application ownership, not transport-request cancellation. |
| Stale sync responses | Two loopback TCP connections; real completed response followed by controlled retirement reproduces the owner leak and verifies cleanup/newer-ID isolation. | Controlled application ordering, not physical WAN race-frequency evidence. |
| Sync history | Completion and retry maps each cap at 1,024 peers; overflow preserves backoff; authorization pruning and disconnect retention pass. | Entry-count/deadline evidence, not RSS or live overload measurements. |
| Pairing cancellation | Both roles preserve established completion; pending cancellation persists abort intent, cleans up, and compacts safely. RPC failure/retry and startup cleanup pass. | Local cancellation, not remote revocation; inconsistent legacy snapshots are not automatically repaired. |
| Acceptance retry | Four live TCP loopback cases cover Submit/Poll persistence and route failures; the normal retry driver delivers acceptance through Applied persistence and reload. Removing the ownership release makes retry time out. | Signed fixture responder and injected route controller; not full inviter approval, kernel rollback, connection promotion, or physical WAN delivery. |
| Completed pairing status | [Relay discovery loss reproduced and fixed](pairing-transaction-review.md#completed-discovery-status). Both role-specific regressions, full workspace, and rebuilt LAN/relay code-pairing VMs pass. | Status metadata only; historical records without operation history may lack discovery data. Not a change to routing or admission. |
| Partial route retry | Successful and failed rollback after partial application preserve logical state; repaired runtime commit replays the full update and authorizes the peer. | Injected command results, not kernel state, durable finalization, or automatic retry delivery. |
| Persisted restart repair | Both roles checkpoint cleanup ownership before replay, preserve Prepared state after partial route/rollback failure, reload, and complete reconciliation. Aborting entries never replay as enrollment. | Real state store and startup function with injected route failures; isolated kernel deletion is tested separately. Not OS power-loss evidence. |
| Live join expiry | Prepared remote approval survives expiry/checkpoint/restore; unprepared expiry still clears it. Expired Prepared entries require cancellation before replacement. | Completed-operation replacement remains supported; broader lifecycle review is separate. |
| Prepared mutations | [Five original cases](pairing-transaction-review.md#current-mutation-regression) now require durable abort, reload, and safe compaction without replacing the newer operation. | Historical negative-control evidence retained; no repair claim for old inconsistent snapshots. |
| Historical enrollments | Both roles retain acknowledgement and native artifacts after replacement/restore; RPC status shares session-owned readiness. [Inviter startup compaction](pairing-transaction-review.md#historical-startup-compaction) also preserves a replacement operation under incompatible declarative authority. | Real state store and startup function with injected routes; not joiner startup, multiple completed sequential enrollments, or physical deployment evidence. |
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
| `package` | [Release package built at `c9ccbe82`](package-tooling-review.md): 1,151 tests pass, 19 opt-in ignored; installed completion verified. Separate from the workspace suite. |
| `releaseArchive`, `releaseArchiveSanity` | [Both built at `c9ccbe82`](package-tooling-review.md); archive content/path checks and extracted CLI help pass. Not non-Nix portability proof. |
| `nixos-consumer-flake-eval` | Built offline; evaluates minimal upstream-module consumer contracts without realizing the OS closure. |
| `nixos-module`, `nixos-consumer-flake` | Current full derivation results not verified. |
| `nixos-vm-smoke` | Built at `cf18fb51`; module readiness, status/metrics, and clean stop pass. [Workflow evidence](nixos-workflow-review.md). |
| `nixos-vm-minimal-lan`, `nixos-vm-mesh` | Same derivation built at `cf18fb51`; all 4 subtests pass. Minimal config and bidirectional IPv4 LAN traffic. |
| `nixos-vm-module-lifecycle` | Built at `cf18fb51`; all 12 subtests pass, including storage-permission recovery and independent instance/DNS lifecycle. |
| `nixos-vm-membership-convergence` | Built offline at `4bb8ff1e`; all 18 subtests pass. [Exact artifacts and limits](nixos-membership-review.md). |
| `nixos-vm-pairing` | Built at `cf18fb51`; all 8 subtests pass. Native Nix generation/evaluation, not rebuild/switch activation. |
| `nixos-vm-code-pairing-lan` | [Strengthened check built](nixos-pairing-activation-review.md): eight subtests pass, including real generated-Nix rebuild/switch on both guests. |
| `nixos-vm-code-pairing-relay` | Built with the completed-discovery fix: five subtests pass. [Artifacts and limits](pairing-transaction-review.md#completed-discovery-status). |
| `nixos-vm-quic-datagram`, `nixos-vm-quic-stream` | Built at `859b29d5`; each passes 2 subtests. Datagram preference and stream-only traffic confirmed by path/session/payload counters. |
| `nixos-vm-forced-relay` | Built at `859b29d5`; all 5 subtests pass. Controlled VLANs and explicit fixture relay. |
| `nixos-vm-network-move` | Original pass followed by an exact-one-count failure. Corrected, stronger test passes twice. [Logs, hashes, and limits](nixos-transport-review.md). |
| `namespace-smoke-preflighted` | Manual full namespace suite: all 12 pass, including TCP queue pressure. This exported derivation runs only direct-overlay smoke; its build result is not established. |
| `android`, `android-e2e-fixture` | Current offline x86_64 JNI/Gradle validation; full Nix derivation results not established. |
| `android-e2e-structure` | Evaluated check body passed with installed tools earlier, outside a Nix sandbox. |
| `android-device-audit-structure` | Full evaluated check body passed with cached tools outside a Nix sandbox; mock-device evidence only. See tooling follow-up below. |
| `debug-bundle-structure` | New process-argument privacy regression passed from working-tree and evaluated Nix source; full wrapper/derivation result remains unverified. |
| `public-relay-repro-structure`, `public-vpn-capture-structure` | [Both built at `c9ccbe82`](package-tooling-review.md); script structure and synthetic capture evidence pass, not public-network behavior. |
| `public-vpn-repro-structure`, `public-vpn-repro-evidence-structure` | [Both built at `c9ccbe82`](package-tooling-review.md); generated scripts and synthetic evidence mapping pass. |
| `public-vpn-evidence-check`, `public-vpn-move-evidence-check` | Both built at `cf18fb51`; positive and rejection fixtures pass. [Exact results](nixos-workflow-review.md). Synthetic reports, not live WAN tests. |

An earlier offline verifier dry run planned 967 derivations. Importing the exact
signed ShellCheck binary-cache output reduced this to four derivations; both
checks then built offline. [Transfer limits and evidence](nixos-workflow-review.md#build-resources).

### Tooling Follow-Up

On 2026-09-06, the full evaluated Android device-audit check body passed at
`336b90cd`, using cached ShellCheck and the locked nixpkgs shebang hook. The
driver supplied the matching `isScript` helper; execution was outside a Nix sandbox.

| Mock Scenario | Result |
| --- | --- |
| Preflight and wrong ABI | Valid fixture accepted; wrong ABI rejected before install |
| Unattended destructive setup | Rejected without installing |
| Full, core, and upstream-VPN audits | Expected transitions and evidence assertions pass |
| Injected Doze failure | Expected failed outcome; Doze released, screen awake, private state removed, profile preserved |
| Evidence eligibility | All short fake-device reports retain `proof_eligible=false` |

- Evaluated body: `/tmp/p2p-vpn-review-device-audit-check.sh`.
- Driver: `/tmp/p2p-vpn-review-device-audit-driver.sh`.
- Log: `/tmp/p2p-vpn-review-device-audit-run.log`.
- Terminal success marker: `/tmp/p2p-vpn-review-device-audit-passed`.
- Retained mock artifacts total approximately 200 KiB; no real ADB device was accessed.

The debug-bundle review found full process arguments could expose a pairing code.
Commit `5b5c1b87` removes arguments while retaining PID, parent PID, state, and
command name. An isolated sentinel fixture failed before the change and passes afterward.

The privacy test also passed from the Nix-evaluated source tree, and ShellCheck
and shell syntax checks passed. Bundles still contain network/host information
and optional command output; this is not a general anonymization guarantee.

The earlier debug-bundle dry run planned 980 source builds. After the ShellCheck
import, a combined debug-bundle/verifier plan still required 96 derivations,
including LLVM/toolchain sources; it was not started. The complete debug-bundle
wrapper remains unverified; its focused privacy regression is not packaging proof.

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

The [network-workflow report](android-network-workflow-review.md) records separate
scenario evidence at `6dbb680c`. The latest rebuilt APK, JNI, fixture, and CLI hashes
are in the [multi-network report](android-multi-network-review.md#current-artifacts)
at `643d798e`; the table below remains historical.

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
| Probe response ownership | [Wrong-peer probe consumption](path-probe-ownership-review.md) is corrected with a failing/passing regression, full native workspace checks, and UDP/QUIC namespace passes. Prepared pairing and the broader timer audit remain separate requirements. |
| Session lifecycle | [Membership-sync review](membership-sync-review.md) cases are fixed. Reconcile broader session-lifecycle review and final platform evidence. |
| Android lifecycle ownership | [Three event-ownership findings](android-event-ownership-review.md) have JVM/emulator coverage, including recurring JNI health polling and automatic native-failure recovery at `4b90f3bc`. Reconcile broader lifecycle evidence on final code. |
| Pairing orchestration | Goal 1 closes the identified Prepared mutation defect; broader lifecycle/platform acceptance remains separate. |
| Address resources | Identify admission is bounded. [Bucket and query retention are reproduced](kademlia-retention-review.md) in both DHT modes; the query diagnostic measures 65 addresses/3,055 encoded bytes and explicit-retirement cleanup. Enforcement, cumulative peer growth, and actual heap measurements remain. |
| Resource comparison | Debug and release signed-ledger samples cover 256 records; timer refresh reuses valid evaluations. Isolate retained allocations and establish daemon/sustained-load impact. |
| Platform validation | All exported VM scenarios have review results at the revisions listed above; Android passes all 68 multi-network checks at `deedd041`. Generated-Nix system activation now passes on both LAN guests. Consumer-flake, remote-deployment, and reboot coverage remain distinct; revalidate affected scenarios after further runtime changes. |
| Android underlay failure | Latest transition passes with independent OS underlay diagnostics. Earlier failure attribution remains unresolved; a pass alone does not establish its cause. |
| Private bootstrap admission | [Delayed changed-port restart](private-discovery-restart-review.md) failed before the Identify classification fix and passed twice afterward. Routing clients no longer expire merely because they advertise pairing support. The Android multi-network scenario also passes with the fix. |
| Android update traffic | Latest replacement traffic passes with ping timing and reply sequences retained. Preserve earlier 4/5 evidence and investigate attribution alongside remaining transport ownership work. |
| Packet stream ownership | [Stream regressions](packet-stream-ownership-review.md) include 1,500 TCP/QUIC overload/recovery cycles each and a reproduced failed-connection reply defect. [TCP namespace pressure](queue-pressure-review.md) has passing three-cycle evidence but also an unresolved intermittent recovery failure. Other transports, byte-limit saturation, heap trends, and final platform validation remain open. |
| Packaging/tooling | Resolve or explicitly account for unverified exported checks without uncontrolled source builds. |
| Documentation | Reconcile architecture and user workflows with final behavior and evidence. |

The user split this review into bounded goals. Goal 1 covers probe ownership and
Prepared pairing mutations only; it does not waive the remaining workstreams.
A missing result cannot support a completion claim for the broader review.

## Next Bounded Work

| Workstream | Scope Retained |
| --- | --- |
| Kademlia resources | Enforce reproduced address/bucket/query retention limits |
| Lifecycle review | Finish timer/event ownership and stale-completion auditing |
| Resource measurement | Establish baseline and sustained CPU/memory evidence |
| Final acceptance | Reconcile affected NixOS/Android scenarios and documentation on final code |
