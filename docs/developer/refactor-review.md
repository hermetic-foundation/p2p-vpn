# Reliability Review and Refactoring

## Status

Started 2026-09-06 against `560754de`. This is an evolving review, not a completed
security audit. Source inspection is distinguished from reproduced behavior.

## Completion Criteria

| Requirement | Required Evidence | Status |
| --- | --- | --- |
| Correctness review | Findings with source references and disposition | In progress |
| Authorization consistency | Shared policy and cross-consumer regression tests | Pending |
| Runtime ownership | Cohesive state owners and testable recovery decisions | Pending |
| Operational verification | Restart, revocation, minimal LAN/relay recovery, isolation | Pending |
| Resource behavior | Comparable idle CPU, memory, connection, and retry measurements | Pending |
| Documentation | Architecture and user instructions match verified behavior | Pending |

Confirmed correctness or security findings must be fixed, or explicitly deferred
with user agreement. Refactoring alone does not satisfy these criteria.

## Initial Findings

### R1: Pairing and DNS Disagree About Name Ownership

Priority: P2. Status: reproduced and fixed; Rust validation passed.

`validate_pairing_hostname_available` checks local configuration and names in
effective membership grants. It receives no signed hostname updates and does
not inspect configured remote names.

DNS instead replaces legacy grant names with effective signed hostname records
and includes configured names. After a rename, pairing can reject a released
name or accept an occupied name, producing a DNS conflict after admission.

| Evidence | Location at Review Baseline |
| --- | --- |
| Pairing validation | `src/runtime/runner.rs:15353` |
| Existing pairing conflict tests | `src/runtime/runner.rs:22630` |
| DNS name precedence | `src/dns.rs:240` |
| Rename regression coverage for DNS | `src/dns.rs:783` |

Required regression cases:

- Renamed member: reject its current name and permit its released name.
- Configured member: reject an occupied name while it remains authorized.
- Revoked member: do not retain name ownership through static metadata.
- DNS disabled: pairing must still enforce effective name ownership.

Implemented `dns::effective_peer_names` for DNS and pairing. Both code protocol
versions, approval, and file pairing pass current signed hostname state.
Fallback labels are also reserved while their owner remains authorized.

The configured-owner test failed before the fix. All 147 pairing-filtered
library tests pass afterward, including response-generation comparisons with
DNS across rename, expiry, and revocation. Workspace validation passed 1,142
tests with 14 ignored; required Clippy groups and formatting passed. Peerless
code pairing and relayed file pairing passed isolated namespace tests.

Direct file pairing exposed R5 below. Full operational gates remain pending.

### R2: Effective Authorization Is Rebuilt Independently

Priority: architectural. Status: confirmed duplication, not a newly proven bypass.

The recent revocation correction added consistent static/signed precedence,
but consumers still independently reconstruct local eligibility and remote sets.
That leaves future policy changes dependent on coordinated edits.

| Consumer | Location at Review Baseline |
| --- | --- |
| Route compilation | `src/config.rs:119` |
| Forwarding transport set | `src/runtime/forward.rs:770` |
| Forwarding packet set | `src/runtime/forward.rs:800` |
| Packet authorization constructor | `src/runtime/packet.rs:165` |
| Runtime membership | `src/runtime/runner.rs:9288` |
| DNS eligibility | `src/dns.rs:289` |
| Inventory eligibility | `src/network_peer.rs:440` |

Introduce an evaluated authorization view with explicit local eligibility,
remote membership, and signed/static provenance. Preserve audit records without
treating their presence as packet or route authority.

### R3: Recovery State Has Distributed Ownership

Priority: architectural. Status: confirmed coupling; behavioral audit pending.

`RuntimeNetworkChangeContext` coordinates paths, connection epochs, discovery,
relay reservations, packet sessions, capabilities, probes, and in-flight packets.
The network-change handler resets these and immediately invokes redial logic.

| Evidence | Location at Review Baseline |
| --- | --- |
| Network-change state and effects | `src/runtime/runner.rs:4435` |
| Periodic discovery and redial effects | `src/runtime/runner.rs:5904` |
| Runtime size | 21,262 lines before the main test module |

Extract recovery decisions with explicit time and generation inputs. Keep
libp2p effects in the runtime adapter. Test obsolete completions, LAN-first
holdoff, relay replacement, cancellation, and quiet-state retry bounds.

### R4: Architecture Documentation Contains Stale Governance Language

Priority: documentation. Status: confirmed.

`architecture.md` describes signed records as a delegated trust graph and
`README.md` indexes membership as a trust graph. The current ledger implements
flat any-member governance with non-cascading revocation and legacy restoration.

Update these descriptions with the implementation changes and retain explicit
migration information in the membership reference.

### R5: Identify Can Disconnect a File-Pairing Probe

Priority: P1. Status: fixed; direct and relayed namespace verification passed.

