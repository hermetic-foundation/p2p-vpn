# NixOS Pairing Activation Review

## Result

The strengthened `nixos-vm-code-pairing-lan` check passed all eight subtests.
Both guests built and switched systems importing their actual pairing-generated
Nix through the upstream module. No runtime or module fix was needed.

| Assertion | Result |
| --- | --- |
| Peerless startup, approval, live enrollment | Passed |
| Enrollment survives daemon restart | Passed |
| Native artifacts exclude secrets and merge module settings | Passed |
| Acknowledgement compacts retained artifacts | Passed |
| Generated Nix activation | Both `/run/current-system` paths changed |
| Automatic service restart | Both service invocation IDs changed |
| Identity preservation | Private-key file hashes unchanged |
| Active module settings | MTU 1360, TCP listener 4555, two signed records, no explicit peers |
| Post-switch health | Validated peers and supported paths on both nodes |
| Post-switch traffic | Bidirectional IPv4 ping commands succeeded |

## Reproduce

```bash
nix build -L --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link --print-out-paths \
  .#checks.x86_64-linux.nixos-vm-code-pairing-lan
```

- Source: `tests/nixos/code-pairing-lan.nix`, modified on parent `8b7ed7c9`.
- Test source SHA-256: `5fe29b0753bd00666e7062d646bb1fa574cad564af69f7426e8b0e5d79678e27`.
- Script duration: 306.09 seconds; activation subtest: 258.00 seconds.
- Each guest built 22 derivations; activation runs sequentially.
- Cleanup completed; no QEMU or test-driver processes remained.

### Artifact Identity

| Artifact | Store Path |
| --- | --- |
| Successful check | `/nix/store/j59jz5bhcypgvp835x503si8yd9ybvjy-vm-test-run-p2p-vpn-nixos-vm-code-pairing-lan` |
| Check derivation | `/nix/store/m5ybx6x4mmn0injrfglj5bw9pdp6zngg-vm-test-run-p2p-vpn-nixos-vm-code-pairing-lan.drv` |
| VM runtime | `/nix/store/2qwiacy16vvidvv6pgx9jsrdgvq63zq8-p2p-vpn-0.1.0` |

- Runtime CLI SHA-256: `2e082552eea83b1bf1a71b990f32f1a2d2220e30c11351a7793f25063e69ad4e`.
- Invocation log: `/tmp/p2p-vpn-review-pairing-switch-direct-boot.log`.
- Compressed log: `/nix/var/log/nix/drvs/m5/ybx6x4mmn0injrfglj5bw9pdp6zngg-vm-test-run-p2p-vpn-nixos-vm-code-pairing-lan.drv.bz2`.
- Compressed-log SHA-256: `cd1343d62a80a0dc469ec64152060268a552580ce28cdcbc42d9439742b3d40c`.

## Fixture Design

1. Re-evaluate the original VM definition with the real generated Nix import.
2. Instantiate once, inspect its build plan, then realize that same derivation.
3. Run `nixos-rebuild switch --no-reexec --store-path` on the built system.
4. Inspect the upstream service, generated runtime configuration, identity, and traffic.

| Resource Control | Implementation |
| --- | --- |
| Concurrency | One host build job; sequential guest builds with one job and two cores |
| Guest limits | Two guests, each with 3 GiB RAM and two virtual cores |
| Network | Offline host build; guest substitutes disabled, isolated VLAN |
| Build expansion | Refuse plans exceeding 128 derivations before realization |
| Dependencies | Explicit build-only outputs, not the entire system derivation closure |

The plan guard parses the pinned Nix diagnostic format. It bounds derivation
count, not bytes or compilation memory. Dependency changes may require adjusting
the explicit fixture inputs; do not bypass the guard to build a bootstrap chain.

### Diagnostic Attempts

| Attempt | Finding and Correction |
| --- | --- |
| Full build dependencies | Planned 3,109 derivations; not started |
| Initial selective closure | Guest lacked build-only tools; added explicit outputs |
| Guarded selective closure | Refused 484 derivations before building; added cached libcap verifier |
| First realized system | Rebuild re-exec attempted channel lookup; use explicit store path and no re-exec |
| Switch with default GRUB | Direct-boot VM disk lacks embedding area; disable GRUB in fixture |
| Final direct-boot run | All eight subtests passed |

One earlier run was terminated during an external host Nix daemon restart.
It was not counted as an assertion failure or as successful verification.

## Limits

- Agenix-style paths contain root-owned fixture files; age decryption is not exercised.
- Pairing output is imported as Nix, but this is not a consumer `--flake` or remote `--target-host` deployment.
- The system switches without reboot; direct kernel boot does not test a bootloader installation.
- Ping checks require successful commands, not five replies or zero loss; this is not sustained-load evidence.
- Controlled LAN and fixture settings do not establish public discovery, carrier NAT, or VPN traversal.
- No production services or physical devices were changed for this check.
- Rust and Android suites were not rerun for this test-only change; their separate results remain in the acceptance map.

See the [acceptance map](review-verification.md) for unresolved findings and
the separate full consumer-system packaging gap.
