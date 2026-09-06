# Review Verification Coverage

## Scope

Audited on 2026-09-06, including committed membership inventory and deadline-aware timer refresh.
This is the current acceptance map for
the [reliability review](refactor-review.md), not a production certification.
Earlier milestones remain historical evidence, not automatic proof for later changes.

## Current Evidence

| Area | Evidence | Limitation |
| --- | --- | --- |
| Workspace | Last full run: 1,193 passed, 18 opt-in tests ignored. Diagnostics run separately. | Native Linux toolchain; not an Android device run. |
| Namespace integration | Live code pairing passes after inventory sharing, in 13.29 seconds. All 11 passed at the preceding refresh-window milestone. | Controlled topology, not public NAT traversal; full suite not repeated for this read-only inventory change. |
| Static analysis | Required correctness, suspicious, and performance Clippy groups pass. | Existing non-fatal style warnings remain. |
| Formatting | Changed Rust files pass rustfmt; whitespace checks pass. | Not proof of the complete flake `fmt` target. |
| Nix source parity | `rust-test-sources` built successfully. | Verifies packaged test inclusion, not execution. |
| Nix consumer evaluation | `nixos-consumer-flake-eval` built; all 15 configuration contracts pass. | Does not build the consumer OS or execute the service. |
| Membership VM | Earlier four-node run passed 18 subtests. | Predates later ownership changes. |
| Storage repair VM | Current-at-test binary recovered automatically after permission repair. | Predates `0acbd725`; tests startup rejection, not ENOSPC or power loss. |
| Android | Emulator lifecycle, always-on, and underlay recovery passed earlier. | JNI must be rebuilt and relevant scenarios repeated after shared runtime changes. |
| Resources | Two controlled idle samples per compared revision. | Small static topology; see [measurement limits](idle-resource-comparison.md). |
| Inventory evaluation | [Joint/separate diagnostic](inventory-evaluation-measurement.md) passed at 8, 32, and 128 records. | Single unoptimized run; not daemon throughput or memory evidence. |
| Retained membership | [Forwarder comparison](forwarder-resource-comparison.md): 12 fresh-process samples at 8, 128, and 256 records. | Larger current samples show higher RSS growth; not exact map allocation cost or release-profile CPU evidence. |
| Refresh window | Full-evaluation equivalence across time boundaries; pending notifications and failed updates covered. 256-record follow-up passed. | Debug follow-up measures two skipped refreshes, not a faster ledger evaluator or production CPU. |
| Live inventory | Regression reproduced `Active` after committed expiry; list/snapshot now share committed membership and audit time. | Native runtime coverage; rebuilt Android device validation remains outstanding. |

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
| `namespace-smoke-preflighted` | All 11 passed at refresh-window milestone; live code pairing repeated after inventory sharing. Derivation result not established. |
| `android`, `android-e2e-fixture` | Earlier offline native/Gradle validation; current derivation results not established. |
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

## Outstanding Acceptance Work

| Workstream | Required Next Evidence |
| --- | --- |
| Shared authority | Membership, TUN, DNS, and inventory share committed evaluation. Review the remaining membership-sync fallback's independent wall-clock evaluation. |
| Recovery ownership | Finish timer/event and stale-completion review beyond the extracted targeted-query owner. |
| Session lifecycle | Review remaining in-flight requests and authorization-driven retirement. |
| Pairing orchestration | Assess transaction ownership across preparation, persistence, and finalization. |
| Address resources | Identify admission is bounded. [Internal growth is reproduced](kademlia-retention-review.md) in both DHT modes; enforcement and query-memory measurements remain. |
| Resource comparison | Signed-ledger RSS sampled through 256 records; timer refresh reuses valid evaluations. Isolate retained allocations and establish release-profile/daemon impact. |
| Platform validation | Repeat affected Android and VM gates on final shared-runtime code. |
| Packaging/tooling | Resolve or explicitly account for unverified exported checks without uncontrolled source builds. |
| Documentation | Reconcile architecture and user workflows with final behavior and evidence. |

No confirmed correctness or security issue is deferred by this table. Any proposed
deferral still requires user agreement. A missing result is not a failure, but it
cannot support a completion claim.
