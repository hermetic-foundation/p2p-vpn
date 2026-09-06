# Reliability Review and Refactoring

## Status

Started 2026-09-06 against `560754de`. This is an evolving review, not a completed
security audit. Source inspection is distinguished from reproduced behavior.

## Completion Criteria

| Requirement | Required Evidence | Status |
| --- | --- | --- |
| Correctness review | Findings with source references and disposition | In progress |
| Authorization consistency | Shared policy and cross-consumer regression tests | In progress |
| Runtime ownership | Cohesive state owners and testable recovery decisions | In progress |
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

Implemented the borrowed `EffectiveAuthorization` policy view in `membership.rs`.
Routes, DNS, pairing name ownership, packet admission, runtime membership, and
inventory ordering now use its local eligibility and static/signed precedence.

Forwarding derives the packet allowlist from its transport-peer map, eliminating
one repeated ledger evaluation per construction, merge, and configuration update.
Forwarding now also shares one evaluated ledger between route compilation and
transport admission. Other runtime consumers still evaluate independently;
cross-runtime snapshot ownership remains part of the review.

New policy tests cover unknown versus configured identities, exact grant expiry,
and local expiry/resignation without erasing surviving network membership.
The cross-consumer integration test exercises minimal configuration, remote
revocation, local resignation, and retained audit state through public constructors.

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

Priority: documentation. Status: corrected.

`architecture.md` describes signed records as a delegated trust graph and
`README.md` indexes membership as a trust graph. The current ledger implements
flat any-member governance with non-cascading revocation and legacy restoration.

Update these descriptions with the implementation changes and retain explicit
migration information in the membership reference.

Architecture and the developer index now describe ownerless governance, event-time
authorization, non-cascading revocation, and the local operational boundary.

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

### R6: Legacy Packet Allowlist Constructor Ignores Signed History

Priority: P2 API risk. Status: reproduced and fixed.

`AuthorizedPeers::from_config` in `src/runtime/packet.rs` collects static peer IDs
without evaluating signed history. Current production callers use the fallible
constructor or the forwarder's evaluated map; no live runtime bypass is established.

The added regression demonstrated that the exported infallible constructor allowed
a revoked configured peer. It now retains its signature but delegates to the shared
policy and fails closed on invalid input.

Coverage checks active signed members, revoked configured peers, malformed peer IDs,
invalid signatures, and cross-consumer agreement after local resignation.

## Review Coverage Still Required

### Namespace Verification Findings

The expanded suite exposed stale capability-event assertions and a datagram wait
for `inbound_accepted_packets` on `daemon-state`, which does not expose that metric.
Use the structured event and query `daemon-status` for inbound evidence instead.

The harness waited for process exit before draining piped diagnostics. A failed
assertion could fill the pipe and appear as a 90-second timeout. Capture now drains
both streams concurrently with a 1 MiB per-stream limit, and node guards reap
children on assertion unwind. Focused capture and cleanup regressions pass.

The pre-refactor runtime at `7300f55e` also failed the direct UDP namespace case
with no packet-plane session. The changed runtime's expanded run had discovery,
owned datagram, and relay-promotion failures; these are not waived operational gates.
Separate stale evidence checks from runtime negotiation and discovery defects next.

After correcting the metric surface, owned QUIC passed on the pre-refactor runtime.
Both direct UDP and owned QUIC passed with the shared authorization view as well.
The default workspace suite passes 1,148 tests, with 14 ignored; required Clippy
groups and formatting pass. Style warnings remain; operational coverage is incomplete.

The DHT fixture used `10.252.0.0/24` and no prior peer address. Public-discovery
address admission deliberately rejects third-party private transports. Check fixture
scope against that security boundary before considering any production-policy change.

The latest mDNS snapshot has one validated peer, one healthy UDP session, five
transmitted packets, and five accepted inbound packets, but no datagram transmissions.
Check whether the fixed ping burst precedes path promotion: session existence alone
does not prove a usable datagram path. Preserve traffic evidence when repairing the test.

Authorization-policy increment's namespace run: 8 passed, 3 failed in 229 seconds.
The open failures are DHT discovery, mDNS datagram evidence, and relay promotion.
Direct UDP, owned QUIC, relay forwarding, file/code pairing, invite import, and
network-move recovery passed. No full-suite or production-readiness claim follows.

The fixture follow-up uses simulated public addresses in isolated namespaces and
explicit external listeners because fixture AutoNAT is disabled. Public-discovery
validation is unchanged. Datagram pings wait for path selection, not just session creation.

An additional mDNS run exposed simultaneous first-handshake failure. The 15-second
startup budget could expire before a 10-second failure backoff and the next 10-second
retry tick. The fixture now allows 30 seconds for that bounded recovery cycle.

Capability evidence now checks accepted-capability counters, including inbound
requests. Receiving capabilities without acceptance still fails the regression.
Previously, the outbound-response-only log assertion rejected valid inbound handshakes.

After these corrections, all 11 namespace scenarios passed together in 184.51 seconds.
Three additional mDNS runs passed. These checks retain actual packet traffic,
datagram counters, route ownership, and relay-promotion assertions.

