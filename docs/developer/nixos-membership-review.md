# NixOS Membership Review

## Result

On 2026-09-06, the exported `nixos-vm-membership-convergence` check passed all
18 subtests at `4bb8ff1e`. The test script took 349.04 seconds, excluding package
compilation. Nix returned success and registered the test output.

This replaces the earlier membership-VM result as current review evidence.
It does not complete the [broader acceptance map](review-verification.md).

## Execution

```bash
nix build --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link --print-out-paths \
  .#checks.x86_64-linux.nixos-vm-membership-convergence
```

| Setting | Value |
| --- | --- |
| Nodes | Three edge VMs and one relay VM |
| Source | Current test, module, and runtime at `4bb8ff1e` |
| Runtime | Nix-packaged optimized binary; no runtime override |
| Build plan | 55 derivations; cached inputs; substitutions disabled |
| Concurrency | One Nix build job; two Cargo jobs via `NIX_BUILD_CORES` |
| Unit tests | VM package skips duplicates; separate native suite passed 1,226 tests |
| Isolation | Test VLANs and explicit relay endpoints; no physical host deployment |
| Cleanup | Driver cleanup passed; all four QEMU processes exited |

No production code or test assertions were changed to obtain this result.
The build used cached dependencies without fetching additional packages.

## Verified Scenarios

| Scenario Group | Assertions |
| --- | --- |
| Native configuration | Peerless edge configs, owner-only durable state, unchanged generated config hashes after movement |
| Delegated admission | A pairs with B; B pairs with C; all three learn the network |
| Indirect connectivity | Peer inventory, kernel routes, DNS A records, system resolver integration, and IPv4 traffic |
| Restart recovery | Membership/routes restored while sources are offline; simultaneous restart reconnects the mesh |
| Network movement | Isolated VLAN uses circuit relay; cold restart recovers; returning to LAN selects a direct path |
| Retry pressure | Selected recovery/query/redial counters stay at or below fixture limits; relay logs contain no `ResourceLimitExceeded` entry |
| DNS authority | Expiry removes names; dedicated hostname records retain precedence |
| Revocation | Names and routes are withdrawn and remain withdrawn after restart |
| Re-admission | Higher epochs restore membership and preserve original/effective inviter attribution |
| Network continuity | Revoking an inviter does not cascade; resignation leaves other members connected; later pairing succeeds |

The [machine-readable result](nixos-membership-review-sample.json) lists every
subtest and duration. Durations include waits and assertions; they are not precise
route-recovery latency measurements.

## Evidence

The result records package, generated-script, test-source, module-source, lockfile,
and compressed-log hashes. Store paths identify the exact driver and output.

| Artifact | Location |
| --- | --- |
| Invocation log | `/tmp/p2p-vpn-review-membership-vm-current.log` |
| Full test log | Compressed Nix build log recorded in the JSON result |
| Registered output | `/nix/store/r2w2mc2f9sy7vyw9p12dlkcwgdq054sb-vm-test-run-p2p-vpn-nixos-vm-membership-convergence` |

The registered output is empty; detailed assertions are in its build log, not
inside an evidence bundle. Preserve the log/hash when archiving or comparing runs.

## Limits

- Explicit fixture relay endpoints make failures deterministic; this is not public IPFS discovery or NAT traversal evidence.
- The fixture disables DCUtR and AutoNAT. It does not prove hole punching or carrier/VPN transitions.
- Traffic and DNS assertions here are IPv4/A-record based; this pass does not establish IPv6 behavior.
- Retry counters do not prove bounded retained memory, sustained packet throughput, or absence of background storms.
- Pairing uses the normal approval flow but does not inject route, disk-full, or mid-write power-loss failures.
- Other exported VM checks, Android gates, and unresolved review findings remain separate acceptance work.
