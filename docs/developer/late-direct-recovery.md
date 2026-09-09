# Late Direct Recovery

## Status

**RM-1 resolved within the scope below.** The ordinary on-link TCP collision is
reproduced, regression-tested and fixed; production recovery and compatibility
checks pass. This is not a universal recovery-time or production-readiness claim.
The original censored outcome and its excluded comparisons remain unchanged.

## Saved Evidence

| Observation | Result |
| --- | --- |
| Public recovery runs other than current repetition 3 | First direct success in 5-20 seconds |
| Current repetition 3 | First direct success at 360.12 seconds; confirmation misses the 375-second stage |
| Delayed stage connectivity | All 76 probes per endpoint succeed; relay remains usable before promotion |
| New LAN address dialing | Both endpoint logs record five transport handshake failures to the new peer LAN address |
| Eventual recovery | Direct TCP establishes, then owned UDP session negotiation succeeds |

Artifact directory: `/tmp/p2p-vpn-resource-cli-smoke.d52044ef81e4a036`.

| File | SHA-256 |
| --- | --- |
| `observations.jsonl` | `25a36d69999fd046038c95987b16a8ebafac41b1adf9cb853d6a0a4424535829` |
| `node-a.log` | `d5e6e9ddb73a7939d65b3f82a1e770693dcc1ed62d8cee0a9f68fc04e64eae33` |
| `node-b.log` | `b158bcf4e2b261f1f3ab74baa214825d48853dd7e1ad66739b8766ec7a3523d6` |

Both logs report `Handshake failed: input error` for the new LAN TCP endpoint.
They lack event timestamps; sampled state provides time bounds, not exact dial
instants. This is not evidence that discovery waited 360 seconds to learn an address.

### Recovery Timeline

Times below are relative to restored LAN access unless marked otherwise.
Observation sampling bounds events; untimestamped daemon logs establish ordering.

| Time / Order | Evidence | Interpretation |
| --- | --- | --- |
| About 1320.56 seconds into the run | Direct-recovery stage starts after address renumbering | Start of the original 375-second budget |
| Within five seconds | A-side discovered-dial failure counter increases | Dial work occurs promptly; not 360 seconds of discovery silence |
| About 20, 50, 100 and 190 seconds | Further counter changes; both logs contain five new-LAN handshake failures | Repeated transport failure precedes packet negotiation; counters also include other endpoints |
| Before first direct packet | A log lines 1470/1472 and B lines 1484/1490 record direct connections | TCP authentication eventually succeeds |
| After TCP authentication | A line 1481 and B line 1497 record owned UDP sessions | Authenticated control negotiation establishes the packet path |
| 360.12 seconds | First qualifying direct packet success; sampled direct matches near absolute 1680.62/1680.68 seconds | Real direct promotion, not merely a connected socket |
| 360, 365, 370, 375 seconds | Four qualifying direct-stage samples | Insufficient samples for the unchanged five-success oracle |
| 20.12 seconds into post-recovery | Five-success confirmation after the stage-local counter resets | Confirmation censoring explains the label, not the preceding runtime delay |

All 152 endpoint probes succeed during the delayed stage. Retained relay service
therefore covers the wait. The other five public recovery runs first succeed in
5-20 seconds and confirm in 25-40 seconds; none replaces the censored observation.

## Diagnostic Protocol

Frozen before execution. Hypothesis: simultaneous ordinary TCP dials reuse
listener ports, producing a TCP simultaneous-open connection with incompatible
Noise initiator roles; synchronized retries may repeat the failure.

| Property | Declared Value |
| --- | --- |
| Isolation | Existing user/network/PID namespace launcher; loopback only, no Internet route |
| Transport | Production `build_node` TCP security/multiplexer; discovery and automatic producers disabled |
| Overlap | Loopback netem delay 25 ms; both outgoing dials queued before polling |
| Conditions | Reused listener ports versus fresh outbound ports |
| Repetitions | Three fresh node pairs per condition; no retries within a pair |
| Deadline | Three seconds per pair; 30-second outer watchdog |
| Evidence | Per-side authenticated success or transport-error outcome; bounded launcher output, no private keys |
| Storage | Existing target; at most 1 MiB new diagnostic output; total task storage below 10 GiB |

