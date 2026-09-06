# Review Verification Coverage

## Scope

Audited on 2026-09-06, including the public-pairing address-admission follow-up.
This is the current acceptance map for
the [reliability review](refactor-review.md), not a production certification.
Earlier milestones remain historical evidence, not automatic proof for later changes.

## Current Evidence

| Area | Evidence | Limitation |
| --- | --- | --- |
| Workspace | 1,183 enabled tests pass; 15 opt-in tests ignored. | Native Linux toolchain; not an Android device run. |
| Namespace integration | All 11 passed at `0acbd725`; pairing and DHT cases pass after the admission fix. | Other nine not repeated after that fix; controlled topology, not public NAT traversal. |
| Static analysis | Required correctness, suspicious, and performance Clippy groups pass. | Existing non-fatal style warnings remain. |
| Formatting | Changed Rust files pass rustfmt; whitespace checks pass. | Not proof of the complete flake `fmt` target. |
| Nix source parity | `rust-test-sources` built successfully. | Verifies packaged test inclusion, not execution. |
| Membership VM | Earlier four-node run passed 18 subtests. | Predates later ownership changes. |
| Storage repair VM | Current-at-test binary recovered automatically after permission repair. | Predates `0acbd725`; tests startup rejection, not ENOSPC or power loss. |
| Android | Emulator lifecycle, always-on, and underlay recovery passed earlier. | JNI must be rebuilt and relevant scenarios repeated after shared runtime changes. |
| Resources | Two controlled idle samples per compared revision. | Small static topology; see [measurement limits](idle-resource-comparison.md). |

## Exported Flake Checks

Inventory source:

```bash
nix eval --offline --json .#checks.x86_64-linux --apply builtins.attrNames
```

The 32 exported names below include aliases. A passing manual equivalent does
not imply that the corresponding Nix derivation was built successfully.

| Exported Check Names | Current Review Evidence |
| --- | --- |
| `rust-test-sources` | Built at the latest runtime milestone. |
| `clippy`, `fmt` | Local checks as described above; full derivation results not established. |
| `package` | Offline workspace build/tests pass; complete package check not established at this revision. |
| `releaseArchive`, `releaseArchiveSanity` | Current archive and sanity outputs not verified. |
| `nixos-module`, `nixos-consumer-flake` | Current full derivation results not verified. |
| `nixos-vm-smoke` | Current result not verified. |
| `nixos-vm-minimal-lan`, `nixos-vm-mesh` | Aliases of the same VM derivation; current result not verified. |
| `nixos-vm-module-lifecycle` | Generated script compiles; focused cached-VM storage test passes. Full target unrun. |
| `nixos-vm-membership-convergence` | Historical 18-subtest result; current rerun outstanding. |
| `nixos-vm-pairing`, `nixos-vm-code-pairing-lan`, `nixos-vm-code-pairing-relay` | Namespace pairing passes; separate current VM outputs not verified. |
| `nixos-vm-quic-datagram`, `nixos-vm-quic-stream` | Current VM outputs not verified. |
| `nixos-vm-forced-relay`, `nixos-vm-network-move` | Namespace equivalents pass; current VM outputs not verified. |
| `namespace-smoke-preflighted` | All 11 passed at `0acbd725`; two affected cases repeated after admission fix. Derivation result not established. |
| `android`, `android-e2e-fixture` | Earlier offline native/Gradle validation; current derivation results not established. |
| `android-e2e-structure` | Evaluated check body passed with installed tools earlier, outside a Nix sandbox. |
| `android-device-audit-structure`, `debug-bundle-structure` | Current result not verified. |
| `public-relay-repro-structure`, `public-vpn-capture-structure` | Current result not verified. |
| `public-vpn-repro-structure`, `public-vpn-repro-evidence-structure` | Current result not verified. |
| `public-vpn-evidence-check`, `public-vpn-move-evidence-check` | Synthetic verifier fixtures; current result not verified. Not live WAN tests. |

An offline, substitution-disabled dry run for the two public-VPN verifier checks
planned 967 derivations, including ShellCheck's source dependency chain. No build
was started. This is not an estimate of work required with available binary substitutes.

## Outstanding Acceptance Work

| Workstream | Required Next Evidence |
| --- | --- |
| Shared authority | Review prepared updates and TUN, DNS, and inventory consumers; preserve public constructors. |
| Recovery ownership | Finish timer/event and stale-completion review beyond the extracted targeted-query owner. |
| Session lifecycle | Review remaining in-flight requests and authorization-driven retirement. |
| Pairing orchestration | Assess transaction ownership across preparation, persistence, and finalization. |
| Address resources | Identify admission is bounded. [Internal mutation paths are mapped](kademlia-retention-review.md); real-swarm reproduction and query-memory measurements remain. |
| Resource comparison | Evaluate signed-ledger scale and affected hot paths; distinguish measured changes from inference. |
| Platform validation | Repeat affected Android and VM gates on final shared-runtime code. |
| Packaging/tooling | Resolve or explicitly account for unverified exported checks without uncontrolled source builds. |
| Documentation | Reconcile architecture and user workflows with final behavior and evidence. |

No confirmed correctness or security issue is deferred by this table. Any proposed
deferral still requires user agreement. A missing result is not a failure, but it
cannot support a completion claim.