The final workspace run passed 1,149 tests with 14 ignored, including the repaired
infallible allowlist and cross-consumer authorization regression.

Required Clippy groups and formatting pass. The broader VM, Android, persistence,
and resource-comparison gates remain open; the ownership work is not complete.

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

### Forwarding Snapshot Milestone

`ForwardingAuthorization` groups routes, transport IDs, and packet admission.
Construction, membership merge, and prepared reconfiguration each build a complete
replacement from one evaluated ledger and timestamp before mutating live state.

| Boundary | Result or Remaining Work |
| --- | --- |
| Evaluation | One ledger evaluation feeds both forwarding routes and transport authorization. |
| Forwarder state | One private snapshot replaces derived authority; history and replay state remain separate. |
| Prepared updates | Runtime call sites prepare and commit synchronously; no intervening forwarder mutation was found. |
| Revision handling | Merge still detects history or effective-authority changes; reconfiguration retains its existing history-change rule. |
| Regression tests | Failed preparation preserves forwarding; local/remote expiry removes routes and packet authority together. |

Keep existing public constructors and serialized contracts. Do not equate an audit
entry with operational authority or erase history to simplify the snapshot.

`ForwarderUpdate` remains a replacement, not a merge or generation-checked token.
The public API does not prevent callers from committing a stale or foreign update.
Runtime callers do not do this; changing that contract needs separate design and
coverage rather than silently ignoring updates in this refactor.

Remaining ownership work includes sharing evaluation with TUN/runtime membership,
reviewing revision consumers, and extracting recovery decisions and timer effects.

### Snapshot Validation and Recovery Finding

| Check | Result |
| --- | --- |
| Forwarder tests | 47 passed, including failed preparation and exact local/remote expiry. |
| Workspace | 1,151 passed; 14 ignored. |
| Required Clippy groups and formatting | Passed; existing style warnings remain. |
| Serial namespace suite | 10 passed; network-move recovery failed. |
| Isolated network-move repeat | Failed at the outer 90-second deadline. |
| Unchanged `95bbcb44` comparison | Also failed at the outer 90-second deadline. |

The suite failure timed out restoring `direct_udp_datagram` after relay fallback.
The last state retained a selected relay and an unconfirmed UDP path. Later log
entries reported UDP promotion. The isolated changed and baseline runs also showed
pending-hello expiry followed by delayed renegotiation.

Retained local evidence:

- `/tmp/p2p-vpn-network-move-tun-e2e-712261`: changed full-suite failure and daemon snapshots.
- `/tmp/p2p-vpn-network-move-tun-e2e-717537`: unchanged-baseline timeout and node logs.

This is an unresolved, reproduced recovery-test failure, not a waived gate or
proof of a production outage. The fixture forces three-second session lifetimes;
probe scheduling and handshake retry ownership need deterministic investigation.
Do not increase deadlines merely to conceal this behavior.

### Handshake Timeout Follow-up

The timeout handler removed pending negotiations without starting another attempt.
A deterministic runner test reproduced the missing retry with cached capabilities
and a healthy direct path, without a new connection or capability event.

Timeout and session-expiry paths now share negotiation eligibility. Each timeout
can start one replacement; subsequent timer ticks preserve its 25-second deadline.
Revoked/unknown peers and relay-only paths cannot use this retry path.

Pending initiators also own their libp2p request ID. Late accept/reject responses
from expired attempts are discarded before they can replace or remove a newer
negotiation. This does not change the wire format or configuration.

The regression failed before the retry and passed afterward. Workspace validation
passes 1,152 enabled tests. Network-move recovery passed in the serial suite and
two isolated repeats (47.25 and 47.21 seconds), without changing its deadlines.
Formatting and required Clippy groups pass; existing style warnings remain.

`/tmp/p2p-vpn-network-move-tun-e2e-733676` retains a passing trace showing a pending
hello expire, a new request ID, and direct UDP promotion. The test verifies traffic
before link loss, over relay fallback, and after direct-path restoration.

The complete namespace run was still 10/11: relay promotion established healthy
direct TCP and UDP paths but failed its explicit DCUtR-success assertion. Node B
reported `AttemptsExceeded(3)`; neither node reported success. This is not evidence
of successful hole punching and remains an open verification issue.

Retained failure: `/tmp/p2p-vpn-relay-promotion-tun-e2e-732272`. Neither the new
timeout-retry branch nor stale-response branch ran in that failure trace.
Inbound handshake ordering and the broader session-ownership audit also remain open.

### Hole-Punch Deduplication Review

Two deterministic defects were found while tracing the relay-promotion failure:

| Defect | Evidence | Correction |
| --- | --- | --- |
| Socket direction substitutes for handshake role | Two-peer regression left no common surviving connection during simultaneous open. | Use the transport-realized handshake role; ordinary listeners remain responders. |
| Duplicate retirement erases completed DCUtR evidence | Success from a current, retiring connection was filtered before metrics/logging. | Accept current-epoch completion evidence without making the connection usable. |