Support requires reproducing bilateral handshake failure under reuse and
authenticated success with fresh ports. If results differ, retain them and revise
the hypothesis; do not run until a desired result appears.

This transport diagnostic cannot by itself explain every historical retry timestamp,
prove the precise kernel path taken by the saved run, or establish full VPN recovery.
Any runtime change requires separate regression and recovery verification.

### Initial Diagnostic Result

| Condition | Three Pair Outcomes |
| --- | --- |
| Reuse | A authenticates; B reports outgoing `Handshake failed: input error` |
| Fresh ports | Both outgoing sides authenticate |

The bounded diagnostic completed in 0.90 seconds. Its test exit means collection
completed, not that the hypothesis passed. It records each side's first terminal
event; an outgoing error does not exclude a subsequent accepted inbound connection.

**Bilateral outage was not reproduced.** The TCP transport binds reused ports to
an unspecified local IP. In this single-namespace loopback topology, route-selected
source addresses need not match the node's listener address; this differs from the
saved two-namespace LAN and can create an unintended self-connection.

The initial root invocation failed before execution because its mapped user could
not traverse the user's private build directory. Running as the owning user
executed the same binary successfully; no artifact permissions were relaxed.

### Evidence Missing After Version 1

- Reproduce both outgoing connections failing with distinct network stacks and correct source IPs, rather than infer that from the loopback result.
- Observe inbound acceptance as well as outbound failures until the fixed diagnostic deadline.
- Relate the five new-address failures to recovery quarantine, retry scheduling and eventual successful negotiation.

The saved stage's A-side failure counter first increments within five seconds of
LAN restoration. Additional increments occur around 20, 50, 100 and 190 seconds;
other-address failures also contribute to this counter. It cannot independently
timestamp individual LAN attempts.

Source backoff is 10, 20, 40, 80, 160 seconds, capped at 300 seconds, with a
10-second redial tick. This is consistent with synchronized failure amplification,
but timing agreement alone does not prove TCP simultaneous open in the saved run.

### Checkpoint Validation

- Namespace harness unit tests: 41 passed; 22 opt-in tests excluded.
- Cached Nix test-source and vendored-source parity check passed.
- Runtime, dependency, configuration and frozen measurement artifacts remain unchanged.

```sh
"$TEST_BINARY" --ignored --exact \
  tun_namespace_tcp_simultaneous_dial_diagnostic --nocapture
```

Run as the user owning the cached test binary. The existing launcher supplies
isolated namespaces and a 30-second watchdog. No physical network changes occur.

## Corrected Diagnostic Protocol

Version 1 remains in commit `51a8a1b2`; its inconclusive outcomes above are retained.
Version 2 corrects topology and observation, not production transport behavior.

| Property | Version-2 Declaration Before Execution |
| --- | --- |
| Topology | Separate endpoint network namespaces, joined only by a veth pair; no default route |
| Addresses | `10.250.0.1` and `.2`, each listening on TCP 4001 |
| Synchronization | Both listeners ready before a shared dial barrier; poll barrier every millisecond |
| Delay | 100 ms egress netem per endpoint to overlap the released dials |
| Conditions / repetitions | Reuse versus fresh outbound ports; three fresh pairs each, no retries |
| Observation | Full three-second window per side; inbound/outbound authentication and failures, plus final connected state |
| Bounds | Existing 30-second outer watchdog; at most 8 KiB result per node and 1 MiB total new evidence |
| Artifacts | Private `p2p-vpn-rm1-tcp-v2.*` directories; children reaped by existing namespace lifecycle |

Support requires no authenticated connection in either direction for reused
ports, with the matching handshake error, and authenticated connectivity for fresh
ports. A collection pass alone is still not a successful hypothesis test.

### Version-2 Results

| Condition | Result In Each Of Three Pairs |
| --- | --- |
| Reused port | Both peers remain disconnected; zero authenticated connections; bilateral `Handshake failed: input error` at about 0.403 seconds |
| Fresh port | Both peers connected; two authenticated connections per peer; no incoming or outgoing handshake errors |

