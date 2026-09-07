# NixOS Workflow Review

## Results

Six exported checks passed at `cf18fb51` on 2026-09-06. These results close
specific gaps in the [acceptance map](review-verification.md), not the broader
review goal. No runtime code or test assertions changed for these runs.

| Check | Result | Scope |
| --- | --- | --- |
| `nixos-vm-module-lifecycle` | 12 subtests; 45.16 s | Two independent instances, identity persistence, DNS, crash and storage-permission recovery |
| `nixos-vm-smoke` | Script passed; 17.96 s | Module service, status, Prometheus output, readiness, clean stop |
| `nixos-vm-minimal-lan` | 4 subtests; 27.33 s | Minimal generated configs, healthy discovery, bidirectional IPv4 traffic, direct path |
| `nixos-vm-pairing` | 8 subtests; 48.79 s | One-time URI pairing, native Nix generation/evaluation, replay rejection, traffic and inviter restart |
| `public-vpn-evidence-check` | Passed | Synthetic positive fixtures; rejects missing relay/QUIC-stream evidence and mismatched configuration |
| `public-vpn-move-evidence-check` | Passed | Synthetic LAN/remote/LAN reports; rejects missing remote provenance and changed configuration |

`nixos-vm-mesh` aliases `nixos-vm-minimal-lan`; it is not an additional run.
Durations are whole VM test scripts, excluding builds, not recovery latency.

## Reproduction

```bash
nix build --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link --print-out-paths \
  .#checks.x86_64-linux.nixos-vm-module-lifecycle \
  .#checks.x86_64-linux.nixos-vm-smoke \
  .#checks.x86_64-linux.nixos-vm-minimal-lan \
  .#checks.x86_64-linux.nixos-vm-pairing \
  .#checks.x86_64-linux.public-vpn-evidence-check \
  .#checks.x86_64-linux.public-vpn-move-evidence-check
```

- Actual execution used three sequential invocations: lifecycle, the other VMs, then verifiers.
- Each VM scenario ran alone. All drivers completed cleanup; no QEMU process remained.
- The optimized VM runtime was reused from the membership check; its hash is recorded below.
- No physical machines, Android devices, or production services were accessed.

## Coverage Boundaries

### Lifecycle

- Storage recovery injects unsafe membership-file permissions, then repairs permissions only.
- Automatic restart restores service and DNS without changing identity or membership bytes.
- DNS includes A/PTR, UDP/TCP, unknown-suffix rejection, resolver restart, and guard cleanup.
- This is not ENOSPC, interrupted-write, power-loss, OS-reboot, or multi-host IPv6 evidence.

### Pairing

- The joiner exports Nix with `--nixos-only` and reuses its module-created private key.
- The test evaluates that Nix through the upstream module and validates generated defaults.
- It then runs the evaluated configuration as private temporary JSON under `systemd-run`.
- It does **not** rebuild/switch the generated Nix configuration. That activation workflow remains unverified here.
- This is URI pairing, not code-based PAKE pairing; fixture transport/discovery overrides remain in use.

The later [code-pairing activation check](nixos-pairing-activation-review.md)
builds and switches both guest systems using their actual generated Nix imports.
That stronger evidence is separate from this historical URI-pairing run.

### Network Evidence

- Minimal LAN traffic is five IPv4 pings in each direction, not sustained-load evidence.
- The verifier fixtures are fabricated reports, not actual public IPFS discovery or NAT traversal.
- Passing verifiers does not establish that every false or inconsistent report is rejected.
- Shared-runtime Android validation, public transitions, and outstanding review findings remain separate.

## Build Resources

Missing BIND and ShellCheck outputs were fetched from the official Nix binary
cache with serial `curl --limit-rate 600k` transfers. Original cache signatures
and NAR hashes were checked by Nix during import; signature checking stayed enabled.

| Constraint | Observed Handling |
| --- | --- |
| Network | Three compressed archives totaling 2,177,089 bytes; metadata additional |
| Rate | 600 KiB/s per archive; one transfer at a time, below 10 Mbps |
| Build | Offline, substitutions disabled; one Nix job and two cores |
| Missing inputs | After imports: 56 lifecycle, 112 combined basic-VM, and 4 verifier derivations |
| Retained task artifacts | About 3.74 GiB across review, Android target, and app build directories |

Client `download-speed` settings were rejected by the untrusted Nix daemon.
They did not enforce the transfer cap; the serial curl downloads did.
No global Nix settings or trusted keys were changed.

The combined verifier/debug-bundle dry run still planned 96 derivations,
including LLVM/toolchain source builds. That build was not started; the
verifiers were isolated into their four-derivation build instead.

## Exact Evidence

The [machine-readable record](nixos-workflow-review-sample.json) identifies
source, runtime, successful output paths, and compressed build-log hashes.
Registered check outputs are empty markers, not retained evidence bundles.

| Invocation | Log |
| --- | --- |
| Lifecycle | `/tmp/p2p-vpn-review-module-lifecycle-current.log` |
| Smoke, LAN, pairing | `/tmp/p2p-vpn-review-basic-vms-current.log` |
| Evidence verifiers | `/tmp/p2p-vpn-review-verifiers-current.log` |

Preserve compressed Nix build logs when archiving results. Store paths and
hashes identify this run but do not guarantee local artifacts survive garbage collection.