Both regressions failed before their fixes. Existing ordinary-connection
deduplication checks still pass. Ping/Identify eligibility remains unchanged;
unknown and old-epoch DCUtR success events remain rejected.

The pinned libp2p core documents role override specifically for simultaneous open.
Its DCUtR behavior queues success during outbound establishment, so application
deduplication can retire the connection before that queued result is consumed.

QUIC is tested separately: the pinned transport still dials as a client for
`PortUse::Reuse`, including listener overrides. Only a listener override with
`PortUse::New` selects its receive-side hole-punch operation.

| Verification | Result |
| --- | --- |
| Offline workspace tests | 1,155 passed; 14 intentionally ignored. |
| Explicit serial namespace suite | All 11 passed, including relay promotion and network movement. |
| Additional isolated relay-promotion run | Passed in 22.07 seconds. |
| Format and whitespace checks | Passed. |
| Workspace/all-target Clippy | Passed; existing style warnings remain. |

The direct-datagram fixture also needed explicit readiness before its finite ping
burst. One failure sent the entire burst over TCP before UDP negotiation finished,
then waited for datagram counters without generating further traffic.

Both direct fixtures now wait for sessions and selected datagram paths on both
nodes. Traffic and acceptance assertions remain intact. The reproduced defects
above are fixed; isolated tests do not establish traversal of real public NATs.

### Update Failure Audit

| Path | Observed Behavior | Follow-up |
| --- | --- | --- |
| Prepared pairing/revocation | Install routes before committing forwarding and runtime membership. | Preserve failed-preparation and route rollback coverage. |
| Expiry timer | Refresh forwarding first; TUN reconciliation errors propagate out of the runtime. | Test supervisor restart and persisted expiry behavior. |
| Revision consumers | DNS refresh and membership persistence use the same revision counter. | Review whether effective-only changes need separate dirty tracking. |

The expiry path consumes its pending-change flag before installing routes, but
the caller exits on error. Source inspection does not establish a stale-authority
retry defect: no in-process retry occurs on that path today.

### Address Retention: Confirmed Growth

Priority: P2. Status: reproduced through admission at `909a9482`; not fixed.

`DiscoveredPeerAddresses::insert_at` (`runner.rs:8899` at this milestone) appends
each distinct peer/address pair without an entry cap. Entries have a one-hour
TTL. The admission path restricts retained entries to authorized overlay peers,
but that does not bound repeated address changes from an authorized peer.

| State | Current Bound |
| --- | --- |
| Recovery dial attempts | Explicit maximum and periodic pruning |
| Recovery discovery queries | Explicit total and concurrent maxima |
| Retained discovered addresses | TTL only; no total or per-peer entry limit found |

#### Admission Measurement

The opt-in `measure_discovered_address_retention_through_admission` diagnostic
supplies distinct public TCP addresses through `learn_peer_address`. It never
polls the swarm and does not send network traffic. Production limits are unchanged.

| Peer Authorization | Supplied | Runtime Entries | Runtime Address Bytes | Additional Kademlia Entries |
| --- | ---: | ---: | ---: | ---: |
| Authorized | 32 | 32 | 256 | 32 |
| Authorized | 128 | 128 | 1,024 | 128 |
| Authorized | 512 | 512 | 4,096 | 512 |
| Not authorized for overlay | 512 | 0 | 0 | 512 |

After advancing the expiry timestamp beyond the one-hour TTL, all 512 authorized
runtime entries disappeared. All 512 additional Kademlia entries remained.
The fixture starts with five Kademlia addresses; totals therefore reach 517.

This proves cumulative retained-state growth, not process memory exhaustion or a
measured idle-CPU regression. Byte counts cover encoded runtime multiaddresses
only, excluding allocation overhead, peer IDs, downstream copies, and dial state.

| Downstream Store | Pinned Implementation | Implication |
| --- | --- | --- |
| Kademlia `Addresses` | Distinct values appended to `SmallVec`; no per-peer entry cap | Runtime eviction alone cannot bound this copy. |
| AutoNAT request-response addresses | `PeerAddresses` LRU: 100 peers, 10 addresses each | Entry count is bounded independently; encoded address bytes still need admission limits. |
| AutoNAT server removal | Removes server eligibility, not its request-response addresses | Do not assume removal clears retained address memory. |

#### Ownership Plan

1. Introduce one bounded discovered-address owner with explicit admission,
   refresh, eviction, expiry, and authorization-removal effects.
2. Bound encoded address size, entries per peer, and total retained entries.
   Reserve capacity across peers and keep fresh LAN and relay alternatives usable.
3. Apply admission before downstream insertion, including public infrastructure
   peers that are intentionally not authorized overlay members.
4. Reconcile Kademlia copies on replacement and expiry without removing explicitly
   configured bootstrap addresses or active paths owned by another source.
5. Preserve retry quarantine on repeated announcements. Report capacity rejection
   and eviction separately from invalid-address rejection.

Acceptance requires deterministic overflow, refresh, source-ownership, expiry,
and revocation tests, followed by minimal-config namespace recovery scenarios.
Repeat this diagnostic after implementation; a passing diagnostic alone is not
evidence that a security bound exists. This finding remains open.

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