The complete collection finished in 25.53 seconds. No pair was retried.
Log: `/tmp/p2p-vpn-rm1-v2-diagnostic.log`.

| Condition | Artifact Directory Suffixes |
| --- | --- |
| Reused port | `.9e25ec1b62ed323c`, `.12cae8290a9d738a`, `.aac2d3f33d5aeffc` |
| Fresh port | `.2cd1d4f80d43831f`, `.0eb68b65702f2466`, `.23e1c0744b35e2c1` |

Directory prefix: `/tmp/p2p-vpn-rm1-tcp-v2`.
Each result observes the entire three-second window, not just the first outgoing
error. Fresh-port outbound authentication occurred in about 0.51-0.52 seconds.

### Mechanism and Scope

- `libp2p-tcp 0.44.1` binds a reused listener port before connecting. Opposing dials can therefore use the same reversed TCP four-tuple.
- Both ordinary dials request the dialer role. `libp2p-noise 0.46.1` initiates Noise on outbound upgrades; it expects a responder's identity message after its first empty message.
- The isolated counterfactual changes only port allocation. Bilateral failure disappears with distinct outbound ports, while identity authentication remains enabled.
- The historical error signature and paired retry behavior match this reproduced failure. The saved run has no packet capture, so its exact kernel packet sequence is inferred, not independently observed.

This establishes a real ordinary-dial collision defect and a causal mitigation in
the diagnostic. It does not imply every handshake input error has this cause or
that all public NAT topologies permit direct connectivity.

## Scoped Runtime Fix

The [ordinary peer dial helper](../../src/runtime/runner.rs) allocates a fresh
outbound port when every supplied address is direct TCP on a currently active
local interface subnet. Interface discovery failure preserves the previous policy.

| Preserved | Boundary |
| --- | --- |
| Off-link public, DNS, mixed-target and relay dials | Retain existing port reuse |
| QUIC and UDP | Not selected by this TCP-only rule |
| Hole punching | DCUtR owns its separate dial path; no role or port override is applied there |
| Admission and scheduling | Existing peer condition, single-address dial concurrency, failure quarantine and backoff remain unchanged |
| Security and configuration | No new options, endpoint synthesis, authentication bypass, wire changes or resource-limit increases |

The policy regression failed against the old reuse decision before implementation.
It covers IPv4/IPv6 on-link TCP and exclusion of off-link, DNS, mixed, empty,
relay and QUIC target lists. Log: `/tmp/p2p-vpn-rm1-policy-negative.log`.

The production recovery checks below exercise this helper through minimal-config
discovery, not just manually configured fresh-port dials. Both endpoints log
`port_use: New` for the renumbered LAN before successful UDP packet delivery.

## Production Recovery Diagnostic

Declared before execution, separate from the unchanged phase-3 dataset and the
default phase-2 soak. One public and one private run; no retries until passing.

| Property | Declaration |
| --- | --- |
| Selector | `P2P_VPN_TUN_E2E_RECOVERY_COLLISION=1`; rejected with soak mode |
| Production entry | Existing recovery fixture, minimal ID-only peers, isolated bootstrap override |
| Fault | After confirmed relay fallback, renumber disconnected LAN from `10.253.0.0/24` to `10.254.0.0/24`; add 100 ms egress delay at both LAN endpoints |
| Recovery | Restore LAN without changing configs, injecting peer addresses, or restarting daemons |
| Pass gate | Original bidirectional direct UDP packet gate within 375 seconds, followed by original 30-second healthy dwell |
| Mechanism evidence | New-LAN ordinary TCP connection logs show `port_use: New`, then authenticated UDP session and actual packet delivery |
| Bounds | One cycle per DHT profile, unchanged 1,650-second watchdog and existing bounded logs; total task storage below 10 GiB |

The regular public one-cycle check passed before this additional diagnostic.
It took 251.37 seconds including setup, and exercised fresh ports at initial LAN
discovery. Its return used mixed-target reuse, motivating the fresh-address check.

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
P2P_VPN_TUN_E2E_RECOVERY_PROFILE=public \
P2P_VPN_TUN_E2E_RECOVERY_COLLISION=1 \
  "$TEST_BINARY" --ignored --exact \
  tun_namespace_automatic_discovery_recovers_after_link_changes --nocapture