`handle_identify_received` exempts active code-pairing sessions from non-relay
infrastructure rejection, but does not preserve a bounded file-pairing probe.
Identify can therefore disconnect an admitted probe before its pairing response.

| Evidence | Observation |
| --- | --- |
| Changed direct namespace run | Timed out after repeated connection closures |
| Baseline `25aa73a3`, three runs | Passed eventually; retained trace confirms the same disconnect race |
| Baseline inviter trace | `identified_non_relay_peer`, then `pairing_response_dropped` and `ConnectionClosed` |
| Decision point | `handle_identify_received`, baseline `src/runtime/runner.rs:19497` |

The baseline's eventual success does not establish stable behavior. Preserve
pairing-capable probes only within their existing admission and expiry bounds;
protocol advertisement must never grant overlay membership or infrastructure status.

The policy regression covers unadmitted identities, mismatched connections,
missing protocol support, deadline expiry, and quarantine. The direct namespace
test now rejects traces with Identify-driven non-relay disconnections, even if
pairing eventually succeeds. Both direct and relayed file pairing passed with
the fix, including traffic and the direct test's existing replay rejection.

Post-fix validation: 1,143 workspace tests passed, with 14 ignored in the default
run. Peerless code pairing also passed separately. Formatting and the required
Clippy correctness, suspicious, and performance checks passed; style warnings remain.

## Review Coverage Still Required

| Area | Evidence Inspected | Next Check |
| --- | --- | --- |
| Membership | Time-ordered ledger, version and epoch decisions | Adversarial ordering, compaction, trust anchors |
| Persistence | State load/save, pairing reconciliation, revoke application | Failure injection between durable and runtime transitions |
| Isolation | Android dispatch generations and overlap rejection | Stale leases, inbound validation, shared-TUN tests |
| Discovery | Network-change reset and periodic redial | Full timer/connection completion lifecycle |
| Android | Mutation polling, teardown, scheduled task cancellation | Slow native shutdown, stale callbacks, per-network independence |
| Resources | Live service identity and memory counters | Controlled baseline and comparable post-change sampling |

No absence-of-bug claim follows from these partial inspections.

## Implementation Sequence

1. Complete the review incrementally and add reproducers for confirmed defects.
2. Fix naming divergence and introduce shared effective authorization decisions.
3. Give membership evaluation and application a clear owner and transaction boundary.
4. Extract discovery/recovery state, decisions, and bounded effects.
5. Extract pairing orchestration and transport-session lifecycle where justified.
6. Address Android/CLI findings with focused changes and preserved interfaces.
7. Run operational gates, compare resource measurements, and reconcile docs.

Each change must have one reviewable purpose and an atomic Conventional Commit.
Push each verified commit to `main`; preserve user changes and wire/config formats.

## Verification and Resource Plan

| Layer | Approach |
| --- | --- |
| Rust | Focused regressions, workspace tests, format, required Clippy groups |
| Integration | Namespace tests and focused NixOS VM scenarios |
| Android | JVM tests, lint, native supervisor tests, reproducible APK |
| End to end | Revocation/restart, minimal LAN/relay recovery, network isolation |
| Physical/public | Record topology and exact binary; distinguish from local proofs |
| Full gate | Inspect exported checks, then execute in bounded batches |

Use at most two Cargo build jobs and one Nix build job during this work.
Run one VM scenario at a time. Keep downloads within the requested 10 Mbps cap.
Use cached dependencies where possible; do not start unrestricted fetches.

Use one reusable task target directory with debug information and incremental
compilation disabled when practical. Inspect its size before expensive stages;
pause builds for cleanup at 10 GiB of task-created temporary output.

Do not garbage-collect unrelated Nix roots or user caches. Clean task-created
temporary artifacts after their final use.

### Live Environment Preflight

On 2026-09-06, both local instances were running the store output
`s5mvmma78p5s0mlq936gp6n21fmf1aha-p2p-vpn-0.1.0`.
This is not evidence that the deployed binary matches the review baseline.

| Instance | MemoryCurrent | TasksCurrent |
| --- | ---: | ---: |
| monarchic-runners | 45,215,744 bytes | 19 |
| personal-devices | 50,794,496 bytes | 19 |

These are single service-cgroup observations, not an idle benchmark. Record
binary identity, duration, topology, workload, and counter deltas for comparisons.

### Validation Environment Recovery

The development shell could not be realized on 2026-09-06: missing dependencies
triggered source builds, and the Bash source fetch failed with HTTP 502.
No complete Nix-shell or package verification is claimed from that attempt.

Focused validation uses installed Nix Rust 1.97.1 and matching Cargo, Clippy, and
rustfmt binaries downloaded from the Nix cache with a 1,200 KiB/s limit. Archive
hashes are checked against cache metadata. Cargo dependencies are offline.

The fallback must retain `RUST_MIN_STACK=8388608` from the flake. Omitting it
caused a CLI test-thread stack overflow; library tests had passed. The complete
workspace subsequently passed with the intended setting.