```

Use `private` for the second declared profile. Build first and do not run builds
during observation. Generated artifact replay commands retain the diagnostic flag.

### Production Results

| Profile | Artifact Suffix | Packet Gate After First Restored-LAN Sample | Fixture Runtime / Result |
| --- | --- | --- | --- |
| Public | `.ef6c19c64da20ae6` | About 7.15 seconds | 136.14 seconds; passed |
| Private | `.c19977b28d538f2a` | About 32.24 seconds | 161.25 seconds; passed |

Directory prefix:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes`.
Including setup, terminal test durations were 142.94 and 167.98 seconds.

- Both endpoints authenticate TCP to the new subnet using fresh ports, then establish owned UDP sessions.
- Each run checks bidirectional packets over initial LAN, forced relay, and restored LAN, plus the original healthy dwells.
- Observer checks preserve configuration bytes and process identities; no management rescue, address injection or retry-until-pass occurred.
- The 30-second returned-LAN dwell completes well within 375 seconds of restoration. This is a separate oracle, not a rewritten phase-3 confirmation result.

These are bounded functional checks, not paired performance measurements or
proof that every mixed-target dial avoids collisions. Public/off-link and mixed
lists intentionally retain reuse; the reproduced fresh on-link discovery case is fixed.

## Verification

| Check | Result |
| --- | --- |
| Offline locked workspace | 1,461 passed; 36 opt-in tests excluded |
| Clippy | Correctness, suspicious and performance groups passed; advisory style warnings remain |
| Formatting / whitespace | Passed |
| Cached Nix source parity | Passed; identical vendored inputs across desktop and both Android source sets, matching Cargo test targets |
| Android native | x86_64/API 26 library compiled in 40.03 seconds; four target warnings |
| Production fresh-address recovery | Both declared profiles passed; fresh-port authentication, packet gate and healthy dwell |
| Namespace compatibility gates | 12 passed in 236.70 seconds: direct UDP, owned QUIC, mDNS, DHT, relay, invite import, live/relayed/code pairing, TCP pressure, network move and direct promotion |

Build logs: `/tmp/p2p-vpn-rm1-final-{workspace,clippy,nix,android}.log`.
Diagnostic binary SHA-256:
`f4396d47c8871a3c1ac1b4f0212d20e317d22e12777b9dedb5b50a9e8944c408`.

The cached Nix check preserves its source-parity assertions but overrides tool
paths. It is not a full Nix package build. ARM64, APK/device and physical-WAN
acceptance are not claimed. No Lean model exists for this transport behavior.

## Final Disposition

| Question | Resolution |
| --- | --- |
| Why was promotion late? | Both sides repeatedly fail fresh-LAN TCP authentication before packet negotiation; the reproduced port-reuse collision matches this signature and synchronized backoff amplification |
| Why was confirmation censored? | First direct success leaves only four samples in the 375-second stage; the next stage resets its confirmation counter |
| What is proved experimentally? | Changing only port allocation removes bilateral authentication failure in all three counterfactual pairs; the production rule then recovers renamed, delayed LAN paths in both DHT profiles |
| What remains inferred? | The saved run's exact TCP packet sequence and individual retry timestamps; no historical packet capture or event timestamps exist |
| What evidence is reused? | Original five-cycle settling/renewal soaks for unchanged owner, renewal and scheduler logic; fresh focused checks cover the changed dialing behavior |
| What is not claimed? | A new long soak of the fixed binary, elimination of every handshake error, collision prevention for mixed-target lists, full performance comparisons or physical-WAN certification |

The [workstream acceptance](kademlia-workstream-acceptance.md) closes this scoped
recovery gate. RM-2 through RM-6 remain separate follow-ups. Original censored
results, deadlines and comparison exclusions are not reclassified.

No new dependencies, production configuration or wire formats changed. The
interface-subnet lookup runs on admitted dial attempts, not per packet; existing
backoff and concurrency bounds remain in force. No new performance claim is made.

Final storage: 7.818 GiB, below 10 GiB. No downloads, physical deployments,
raw-artifact deletion or concurrent builds during recovery observation occurred.
