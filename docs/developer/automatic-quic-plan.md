# Automatic QUIC Packet Transport

## Status

Core implementation published as `8690bce2`; NixOS wiring published as `43345ef1`.
Acceptance remains incomplete. Local evidence does not certify physical Android or public-network recovery.

## Acceptance Audit

| Requirement | Verified evidence | Remaining work |
| --- | --- | --- |
| Defaults and overrides | Shared config and Android profile regressions; 26 NixOS contracts; Pixel profile upgrade | Native NixOS activation |
| QUIC payload preference | Minimal TUN fixture; physical Pixel/Linux traffic; OnePlus bidirectional backend counters | Longer physical stability and unresolved Pixel loss |
| Compatibility | Explicit UDP-only and stream-only current peers; override round trips; isolated relay payloads | Archived-release compatibility is not established |
| Autonomous recovery | Four initiator orderings; namespace blocking; OnePlus QUIC block/re-promotion | Physical movement, residual loss and sustained settling |
| MTU and isolation | 1,280-byte IPv4 fallback/recovery traffic; Android supervisor tests | Smaller-underlay MTU boundaries and physical multi-network behavior |
| Verification | 1,526 workspace tests; required root Clippy groups; ARM64 build; fresh JVM tests; cached source parity | Full Nix package realization and remaining targeted scenarios |
| Deployment | Verified debug APK upgrades on Pixel and OnePlus preserving profiles | Physical stability, fallback and recovery checks |
| Delivery | Core and NixOS commits pushed to main | Final evidence review and requirement-by-requirement closeout |

- UDP compatibility uses a current runtime with QUIC disabled, not an archived release.
- Namespace underlays are isolated fixtures, not substitutes for physical or public-NAT evidence.
- The separate Android always-on process-restart finding is not claimed fixed here.
- Remaining evidence gaps do not authorize deployment, underlay changes or personal-flake edits.

## Physical Pixel Check: September 10

The authorized in-place APK upgrade preserved the existing identity, hostname and network.
Wi-Fi remained selected with zero reported underlay changes. USB was management only.

| Artifact | SHA-256 |
| --- | --- |
| Installed debug APK | `02ead2d9990d90a0d2c4ef914cdaac9923afdcfb50ce438d426f27d2fc6cdb4e` |
| ARM64 native library | `3d48757b7b4bdd2d4f68f9cf8b9f952479bbd3d75e6c875cd85d87c926258cd5` |

The laptop temporarily ran the new binary through a runtime-only systemd override.
Its existing config omitted `packet_plane`; no endpoints, profiles or firewall rules changed.
The Nix-managed binary was restored after testing; the updated APK remains installed.

| Check | Result |
| --- | --- |
| Updated phone against original Linux binary | Five small pings each direction, all delivered |
| Initial Linux service restart | One of five replies; retained as restart interruption evidence |
| Automatic QUIC discovery | One session; Linux selected `direct_quic_datagram` without endpoint configuration |
| Phone to updated Linux, small packets | Ten of ten replies; Linux owned-QUIC payload counter reached ten |
| Linux to phone, 1,280-byte IPv4 packets, DF set | Ten of ten replies |
| Phone to Linux, 1,280-byte IPv4 packets | Four of ten replies; stability acceptance failed |
| Counters after both full-size runs | Linux owned-QUIC 22, owned-UDP 8; selected path had changed to UDP |

- At 12:41:20 device/laptop journal time, both datagram paths were demoted for probe timeout.
- At 12:41:40 QUIC was demoted again; probe acknowledgements also arrived at those timestamps.
- Android still reported one QUIC session and no underlay transition after the failure.
- Android's legacy payload counter combines UDP and QUIC; it cannot certify the return backend alone.

This establishes physical QUIC payload capability, not stable QUIC preference or an MTU root cause.
Next: correlate probe deadlines, receive scheduling and backend selection around the observed loss.
Do not classify these failures as successful fallback or public-network recovery.

### Follow-Up Isolation

- With the original Linux binary restored, the updated phone delivered 30/30 full-size pings.
- Each packet was 1,280 bytes including IPv4/ICMP headers; the run lasted 29.057 seconds.
- Android reported Wi-Fi, unchanged runtime generation and zero underlay selection changes.
- During this comparison, Android reported awake, externally powered and neither idle mode active.

The earlier Android logs show six datagram path demotions across five peers at 12:41:36.
Public control events continued during the loss window, which argues against a completely stalled runtime.
The comparison is sequential, not a controlled proof that QUIC alone caused the loss.

| Observation | Interpretation and next check |
| --- | --- |
| Stream probe rejected at 12:41:20, sequence 131 | Packet-plane probes bypass that forwarder replay check; do not attribute QUIC loss to this event alone |
| UDP and QUIC probes demoted together | Correlate socket delivery and probe deadlines, not only QUIC negotiation |
| Full-size original-binary comparison passed | Retain as baseline; repeat new-binary traffic with bounded packet timing evidence |
| Android summary combines datagram payload counters | Add backend-specific Android observation before claiming both-direction QUIC-only delivery |

Next physical run requires fresh deployment authorization for the temporary Linux binary.
Keep profiles, endpoints, firewall and underlay unchanged; restore the Nix-managed service afterward.
Collect bounded outer-packet metadata and timed path/counter snapshots alongside full-size pings.
Do not change probe deadlines, replay checks or MTU until the failure is isolated.

### Android Counter Plan

1. Parse existing native owned-QUIC and owned-UDP payload counters into `RuntimeSummary`.
2. Expose additive debug status fields; preserve the legacy cumulative combined counter.
3. Keep new fields scoped to current native runtimes; compare deltas only without intervening restarts.
4. Test mixed backends, network aggregation, malformed input, saturation and restart reset.
5. Run cached JVM tests and debug APK assembly without redeploying or rebuilding native code.

These observations do not change transport selection and do not resolve the physical loss defect.

Implemented and locally verified: all JVM tests and debug APK assembly passed using cached tools.
The two affected summary suites passed 12 tests, including four new regressions.
APK alignment passed; native library hash is unchanged from the physical run.

New APK SHA-256: `2be7a7d92012e45ff1f51a5220c6df8d730ab22acffac1c5ef7a52a5ece1aee8`.
This diagnostics APK has not been deployed. Rust/Nix checks were not rerun for this Java-only change.

## Physical OnePlus Check: September 10

The user authorized replacing the Pixel test device with the already-paired OnePlus,
upgrading its APK and temporarily restarting the laptop with the test binary.
No pairing, endpoints, firewall rules, personal-flake settings or underlays changed.

| Item | Observed value |
| --- | --- |
| Device | OnePlus 9 Pro, Android 16, USB serial `1ebfe979` |
| Preserved profile | `personal-devices`, `oneplus-9-pro`, `100.64.241.163` |
| Preserved peer ID | `12D3KooWBEiDFMNfaHXC7kQQAwjgsLZYv8xjVEQY77QuasfC8pPs` |
| Installed APK SHA-256 | `cf0921ff4e015d86d6a94c24c5d339974642017adfe25d01e84db44d98ce7cac` |
| Test Linux binary SHA-256 | `aad10df4f669ab7254ab97b390b72f1dad5094bd3dd3bb4c158c50de47bd11f0` |
| Source revision | `6aaee223` |
| Test environment | Validated Wi-Fi; phone awake and USB-powered; management over USB |

The existing network was reconnected after APK installation. The laptop's existing
minimal config still omitted `packet_plane`. It discovered and selected the phone's
owned QUIC datagram endpoint without manual endpoint configuration.

| Traffic check | Replies | Mean RTT |
| --- | --- | --- |
| Laptop to phone, small | 10/10 | 35.718 ms |
| Phone to laptop, small | 10/10 | 19.864 ms |
| Laptop to phone, 1,280-byte IPv4, DF | 20/20 | 94.958 ms |
| Phone to laptop, 1,280-byte IPv4, DF | 30/30 | 88.906 ms |
| Laptop to phone, one-minute 1,280-byte repeat, DF | 60/60 | 147.419 ms |

- Before the repeat, Linux counted 69 owned-QUIC payloads and one QUIC-stream fallback;
  Android counted 70 owned-QUIC payloads. Both owned-UDP counters were zero.
- After the repeat, Linux counted 129 owned-QUIC payloads with fallback unchanged;
  Android counted 130. Both owned-UDP counters remained zero.
- Linux retained `direct_quic_datagram` with unchanged establishment time and zero probe failures.
  Android reported generation one, unchanged profile and zero underlay changes/recoveries.
- The first attempted ping overlapped the service restart and failed to bind the absent `pv1`.
  It was not included in the completed traffic runs above.

### Capture And Limits

- A 110-second, 96-byte-snaplen capture was restricted to the phone's owned datagram endpoints.
  It recorded 606 packets and zero reported capture drops; no builds ran during traffic tests.
- QUIC endpoints `192.168.0.229:48978` and `192.168.0.180:56065` exchanged 518 packets.
  UDP fallback endpoints exchanged 88 control packets while owned-UDP payload counters stayed zero.
- Each QUIC direction had 90 outer UDP packets longer than 1,280 bytes.
  This is transport-size evidence, not decrypted attribution of individual overlay packets.
- Local evidence: `/tmp/p2p-vpn-oneplus-quic-20260910.pcapng`,
  `/tmp/p2p-vpn-oneplus-{before,after,final}.state` and matching Android status snapshots.
- The capture ended during the repeat, not after it. The full repeat's ping output is retained
  in `/tmp/p2p-vpn-oneplus-sustained-ping.log`; its maximum RTT was 468.871 ms.

The runtime-only Linux override and copied binary were removed, and the original Nix-store
service was restored; five post-restoration pings all succeeded.
The updated APK remains installed with the existing profile.
Temporary project storage measured 9,780,196 KiB, below the 10 GiB limit.

This bounded OnePlus Wi-Fi pass does not resolve the earlier Pixel failure or certify
sleep, cellular, physical fallback, movement or long-duration stability. No timing or
transport fixes were inferred from a passing run; those acceptance gaps remain open.

## Physical OnePlus Block Check

Authorized deployment used the prepared fallback-fix APK and temporarily replaced the
laptop's `personal-devices` service binary. Existing identity, pairing and minimal config
were preserved. The phone stayed on validated Wi-Fi, awake and USB-powered.

| Artifact | SHA-256 |
| --- | --- |
| Installed APK | `1d71d2cad8c4c1cfaaa532589461368a69c72574c00fbdfe929df470d963e6f7` |
| Linux binary | `5b5c00c18a1db243403b5bb91b6910c05c775f7fec77fb2e33a6f5fdebda18b9` |

### Procedure And Results

- On September 10, blocked laptop input/output UDP matching phone `192.168.0.229:40402`
  from 13:51:18 to 13:52:45 CDT. The dedicated chain counted 65 dropped packets.
- A 120-second automatic flush timer protected against a stranded block.
  Explicit unblock occurred first; the timer, jumps and dedicated chain were removed/stopped.
- No VPN runtime restart, manual endpoint update or pairing change occurred between block
  and recovery. The laptop process stayed `2438821`; Android reported generation one.

| Stage | Result |
| --- | --- |
| Baseline, 1,280-byte IPv4, DF | 5/5 each direction; QUIC payload counters increased |
| Block transition, small packets | 21/45; first reply at sequence 25, about 26 seconds after blocking |
| QUIC blocked, full MTU | 5/5 each direction on fallback |
| Unblock transition, full MTU | 44/45; autonomous QUIC session establishment at 13:52:56 |
| Post-recovery, phone to laptop, full MTU | 10/10 |
| Post-recovery, laptop to phone, full MTU | 9/10; stability acceptance remains incomplete |
| Original Nix Linux service restored, full MTU | 5/5 |

- At unblock, Linux owned-QUIC/UDP counters were 20/44; Android reported 16/26.
  These are runtime-wide counters, not per-peer counters or delivery acknowledgements.
- After the transition, Linux selected QUIC with counters 32/77; Android reported 30/56.
  After the final tests, Linux reached 52/77 and Android 48/57: do not call this QUIC-only delivery.
- Linux logged QUIC timeout at 13:51:34 and UDP timeouts at 13:51:54 and 13:53:54.
  UDP was not firewall-blocked; these events require investigation, not a deadline workaround.
- The 180-second endpoint-scoped capture retained 230 packets with zero capture drops.
  It ended before the last post-recovery loss, so it cannot locate that packet's loss point.

### Evidence And Cleanup

- Capture: `/tmp/p2p-vpn-oneplus-block-20260910.pcapng`.
- Logs and snapshots: `/tmp/p2p-vpn-block-*` and `/tmp/p2p-vpn-unblock-*`.
- The updated APK remains installed. The temporary Linux override/binary and firewall rules
  were removed; the original Nix-store service was verified active with no drop-ins.
- Temporary project storage measured 9,782,132 KiB, below 10 GiB.

This demonstrates bounded physical fallback and re-promotion, not lossless recovery or
settled stability. The OnePlus residual loss and earlier Pixel failure remain unresolved;
physical underlay movement and sleep behavior were not tested in this run.

### Probe Correlation Follow-Up

- Retained Android logs show a UDP timeout at 13:52:17.361, payload-based path refresh
  at 13:52:17.477 and an RTT confirmation at 13:52:17.479. Proximity alone does not identify a probe.
- The old logs omitted probe tokens. Add `probe_token` to existing RTT confirmation events
  and emit `path_probe_expired` with peer, path, token, elapsed age and timeout threshold.
- Only expiry adds an event; normal probe/packet send frequency and all deadlines are unchanged.
  Tokens are correlation identifiers, not keys; packet contents are not logged.
- The tracker regression now verifies that expiration preserves the token and send timestamp.
  Late acknowledgements for already-removed tokens are still ignored, not newly logged.
- Journal inspection found no matching rejection/drop events during the bounded test window.
  Android's rolling buffer no longer covers the full window; absence is not proof of no rejection.

Next physical capture must include both owned UDP and QUIC endpoints plus only the test
ICMP flow on the TUN interface. Keep capture running through the final stability check;
the previous QUIC-only capture cannot localize a packet using UDP fallback.

This is diagnostic work, not a transport fix or evidence that the residual loss is resolved.
No redeployment, firewall mutation or underlay change was performed during this inspection.

- Validation: 15 probe-focused tests passed; the full core suite passed 1,167 tests with eight
  ignored in 46.48 seconds. Required Clippy groups passed in 16.41 seconds with existing warnings.
- Rustfmt and whitespace checks passed. Workspace integration, Android rebuild and physical
  testing have not been rerun for these diagnostic fields; previous artifacts lack them.

### Prepared Correlation Artifacts

- Source `ff4d9884`: offline ARM64 build passed in 39.26 seconds; Linux binary build
  passed in 40.84 seconds. Both reused existing caches with two Cargo jobs.
- APK assembly passed in seven seconds and 16 KiB alignment passed. JVM tests remained
  up-to-date. The four existing Android platform dead-code warnings remain.
- Nix-evaluated ARM64 source `/nix/store/qmn93djd7sbndh7snvlq90w2cjnzb50f-source/src`
  matched working-tree `src`. This is source parity, not full Nix realization.
- Temporary storage: 9,782,304 KiB. Neither artifact was deployed; the requested repeat
  physical capture still awaits authorization.

| Prepared artifact | SHA-256 |
| --- | --- |
| Debug APK | `5da7036df4c30f7b24e08a30368ce44a100c33b396c5a992fa0dc8407b61d87f` |
| ARM64 library | `ac716abb317630ed7705f0e62647ae890ad0de0722fb282dcd0ad1dcc98f76d6` |
| Linux binary | `21da5503ffc19f9b25963154056c633a3af9d4242950d1865106178b409f473c` |

### Correlated Physical Repeat

The user granted standing test/deployment authorization for the attached OnePlus.
Preserve its identity and pairing; physical underlay switches and other devices remain
separately scoped. This run used the prepared `ff4d9884` diagnostics artifacts above.

- Temporarily ran Linux process `2458974` with the existing minimal configuration.
  Phone identity/profile remained unchanged; neither runtime was manually restarted during measurement.
- QUIC endpoint `192.168.0.229:42891` was blocked from 14:12:12 to 14:13:22 CDT on September 10.
  The dedicated chain dropped 55 packets; its automatic unblock timer was stopped after explicit removal.
- Capture included phone QUIC and UDP (`42845`) endpoints on `wlp1s0`, plus only the
  overlay test ICMP flow on `pv1`. Android logs were collected continuously over USB.

| Full-MTU traffic stage | Replies |
| --- | --- |
| Baseline laptop to phone | 10/10 |
| Block transition laptop to phone | 30/45; sequences 1-15 lost |
| Blocked-path phone to laptop | 5/5 |
| Unblock/re-promotion laptop to phone | 45/45 |
| Settled laptop to phone | 60/60 |
| Settled phone to laptop | 30/30 |

- Settled Linux owned-QUIC/UDP counters changed from 50/63 to 140/63.
  Android changed from 45/60 to 134/61; retain the one UDP payload rather than claim QUIC-only traffic.
- At 14:14:33, both Linux probe paths expired after 14,997 ms against a 12,000 ms threshold.
  Full-MTU payload traffic still completed successfully during this check.

| Path | Expired probe token | New confirmed token at 14:14:33 | New RTT |
| --- | --- | --- | --- |
| UDP | `5159263635875717152` | `5557157863473720637` | 16 ms |
| QUIC | `18371448710816733584` | `299777907167434412` | 28 ms |

The nearby confirmations were fresh probes, not delayed acknowledgements of the expired tokens.
The capture records both outgoing probes at 14:14:18.096 and no small incoming UDP datagram
until 14:14:33.118. It does not establish whether the earlier probes reached the phone.

- Evidence: `/tmp/p2p-vpn-correlated-20260910.pcapng` and `/tmp/p2p-vpn-correlation-*`.
  Five-minute capture: 1,151 underlay packets and 375 TUN packets, zero reported capture drops.
- TUN capture contains all 195 requests and 180 replies: exactly the 15 intentional-transition
  losses. It covers the final traffic checks; no post-recovery loss reproduced in this run.
- Restored the original Nix Linux service and removed the task-owned override, binary and
  firewall chain. Timer inactive; captures exited. Five post-restoration pings all succeeded.
- Storage measured 9,783,356 KiB. The diagnostics APK remains installed on the OnePlus.

This strengthens physical fallback/re-promotion evidence and clarifies the probe timeline.
It does not explain the prior post-recovery loss, certify long-duration stability or test
physical network movement. No timeout or routing policy change was made to obtain this pass.

### Settled Packet Timing Follow-Up

Analyzed the retained capture over epoch seconds `1789067667` through `1789067727`.
Sort by packet timestamp: the multi-interface capture's record order is not globally chronological.
All 60 full-MTU requests have replies in this interval.

| Candidate timing, milliseconds | Median | 95th percentile | Maximum |
| --- | --- | --- | --- |
| TUN request to next outgoing UDP datagram of at least 1,345 bytes | 1.268 | 1.909 | 600.817 |
| Last incoming UDP datagram of at least 1,345 bytes to TUN reply | 1.548 | 2.054 | 2.410 |
| Matched ICMP request/reply RTT at TUN | 134.745 | 285.561 | 683.671 |

These are temporal candidates, not decrypted payload attribution. Large probes can have
similar sizes; the underlay capture excludes stream transports. Percentiles use nearest rank.
The median uses the mean of the two central observations.

- Sequence seven entered `pv1` at `14:14:33.280917460` CDT. The next captured outgoing
  QUIC datagram was at `.881734929`, followed by its candidate return at `.963183421`.
- Its TUN reply arrived at `.964588486`. The other 59 request-to-outgoing candidates
  were below 3 ms; every return-to-TUN candidate was below 3 ms.
- Precise system journal time places `control_capabilities_accepted` at `.880948`,
  less than 1 ms before that outgoing datagram. The preceding logged event was at `.174373`.

`handle_control_response_event` validates membership and hostname records synchronously
inside the runtime event loop. This is a profiling target, not proof that validation
occupied the entire gap; the log does not record handler entry or time spent per stage.

Next: measure control-handler duration and packet dispatch independently of QUIC queueing.
Both physical artifacts were debug builds, so account for optimization before attributing
timing to release behavior. Do not weaken authorization or probe deadlines based on this correlation.

### Duplicate Membership Merge Diagnostic

Extended `measure_forwarder_signed_membership_resources` to time three unchanged
membership merges after its existing construction/refresh checks. Each merge must
accept zero records, count every duplicate and preserve records and revision counters.

| Records | Three duplicate merges | Mean per merge | Evidence |
| --- | --- | --- | --- |
| 8 | 1,029,053 microseconds | 343.018 ms | Initial diagnostic |
| 8 | 1,025,475 microseconds | 341.825 ms | Fresh-process repeat |
| 128 | 16,247,999 microseconds | 5,416.000 ms | Fresh-process run without concurrent build |

- These are unoptimized Rust 1.97.1 host measurements, without a phone or network.
  They reproduce synchronous merge cost, not physical loss or release-build latency.
- An earlier 128-record run overlapped Clippy and took 16,779,426 microseconds for
  three merges. Exclude it from clean timing evidence; all its correctness assertions passed.
- Reproduce with `P2P_VPN_REVIEW_LEDGER_RECORDS=8` or `128`, running only
  `runtime::forward::tests::measure_forwarder_signed_membership_resources` with `--ignored --nocapture`.
- Required Clippy groups and Rust formatting passed. No production behavior changed;
  Android packaging, physical tests and full workspace tests were not rerun for this diagnostic.

Next implementation boundary: avoid repeated validation only for exact signed records
already held in validated forwarder state, inside the existing membership refresh window.
New, modified, expired or time-transitioning input must retain the full validation path.
Keep public merge helpers validating arbitrary caller-supplied histories.

Regression coverage must compare cached and full evaluation across expiry, future grants,
clock rollback, revocation, changed signatures, conflicting versions and pending authorization
updates. The diagnostic commit itself did not implement this optimization.

### Exact Duplicate Membership Fast Path

- `Forwarder::merge_membership_records` now recognizes exact retained signed records
  inside its existing time-valid authorization window, including empty and duplicate batches.
- It returns the same ignored-record count without signature revalidation, ledger cloning
  or authorization reconstruction. Record equality includes the signature and full payload.
- New or altered records, activation/expiry boundaries and clock rollback use the original
  full merge. Public membership helpers and persisted-state restoration remain unchanged.
- No additional cache or unbounded allocation was introduced. Existing record limits bound
  retained-state comparison; pending authorization updates are not consumed by this fast path.

The differential regression exercises future grants, a future revocation, expiry, repeated
timestamps and rollback against full evaluation. Negative coverage checks changed signatures
and changed payloads; duplicate batches preserve revisions and pending authorization changes.

| Unoptimized host diagnostic | Before, three duplicate merges | After, three duplicate merges |
| --- | --- | --- |
| Eight records, same fixture fingerprint | 1,025,475 microseconds | 5 microseconds |
| 128 records, same fixture fingerprint | 16,247,999 microseconds | 414 microseconds |

No build ran during either post-change diagnostic. These measure the duplicate fast path,
not new-record validation, hostname handling, throughput or end-to-end network latency.

- Workspace/all-target tests passed: 1,529 passed, 46 ignored, zero failures.
  Core tests took 47.77 seconds; evidence: `/tmp/p2p-vpn-duplicate-merge-workspace.log`.
- Both resource diagnostic sizes passed their correctness assertions. Formatting and
  whitespace checks passed; no Lean/TLA/Lake sources were found in the repository scan.

- Required Clippy groups passed in 16.64 seconds; existing non-fatal warnings remain.
  Evidence: `/tmp/p2p-vpn-duplicate-merge-clippy.log`.
- Cached offline Android ARM64 compilation passed in 40.01 seconds with four existing
  platform dead-code warnings; no Java or JNI contract changed.
- Offline Nix evaluation produced `/nix/store/wmw28ms4m8pxxabcvfmylzz0yil083lp-source`;
  its Rust `src` tree matches the tested worktree. This is not full Nix package realization.

Controlled physical QUIC remeasurement remains pending for this implementation.
This does not claim to resolve the earlier physical packet loss.

### Fast-Path APK Deployment

Deployed source `fb47a4cc` to the authorized OnePlus over USB. Updated in place and
reconnected its existing network; peer ID and `oneplus-9-pro` hostname were preserved.
Wi-Fi remained selected with zero reported underlay changes, losses or recoveries.

| Artifact | SHA-256 |
| --- | --- |
| Installed debug APK | `54181ee01fd13df9b8b7d61f726e2199ee3479e8314437e45c50c7b4c7e530e9` |
| ARM64 native library | `63d305306b330bda584ce386d599de7b58391b652eb0970bdd09e6b746e6b5b2` |
| Prepared Linux binary, not deployed this round | `cc35fbbd3916fb49280409569b47f561b616649be7dc97d61ebea85bc7d8d048` |

- Cached APK assembly passed in seven seconds; JVM tests were up-to-date, not freshly rerun.
  Native ELF segments retain 16 KiB alignment; APK `zipalign -c -P 16 4` passed.
- The Nix-managed Linux service remained process `2465207`, with no runtime drop-ins.
  No Linux configuration, pairing, firewall rule or underlay was changed.

| Upgrade smoke test, 1,280-byte IPv4, DF | Replies | Mean RTT |
| --- | --- | --- |
| Laptop to updated phone | 10/10 | 127.494 ms |
| Updated phone to laptop | 10/10 | 71.334 ms |

Android owned-UDP payloads increased from zero to 20; owned-QUIC remained zero.
An established QUIC session is not proof it carried these packets. This verifies the
upgrade's basic connectivity against the unchanged Linux service, not QUIC preference or recovery.

All build/install/ping commands finished; temporary storage measured 9,783,716 KiB.
Next: temporarily deploy the prepared Linux binary and repeat the captured QUIC block/recovery
scenario without changing endpoints or manually rescuing either runtime during measurement.

### Fast-Path Physical Recovery Repeat

Both sides ran `fb47a4cc` artifacts listed above. The laptop temporarily used the new
binary with its existing minimal config; the phone retained its identity, profile and Wi-Fi.
No runtime restart or manual route/endpoint change occurred during measurement.

| Full-MTU IPv4 stage, DF | Replies | Mean RTT |
| --- | --- | --- |
| Baseline laptop to phone | 10/10 | 99.306 ms |
| QUIC block transition | 30/45 | 142.042 ms |
| Phone to laptop while blocked | 5/5 | 64.557 ms |
| After unblock | 41/45 | 93.558 ms |
| Settled laptop to phone | 60/60 | 121.002 ms |
| Settled phone to laptop | 30/30 | 66.302 ms |

- Blocked phone QUIC endpoint `192.168.0.229:48766` from 14:43:43.110 to 14:44:32.757 CDT
  on September 10. The dedicated chain counted 42 drops; UDP endpoint `48146` remained available.
- Baseline Linux owned-QUIC payloads grew from zero to ten, while Android still used UDP.
  During settled checks, Linux QUIC/UDP counters changed `44/60` to `134/60`.
- Android settled snapshots changed QUIC/UDP `14/89` to `102/89`. These asynchronous
  snapshots show 88 additional QUIC payloads, not an exact count of all 90 settled exchanges.
- Linux process `2491626` stayed unchanged. Android native logs retained process `10311`;
  final status reported generation one and zero underlay selection changes/losses/recoveries.

#### Remaining Loss Evidence

Post-unblock sequences `7`, `17`, `22` and `41` lacked replies. Their candidate outgoing
datagrams were captured within 3 ms of TUN ingress: UDP for the first three, QUIC for the last.
This does not support attributing these four losses to a long laptop dispatch stall.

- No matching-size return datagram appeared within the following 500 ms for those requests.
  Encrypted outer packets remain temporal candidates, not decrypted packet attribution.
- All 60 settled request-to-outgoing candidates were below 3 ms: median 1.319 ms,
  nearest-rank 95th percentile 1.973 ms and maximum 2.802 ms.
- Neither isolated timing improvements nor settled success explain the four lost replies.
  Next: distinguish phone receive/TUN handling from ordinary underlay loss before changing routing.

#### Evidence And Cleanup

- `/tmp/p2p-vpn-fastpath-physical.45xpVo/` contains the capture, scoped Android logs,
  precise Linux journal, stage pings, timestamps, endpoint snapshots and payload counters.
- Capture: 969 underlay packets and 371 TUN packets, zero reported capture drops.
  It covers all test stages; capture/logcat processes exited after measurement.
- The initial attempt, `.h2nklI`, stopped before traffic because its harness waited for QUIC
  but not UDP readiness. Retained evidence; corrected the harness within the same 60-second bound.
- Original Nix service restored as process `2495372`, with no drop-ins. Dedicated firewall
  chain absent; timer inactive/unloaded. Five post-restoration pings succeeded.
- Temporary storage: 9,784,796 KiB. The optimized APK remains installed on the OnePlus.
  The bounded host-specific script is `/tmp/p2p-vpn-fastpath-physical.sh`.

This verifies another autonomous fallback/re-promotion cycle and bounded settling, with
documented recovery-window loss. Physical movement and the earlier loss investigation remain open.

## Failed Datagram Fallback Review

- Inspection found that a failed QUIC send could try UDP, then immediately drop on UDP failure
  without checking a healthy stream. UDP failure also demoted the original QUIC path.
- The queue-drain regression reproduces a missing QUIC runtime and a stale UDP session MTU
  below the queued packet size. Before the fix, the stream-dispatch assertion failed: zero versus one.
- Fallback now checks supported streams after UDP failure and demotes the backend that failed.
  If no stream exists, the packet is dropped once with the UDP error's classification.
- The regression covers stream-available and no-stream cases, payload/drop counters and in-flight
  dispatch accounting. It does not prove remote stream delivery or identify the Pixel failure cause.
- No wire format, authorization, configuration, probe deadline or transport preference changed.
  No physical deployment was performed for this follow-up fix.

- Core-library suite: 1,167 passed, eight ignored, zero failures in 46.51 seconds.
- Required Clippy correctness, suspicious and perf groups passed in 16.45 seconds;
  existing non-fatal lint warnings remain. Rustfmt and whitespace checks passed.
- This follow-up did not rerun full workspace, privileged namespace, Nix realization or Android
  builds. Their earlier evidence predates this fix; physical fallback validation remains required.

No existing Lean/TLA+ model was found; this asynchronous fallback change has executable coverage only.

### Android Build Follow-Up

- Source revision `26d716b6`: cached offline ARM64 native build passed in 39.12 seconds,
  with four existing platform-specific dead-code warnings. Two Cargo jobs; no downloads.
- Debug APK assembly passed in seven seconds; JVM tests were up-to-date, not freshly executed.
  `zipalign -c -P 16 4` passed. Neither phone nor Linux service was redeployed.
- Nix-evaluated ARM64 source `/nix/store/zq7kfqd9r650nrq04ycmrsja9dsxxz0s-source`
  matched working-tree `src` and `crates` recursively. This is not a full Nix realization.

| Prepared artifact | SHA-256 |
| --- | --- |
| Debug APK | `1d71d2cad8c4c1cfaaa532589461368a69c72574c00fbdfe929df470d963e6f7` |
| ARM64 native library | `092639d66a412036f86b7027353931fe4e5fda1280b421a9110c6ea3442e46f7` |

Physical fault injection remains pending authorization: upgrade the OnePlus, temporarily
run the laptop test binary, block only their owned QUIC packet traffic, then remove the
block and observe autonomous recovery. Restore the Nix-managed service afterward.

### Workspace And Recovery Follow-Up

- Current fallback-fix workspace run: 1,529 passed, 46 ignored, zero failures.
  Command: `cargo test --offline --locked --workspace --all-targets -- --test-threads=2 --quiet`.
- Separately ran ignored `tun_namespace_minimal_quic_recovers_after_packet_block`:
  passed in 74.75 seconds, with the existing deadlines and a 250-second outer watchdog.
- Blocked QUIC delivered five small and five 1,280-byte IPv4 packets through UDP fallback.
  After removing the block, autonomous QUIC promotion delivered all ten packets again.
- The fixture asserted backend-specific payload-counter growth at both stages.
  No physical service, phone, firewall or underlay was changed; all namespace processes exited.
- Retained evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.494ccd7159c2147b`;
  workspace and scenario logs are `/tmp/p2p-vpn-fallback-chain-{workspace,namespace}.log`.
- Harness SHA-256: `709a490f175b0ad84e60b86751e37f62be69f18265706482330b17da4b01b2e1`.
  Storage during validation measured 9,780,592 KiB, below 10 GiB.

These checks cover the shared fallback fix, not physical Android recovery or the unresolved
Pixel loss. Physical fault injection still requires the requested explicit authorization.

## Stream-Only Compatibility Check

- Extend the isolated minimal-config mDNS fixture with one explicitly stream-only peer.
- Leave the other peer's datagram defaults enabled; supply no peer endpoints or routes.
- Require five delivered pings and at least five direct-stream payloads per side.
- Require zero datagram and relay payload counts; retain existing discovery and timeout assertions.
- Run helper tests and the ignored namespace scenario with cached tools and bounded storage.
- This verifies current stream-only configuration compatibility, not an archived binary release.

| Scenario | Result | Evidence directory under `/tmp/` |
| --- | --- | --- |
| Stream-only, strict five-reply assertion | Passed, 7.27 seconds | `p2p-vpn-mdns-tun-e2e-1.d5d5b552096ba4ea` |
| Minimal automatic QUIC regression | Passed, 11.99 seconds | `p2p-vpn-mdns-tun-e2e-1.e343901827b88dcd` |
| Minimal UDP-only compatibility regression | Passed, 11.08 seconds | `p2p-vpn-mdns-tun-e2e-1.2d690ec9c581005a` |

The strict stream-only run saved ping output and daemon snapshots before shutdown.
Both peers submitted at least five direct-stream payloads, with zero datagram or relay payloads.
The run used TCP streams; it does not independently certify QUIC stream-only behavior.

- Namespace helpers: 60 passed, 32 ignored, zero failed.
- Cached Clippy correctness/suspicious/perf gates passed; nonfatal warnings remain.
- Rustfmt and whitespace checks passed. No production code or device configuration changed.
- Final harness SHA-256: `cd3c041004f256a82261591eb2dd5f6e71d8233900f5146757378ee57ad4b6b7`.
- QUIC/UDP regression runs preceded the final stream-only assertion strengthening.

## Cached Nix Source-Parity Audit

Audited at application revision `a222b56f`, without building derivations or activating a host.

| Source | Store path | Result |
| --- | --- | --- |
| Desktop Rust package | `/nix/store/nif86ggk4wnxzixr0d9yn6479ajq1pyh-source` | Rust source, crates, vendors and lockfile match |
| Android ARM64 and x86_64 native | `/nix/store/j31i509jj64pczng994298736bmv305j-source` | Rust source, crates and vendors match |
| Android APK source | `/nix/store/vzkl8860h3rq6q731y6w1jldd8i7j2ij-android` | `app/src` matches, including backend counter changes |

- Cached Cargo metadata reports identical test targets for worktree and desktop package source.
- Rust test file lists and contents match; non-Rust test assets are excluded by the declared fileset.
- NixOS consumer evaluation: 17 contracts true; QUIC-default evaluation: nine contracts true.
- Six-instance firewall output includes QUIC UDP ports 52820 through 52825.
- Repository search found no Lean, TLA+, Alloy or Lake model files.

The offline `rust-test-sources` build plan requires 704 derivations and was not executed.
The comparisons above used cached host tools against Nix-evaluated source paths.
They do not certify full package builds, Android Nix cross-builds or system activation.

## QUIC Datagram Size Boundary

- Test a real loopback QUIC receiver advertising a 1,200-byte UDP payload limit.
- Negotiate a 1,280-byte overlay MTU; require oversized submission to fail explicitly.
- Verify a later small authenticated packet still arrives on the same session.
- Classify Quinn `TooLarge` as a size drop, not a missing transport peer; test other errors unchanged.
- Keep fallback, demotion and timeout policy unchanged; this is not a physical-loss root-cause claim.

The loopback test confirms that negotiated overlay MTU can exceed Quinn's datagram allowance.
An oversized submission fails with `SendDatagramError::TooLarge`; a subsequent small frame arrives
without reconnecting or replacing the authenticated packet session.

The diagnostic regression failed before the fix: actual `NoTransportPeer`, expected `PacketTooLarge`.
The fix changes only the final-drop category for this error. It does not change path selection,
fallback, retry deadlines or MTU discovery and does not resolve the physical stability finding.

| Validation | Result |
| --- | --- |
| Rust library suite | 1,166 passed, eight ignored, zero failed; 46.77 seconds |
| Required Clippy groups, library and tests | Passed; existing nonfatal warnings remain |
| Android ARM64 native compilation | Passed using cached toolchain; 39.02 seconds |
| Debug APK assembly | Passed; JVM tests reused their previously passing cached results |
| APK alignment and formatting | Passed |
| Temporary storage before APK repack | 9,778,824 KiB, below 10 GiB |

Latest built APK SHA-256: `cf0921ff4e015d86d6a94c24c5d339974642017adfe25d01e84db44d98ce7cac`.
Native library SHA-256: `a4f115c5d18320d470cb865c72fadbc6188ade7db9deef92effe25112d26f02e`.
Neither artifact was deployed. Full Nix realization and physical MTU recovery remain unverified.

## Historical Implementation Progress

The chronological notes below preserve earlier results and failures. Their pending-work statements
describe those stages; the acceptance audit above records the current status.

- Shared defaults now request an ephemeral QUIC listener alongside UDP.
- Deserialization distinguishes omitted QUIC settings from an explicit empty list.
- Legacy `listen: []` without a QUIC override remains stream-only.
- Serialization retains explicit QUIC disables; entirely default packet settings remain omitted.
- Configuration tests: 60 passed, including omission/override/serialization regressions.
- Library tests: 1,154 passed, 8 ignored, zero failures (46.09 seconds, two test threads).
- Formatting and whitespace checks passed; no Lean/TLA+/Alloy files found.
- NixOS allocates QUIC ports from 52820, includes them in collision checks and opens the derived firewall ports.
- Consumer evaluation passes 17 contracts; QUIC module evaluation passes nine contracts.
- Six-instance module fixture UDP ports match the strengthened expected list.
- Runtime integration, workspace, Clippy and Android validation remain pending.
- No deployments or personal configuration changes have been made for this goal.

Local test build used cached Cargo 1.96.0 with Rust 1.97.1, offline/locked,
two build jobs, no debug symbols or incremental state. Test executable:
`/tmp/p2p-vpn-review-target/debug/deps/p2p_vpn-3e67c2326d773a12`.

```sh
cargo test --offline --locked --lib config::tests::
timeout 180 /tmp/p2p-vpn-review-target/debug/deps/p2p_vpn-3e67c2326d773a12 \
  --test-threads=2 --quiet
```

The build emitted two existing `doc_cfg` warnings from vendored `stats_alloc`.
Full temporary storage accounting after tests: 9,386,480 KiB, below 10 GiB.

Nix checks ran offline by evaluating derivation contents; no system build or activation:

```sh
nix eval --offline .#checks.x86_64-linux.nixos-consumer-flake-eval.text
nix eval --offline .#checks.x86_64-linux.nixos-quic-defaults-eval.text
nix eval --offline --raw .#checks.x86_64-linux.nixos-module.udpPorts
```

## Minimal LAN Packet Test

`tun_namespace_minimal_config_prefers_quic_datagrams` uses two isolated network
namespaces, generated identities and peer IDs. No packet listener, endpoint,
bootstrap address or route is supplied. Discovery is restricted to mDNS for isolation.

The existing UDP fixture remains explicitly UDP-only; the new test independently
deserializes minimal configuration. Both require real TUN traffic.

| Attempt | Result | Evidence |
| --- | --- | --- |
| Before direct-path advertisement fix | Failed after 74.50 seconds waiting for QUIC selection | Both QUIC listeners bound, but capabilities advertised zero QUIC endpoints; UDP remained selected |
| After fix | Passed in 12.03 seconds | Both selected QUIC datagrams; overlay ping passed; each log recorded increasing QUIC payload counts through at least four |

The direct-connection helper previously advertised only UDP packet endpoints.
It now also advertises the QUIC listener's route-derived address and certificate,
using the existing relay and overlay-address exclusion guards.

No deadline, assertion or underlay was changed between attempts. No runtime was
manually rescued. The passing artifact includes the explicit UDP fixture override.

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 timeout 100 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-0e281f928ed88e72 \
  --ignored --exact tun_namespace_minimal_config_prefers_quic_datagrams --nocapture
```

| Artifact | SHA-256 |
| --- | --- |
| Passing test executable | `d2da95110d2a8fa3421f19289afce2bcc4dd10799f770b1110e236a89c7bc3ed` |
| Failed node A log | `0f79fc5803a8b1905676458490bf2f838bed6646071cb2629db1988296159eea` |
| Failed node B log | `afcb012f5c120b835b8b4bdff38302d52e019f9bfee334c61a8487f2340d9b87` |
| Passing node A log | `423b202288c3bd891cc9f8aa56a168bb1e48a7a5d755b35b0ee80e90da2ba49a` |
| Passing node B log | `3c78f70646a5293fb570621b98b96b5578f2e9f8030f441ebe11151ae68ececb` |

Evidence directories under `/tmp/`:

- Failure: `p2p-vpn-mdns-tun-e2e-1.ca9f98bf52cc62c9`.
- Pass: `p2p-vpn-mdns-tun-e2e-1.f457ec297d327cc4`.

This single LAN result does not establish blocked-QUIC fallback, movement,
public NAT reachability, Android behavior or sustained resource bounds.

## Capability Compatibility Follow-Up

The direct QUIC advertisement now uses the existing capability builder, keeping
the advertised preference consistent with its certificate and support flags.

| Check | Result |
| --- | --- |
| Advertisement unit regression | Passed: certificate, preference, deduplication, relay exclusion, overlay exclusion and disabled backend |
| Minimal config with UDP-only peer | Passed in 11.03 seconds; automatic UDP endpoint discovery and overlay payload traffic |
| Minimal QUIC repeat on same build | Passed in 12.09 seconds; QUIC selection and payload traffic on both nodes |

The UDP-only peer is a current runtime with its QUIC listener explicitly disabled,
not an archived release binary. It verifies capability fallback, not full old-release compatibility.

Both namespace scenarios use the existing 100-second outer watchdog and unchanged
internal deadlines. No test processes remained afterward.

| Artifact | SHA-256 |
| --- | --- |
| Follow-up namespace executable | `dc208b85b76394c89388de530cc27d8bacfde50777aea3490c2ac069da07a8e1` |
| UDP-only node A log | `19e7af71c94a05426b79c718154d3ae6af73face7c7bb5a33b9bf302693ae9be` |
| UDP-only node B log | `eae2ed649df92dcce1169f3bb3b538f22a896fad846bc0a5340811ce4fc91920` |
| QUIC repeat node A log | `709c8c272c50da6503222619d14de1c85cf9b9c2ffa4f2c5e01713e127d8e7ee` |
| QUIC repeat node B log | `5679bed7ba87bb58858cc5140054d8e526544a036107006d013d7810088f7bc6` |

Directories: `/tmp/p2p-vpn-mdns-tun-e2e-1.894702b35cfc5738` (UDP-only),
`/tmp/p2p-vpn-mdns-tun-e2e-1.f289087954c141af` (QUIC repeat).

The first follow-up build rejected test-only `IpCidr` string parsing. The test now
uses the existing constructor; the rebuilt unit and integration checks passed.

## Blocked QUIC Regression

`tun_namespace_minimal_quic_recovers_after_packet_block` establishes automatic
QUIC traffic, drops the two QUIC packet listener ports inside disposable namespaces,
requires UDP fallback, then removes the block and requires QUIC payload recovery.

| Bound | Value |
| --- | --- |
| Orchestrator | 240 seconds, fixed before first execution |
| Outer watchdog | 250 seconds |
| Path transition | Existing 60-second wait per node |
| Traffic checks | Five of five replies per stage; post-restore QUIC counter increase |
| Intervention | Namespace firewall changes only; no daemon restart or reconfiguration |

First run failed in 72.94 seconds while waiting for UDP selection on node A.
QUIC probe timeouts demoted the path, but no UDP session formed. A healthy TCP
path remained; the test stopped before testing fallback traffic or restoration.

Source findings from the failed run:

- `packet_plane_negotiation_backend` prioritizes renewed QUIC negotiation over an absent UDP session.
- `packet_plane_accept_backend` selects QUIC from capabilities whenever both peers support it.
- Acceptance chooses that backend before matching the authenticated hello endpoint.
- A UDP-capable peer with blocked QUIC must be able to negotiate UDP without disabling QUIC support.

This is an unresolved in-scope failure, not evidence that all fallback is broken.
The unchanged test must pass after correction; its deadline is not a tuning target.

Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.00514e8351d97b8a`.
No test processes remained after failure; the isolated namespaces and their rules exited.

| Artifact | SHA-256 |
| --- | --- |
| Test executable | `7ed5c4a354ac14ec857ce8d89a27863ebea70e6ef3a6fe218ecd069440f6be7d` |
| Node A log | `569b54534c196981a6398bc897c188f9d60420f78f6892a86b5f16b4f7c7b9b0` |
| Node B log | `f4e9abd2bce44d2fa4a792ae9d2a1ba3947db68e0d5ad8150845a86048937e52` |

### Authenticated Backend Selection

Acceptance now verifies the signed hello before choosing its advertised backend.
A UDP endpoint can negotiate UDP while both peers continue advertising QUIC.
QUIC certificate requirements and endpoint ownership checks remain enforced.
Existing UDP sessions may renew even when QUIC capabilities are present.

| Check | Result |
| --- | --- |
| Packet-plane unit group | 76 passed |
| Full library | 1,157 passed, eight ignored; 46.47 seconds |
| Dual-capability UDP regression | Signed handshake, encrypted bidirectional traffic and overlapping renewal pass |
| Endpoint selection negatives | Unadvertised endpoint, missing QUIC certificate and disabled UDP support rejected |
| Minimal QUIC namespace repeat | Passed in 12.03 seconds with unchanged payload assertions |

Cached offline build used the same toolchain and resource limits as above.
Library executable SHA-256:
`8bc93c01f3939197ae90c0034aed78bd4f75faf61056e1fe2ab5a33c23aa881a`.

Namespace evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.e341dcdfee913b5a`.
Integration executable SHA-256:
`e2124bd8b3b47d6ca6e4a7dbc969ef594aa172f18afedae778dce6b31489fdea`.
No fixture processes remained after completion.

This fixes acceptance, not autonomous recovery. Remaining source findings:

- Probe timeout demotes the packet path and redials control addresses, without retrying packet negotiation.
- Session maintenance retries expiry/renewal, not every unhealthy packet path.
- Backend selection uses QUIC session presence before path health, hiding a concurrent UDP session.
- Periodic probing similarly chooses one datagram backend; independent fallback health needs coverage.

### Recovery Scheduling And Selection

The five-second path maintenance tick now retries packet negotiation with the
existing authorization, deterministic initiator and pending-handshake guards.
An absent UDP fallback is negotiated before retrying an established QUIC session.

Selection and diagnostic snapshots now consider datagram path health. Probing
visits each authenticated backend independently. A follow-up preserves existing
path scores and allows probes after demotion clears the established-path count.

| Check | Result |
| --- | --- |
| Focused packet tests before follow-up | 195 passed |
| Unchanged blocked-QUIC fixture before follow-up | Passed in 85.72 seconds |
| Blocked phase | Both peers selected UDP; five of five ping replies |
| Restored phase | QUIC session renegotiated autonomously; selected QUIC and five of five replies |
| Full library after score/probe follow-up | 1,158 passed, eight ignored; 46.43 seconds |
| Unchanged recovery fixture after follow-up | Passed in 70.84 seconds |

Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.001a4e8f454f10a2`.
Integration executable SHA-256:
`63695efa3926c256005d4ed10b5f4824db4f4f84effbdc8104b3063cd4071271`.

Follow-up evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.21f627ba4c23436b`.
Integration executable SHA-256:
`3e3121d55108c9ef9fc40429dc05c07d1db7da3142f40fbfe252b68c59b57613`.

**Evidence correction:** `outbound_quic_datagram_packets` currently increments for
both owned UDP and QUIC payload sends. Prior counter claims in this report prove
aggregate datagram traffic, not QUIC-specific traffic. Add backend-specific
counters and strengthen the fixtures before accepting QUIC payload certification.

Remaining recovery work includes QUIC blocked from startup, bounded retry behavior,
endpoint movement and independent backend payload accounting. A passing transition
fixture does not satisfy these additional requirements.

### Backend Payload Counters

Added `outbound_owned_quic_datagram_packets` and `outbound_owned_udp_datagram_packets`.
They count successful payload submission to the named backend, excluding probes.
The legacy aggregate retains its meaning. Delivery still requires receiver or
round-trip evidence; successful submission alone is insufficient.

The recovery fixture now requires five backend-specific payload sends from each
node for UDP fallback and QUIC restoration. Minimal transport evidence also requires
the named counter; the aggregate or inbound traffic cannot substitute for it.

The first strengthened minimal-QUIC run failed after 20.62 seconds because the
control-state formatter omitted the new counters. Logs showed five QUIC sends,
zero UDP sends. The formatter and its regression coverage have been corrected;
the rebuilt artifact passes the full library and minimal transport checks below.

Failed evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.d33d3e2130c767f5`.
Executable SHA-256:
`324c326c8b4cc6220920ccf2494904a4ccce4c9c8d4c99f18dc62d1ca7071015`.

| Strengthened check | Result |
| --- | --- |
| Full library | 1,159 passed, eight ignored; 46.43 seconds |
| Minimal QUIC | Passed in 12.04 seconds, backend-specific sends on both peers |
| UDP-only capability peer | Passed in 11.08 seconds, backend-specific UDP sends |
| QUIC block and restore | Passed in 70.88 seconds; both peers sent five UDP payloads during fallback and five QUIC payloads after restoration |

Passing executable SHA-256:
`ca382092b821ad624aa966f7949757057e570bf114f220aab19d8046b785317a`.
Evidence directories under `/tmp/`:

- QUIC: `p2p-vpn-mdns-tun-e2e-1.e8b63a1247f3cc4f`.
- UDP-only: `p2p-vpn-mdns-tun-e2e-1.c8dda59e02868b98`.
- Recovery: `p2p-vpn-mdns-tun-e2e-1.3137f28009068966`.

### QUIC Blocked From Startup

`tun_namespace_minimal_quic_blocked_from_startup_recovers` blocks node B's QUIC
packet port before node A starts. Both use minimal configuration and isolated
mDNS discovery; neither can establish an owned QUIC session before the block lifts.

- Require UDP selection within the existing 60-second per-node bound.
- Require zero QUIC sessions/payloads and five UDP payload sends per node during the block.
- Remove only the fixture firewall rule; require QUIC selection and five payload sends per node.
- Keep the existing 240-second orchestrator and 250-second outer watchdog.

The first run failed after 62.56 seconds: repeated QUIC connection attempts
prevented any UDP session. TCP control remained healthy. Evidence:
`/tmp/p2p-vpn-mdns-tun-e2e-1.baf13dfa2d768cb9`.
Executable SHA-256:
`5ff5bed3a4aa406bd6ff407fee171dcddb0557429d489217b1571438ce3f96dd`.

The fix schedules QUIC attempts at least 30 seconds apart, permitting
UDP negotiation between attempts. Cancellation preserves this retry deadline;
peer removal and network reset clear it. Authorization reconciliation includes
retry-only peers.

| Check | Result |
| --- | --- |
| Full library | 1,160 passed, eight ignored; 46.34 seconds |
| Non-privileged namespace harness | 60 passed, 30 ignored |
| Startup-blocked recovery | Passed in 50.28 seconds with unchanged deadlines and backend-specific payload assertions |
| Established-session block/restore repeat | Passed in 70.85 seconds with backend-specific payload assertions |

Passing evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.5365daaa3128b8bc`.
Executable SHA-256:
`cd6b2b97e88ae50f0ca9708508cabd73602227c90f2a87272e2e5e6f9e9732a2`.

Established-session repeat evidence:
`/tmp/p2p-vpn-mdns-tun-e2e-1.07e3cdbc15b75635` (same executable).
Both fixtures exited without remaining owned test processes.

### LAN Address Change And Return

`tun_namespace_minimal_quic_follows_lan_address_change` moves node B from
`10.250.0.2` to `.3`, then back to `.2`, on the existing namespace interface.
It changes no VPN configuration and sends no runtime network-change notification.

- Require the remote QUIC session to identify the changed endpoint within 60 seconds.
- Require QUIC selection and five successful pings plus five QUIC sends per peer at each stage.
- Keep both daemon processes running; use the 240/250-second fixture watchdogs.

First run failed after 126.67 seconds on return. The move to `.3` passed endpoint,
selection, ping and payload assertions. On return, capabilities correctly advertised
`.2` and UDP recovered, but no QUIC session existed at the deadline.

Logs include expired UDP handshakes and a failed QUIC connection attempt before
another QUIC attempt. Investigate negotiation/session replacement and control-path
selection; this is not evidence of a missing return-address advertisement.

Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.810228d73f43f0c7`.
Executable SHA-256:
`2cd3506f2d9a6c30073df1ed486055639ab5da724bef2203f1ae336f3deeff5f`.

Pixel `3C161FDJG00013` reconnected during this test. Read-only ADB confirmed the
Pixel 8 Pro and running `org.hermeticfoundation.p2pvpn.debug` process. No APK,
pairing, profile or underlay changes were made to the phone.

### Terminal Handshake Cleanup

The failed movement trace showed control-request timeouts followed much later by
packet-hello expiry. Terminal transport failures did not retire their corresponding
packet negotiations; stale-connection replies had the same cleanup gap.

Cleanup now matches both peer and request ID, removes only that initiator's pending
handshake and aborts its connection task. It preserves QUIC retry backoff and does
not accept stale responses or change connection-deduplication policy.

The owner-matching regression and full library pass: 1,161 tests passed, eight
ignored (46.69 seconds). The movement fixture is being repeated without changing
deadlines or injecting runtime notifications.

## Baseline Findings

| Surface | Current behavior | Required change |
| --- | --- | --- |
| Rust defaults | UDP binds `0.0.0.0:0`; QUIC listener list is empty | Automatically enable QUIC alongside compatible fallback |
| Path ranking | QUIC datagram 100, UDP 95, QUIC stream 75, TCP 40, relay 30 | Preserve datagram preference and health gating |
| NixOS | Assigns UDP packet ports from 51820; only explicit QUIC listeners open firewall ports | Allocate collision-checked QUIC listeners and firewall rules automatically |
| Discovery | Observed endpoint updates/withdrawals already accept both packet backends | Verify wildcard listeners, address changes and reachable candidate validation |
| Capabilities | QUIC certificate and endpoint candidates gate owned-QUIC support | Advertise only usable capability state; preserve old-peer fallback |
| Android | Profiles use shared config defaults; saved profile is encrypted | Apply defaults without rewriting identity or pairing state |

Existing source anchors:

- `src/config.rs`: `PacketPlaneConfig`, packet-listener parsing and defaults.
- `src/lib.rs`: `PathKind::default_score`.
- `src/runtime/runner.rs`: startup capabilities, observed endpoints and withdrawal.
- `nix/nixos-module.nix`: effective packet listeners, rendering and firewall ports.
- `crates/p2p-vpn-android/src/lib.rs`: shared runtime and explicit test overrides.

## Compatibility Rules

| Input | Intended behavior |
| --- | --- |
| Minimal config | Automatic QUIC datagrams with UDP and streams retained |
| Explicit `quic_listen: []` | QUIC packet listener remains disabled |
| Explicit nonempty QUIC listeners | Preserve requested bind addresses |
| Existing `listen: []`, no QUIC override | Preserve documented stream-only intent |
| UDP-only older peer | Negotiate UDP or streams without requiring peer upgrade |
| Persisted Android identity/profile | Preserve membership, addresses and enabled state |

Distinguish omission from explicit disable during deserialization and serialization.
Do not omit an explicit disable when rendering or round-tripping configuration.

## Implementation Sequence

1. Add configuration regression tests for omission, explicit disable, overrides and round trips.
2. Implement automatic listeners; retain existing fallback and stream-only semantics.
3. Integrate NixOS port allocation, collision checks, configuration and firewall tests.
4. Verify candidate publication, authenticated negotiation, withdrawal and recovery.
5. Exercise packet selection and fallbacks with minimal configurations in local fixtures.
6. Build Android ARM64 and validate new/existing-profile behavior.
7. Obtain deployment authorization, then collect physical payload and recovery evidence.
8. Reconcile user documentation and perform the full acceptance audit.

## Evidence Matrix

### QUIC Migration Diagnostic Review

- A focused client-socket rebind test reproduced stale receive-address metadata.
  The multi-peer receive loop sampled the remote address before awaiting a datagram.
- Sample the live connection address after receipt, as the single-peer path does.
  Keep signed handshake endpoints unchanged; migration does not require new identity keys.
- The focused regression failed before the fix and passed afterward (0.03 seconds).
  It verifies authenticated delivery and a changed live address with an unchanged handshake endpoint.
- Movement fixtures must distinguish handshake endpoints from live connection endpoints.
  Preserve payload-growth and delivery assertions and the previous failed return-path evidence.
- Added `packet_plane_quic_connection` diagnostics for live session connection endpoints.
  Existing `packet_plane_quic_session` output retains its signed handshake metadata.
- The movement fixture passed in 24.87 seconds using live endpoints: five of five pings
  and at least five QUIC payload submissions per peer after both movement and return.
- Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.aeadf1e8e7995066`.
  Harness SHA-256: `8546a22924df12695d28e0eb0d4a02d85649fa7d4e3c58f58b18760e2079d5d2`.
- This randomized run does not establish all TCP/packet initiator-role combinations.
  Deterministic role coverage and the earlier failed return-path case remain open.

### Movement Initiator Roles

Set `P2P_VPN_MOVEMENT_INITIATORS` when running
`tun_namespace_minimal_quic_follows_lan_address_change`.

| Value | Preferred TCP initiator | Packet negotiation initiator |
| --- | --- | --- |
| `aa` | Stationary A | Stationary A |
| `ab` (default) | Stationary A | Moving B |
| `ba` | Moving B | Stationary A |
| `bb` | Moving B | Moving B |

- Identities are generated with the requested ordering before daemons start.
  Generation is bounded to 256 attempts; invalid selectors fail immediately.
- No path, endpoint, connection or runtime state is overridden to obtain these roles.
  Each run retains the same address-movement, return and payload assertions.
- Controlled `ba` failed LAN return after 111.78 seconds total; first movement passed.
  Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.e7dbf3eb88c4ad99`.
- Harness SHA-256: `3755479962611a67176df14869fd131d33df1d6477f8b8a6740a08e00f380a98`.
  UDP remained healthy, but the final snapshot contained no QUIC session.
- B advertised its returned `.2` endpoint; A retained `.3` capabilities after control timeouts.
  Subsequent signed packet accepts were rejected with `endpoint_not_advertised`.
- Investigate capability-refresh retry across connection replacement; retain endpoint authorization.
  The diagnostic correction alone does not resolve this control-plane recovery defect.
- Recovery now refreshes capabilities at most once per ten seconds for authorized direct peers
  with negotiated QUIC support but no healthy QUIC packet path. Healthy QUIC stops these refreshes.
- Retry deadlines survive individual packet-request cancellation and are cleared on peer removal
  or runtime reset. The focused rate-limit and cleanup regression passes.
- The refreshed `ba` run still failed on first movement after 72.94 seconds total.
  Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.84c4797737f827e2`.
- Final capabilities now contain the correct `.3` endpoint; UDP is healthy, QUIC sessions are zero.
  Capability refresh alone is insufficient; inspect session retirement and negotiation ordering next.
- Timed-out control requests can now retire their old direct connection when a newer, current,
  same-peer and same-transport connection exists. Last connections and ordinary deduplication are unchanged.
- The focused retirement predicate regression passes. Controlled `ba` now passes in 85.81 seconds,
  with five of five pings and QUIC payload growth after both movement and return.
- Evidence: `/tmp/p2p-vpn-mdns-tun-e2e-1.bde63c668c75c84f`.
  Logs show retirement of stalled TCP connections 2 and 3 before replacement QUIC sessions establish.
- Harness SHA-256: `7e6412b6a80ad3112e316059212f201e79365ded78024739adb8f7afd353f223`.
  Library regressions: 1,164 passed, eight ignored, zero failures (46.41 seconds).

### Controlled Movement Results

All four role combinations passed on the harness hash above. Each requires five of five pings
and QUIC payload-counter growth on both peers after address change and after return.

| Roles | Total duration | Evidence directory under `/tmp/` |
| --- | --- | --- |
| `aa` | 100.81 s | `p2p-vpn-mdns-tun-e2e-1.def69510631c2737` |
| `ab` | 25.57 s | `p2p-vpn-mdns-tun-e2e-1.93433767de81058c` |
| `ba` | 85.81 s | `p2p-vpn-mdns-tun-e2e-1.bde63c668c75c84f` |
| `bb` | 25.62 s | `p2p-vpn-mdns-tun-e2e-1.c3aae3af8a20b7d8` |

- Total duration includes startup, both movement stages and traffic verification.
  Per-stage deadlines and delivery assertions were unchanged.
- These are local namespace results, not physical Android or WAN certification.
  Repeated stress, MTU-sized traffic and remaining transport regressions are still required.

### MTU-Sized Fallback Evidence

- Recovery traffic now includes five small pings and five IPv4 packets at the smaller selected
  path MTU of the two peers, with fragmentation prohibited. Each backend must submit ten payloads per peer.
- The tested path MTU was 1,280 bytes. Both UDP fallback and restored QUIC delivered every packet.
  This does not establish behavior over a smaller underlay PMTU or IPv6 MTU boundaries.

| Scenario | Duration | Evidence directory under `/tmp/` |
| --- | --- | --- |
| Established QUIC blocked, then restored | 74.99 s | `p2p-vpn-mdns-tun-e2e-1.c52f6daa076ec1f8` |
| QUIC blocked before discovery, then restored | 58.85 s | `p2p-vpn-mdns-tun-e2e-1.7dce33dab5121e6f` |

- Harness SHA-256: `e7c71141eb85fc0e66c1a951569822b2323070cbe0d05890450d44cde1631b3f`.
  Non-privileged harness tests: 60 passed, 31 ignored, zero failures.

### Android Default Verification

- Android profile creation and inspection use the shared configuration loader.
  No Android-specific transport-default implementation or profile rewrite was needed.
- New regressions verify omitted packet settings enable QUIC, reload preserves profile JSON,
  identity and hostname, and explicit QUIC-disable, stream-only and listener overrides survive reload.
- Host-executed Android bridge tests: 72 passed, zero failures (0.32 seconds).
  These tests do not exercise Android JNI or the physical VPN service.
- Cached offline ARM64 native build passed in 39.35 seconds, with four existing platform dead-code warnings.
  ELF load segments retain `0x4000` (16 KiB) alignment.
- Native library SHA-256: `3d48757b7b4bdd2d4f68f9cf8b9f952479bbd3d75e6c875cd85d87c926258cd5`.
  APK packaging, JVM verification and authorized device deployment remain outstanding.
- Debug APK packaging passed using the cached SDK and freshly built ARM64 library.
  SHA-256: `02ead2d9990d90a0d2c4ef914cdaac9923afdcfb50ce438d426f27d2fc6cdb4e`.
- Forced `:app:testDebugUnitTest` rerun passed: 22 tasks executed, nine seconds.
  The earlier up-to-date result is not the fresh-test evidence.
- APK `zipalign -c -P 16 4` passed. Pixel USB visibility was confirmed read-only.
  No deployment or profile mutation has occurred; fresh physical-test authorization is required.

### Current Module And Fixture Checks

- Offline NixOS consumer evaluation: all 17 contracts true.
  QUIC-default evaluation: all nine contracts true; module failed assertions: `[]`.
- These evaluate generated settings, defaults, overrides, collisions and firewall rules.
  They do not activate a NixOS system or substitute for the full Nix package checks.
- Android E2E fixture tests: ten passed, one private-bootstrap diagnostic ignored.
  Explicit stream-only fixture behavior remains intentional and validated.
- No Lean, TLA+, Alloy or Lake model files were found in the repository file scan.
  Clippy remains outstanding; cached Clippy uses Rust 1.95 rather than the current build compiler.
- Physical deployment remains pending explicit authorization; no device or personal-flake mutation.

### Clippy Verification

- Cached Rust 1.95 Clippy passed root-package `--all-targets` with correctness, suspicious and perf
  groups denied. Non-fatal warnings outside those groups remain; this is not a warning-free audit.
- The first attempt failed before project checks because the cached rustup LLD wrapper referenced
  a removed Nix store path. Retrying with `RUSTFLAGS='-C link-arg=-fuse-ld=bfd'` passed in 57.83 seconds.
- Checks used two jobs, offline dependencies, disabled incremental/debug data and the separate
  `/tmp/p2p-vpn-clippy-target`. No download or compiler replacement was performed.

### Workspace Regression Run

- `cargo test --offline --locked --workspace --all-targets -- --test-threads=2 --quiet` passed:
  1,526 passed, 45 ignored, zero failures, using the flake's `RUST_MIN_STACK=8388608` setting.
- Coverage includes the core library, CLI, authorization, pairing CLI, Kademlia resource tests,
  resource-measurement helpers, namespace helpers, Android bridge and Android E2E fixture.
- A preceding direct CLI invocation omitted the documented stack setting and aborted with stack
  overflow. The corrected run passed 158 CLI tests with one ignored; no test assertions were changed.
- Ignored tests are not covered by this workspace pass. Namespace scenario evidence above records
  the ignored integration tests executed separately; physical deployment is still pending approval.

### Relay Evidence Correction

- The old relay-overlay fixture passed in 16.94 seconds but delivered payloads over direct TCP.
  `/tmp/p2p-vpn-relay-tun-e2e-1.d12e9138cc4e387d` is not proof of relay payload delivery.
- The fixture now isolates both peer bridge ports while retaining reachability to the relay.
  It requires at least five relay payloads per peer and zero direct-stream or datagram payloads.
- Strengthened relay-only test passed in 16.99 seconds.
  Evidence: `/tmp/p2p-vpn-relay-tun-e2e-1.53e27883ea5c0e76`.
- Harness SHA-256: `abee3b84e1f0c61c8d2ca74d8b981d132ad896892aed435e613d04e53197e1ce`.
  Non-privileged harness regressions: 60 passed, 31 ignored, zero failures.

### Required Scenario Evidence

| Scenario | Required observation |
| --- | --- |
| Minimal direct peers | Successful traffic plus QUIC datagram payload-counter growth |
| Older UDP-only peer | Successful authorized UDP payload traffic |
| Explicit stream-only override | No packet listeners; working stream payload traffic |
| Blocked QUIC | Working fallback with bounded retries and queue memory |
| QUIC restored | Autonomous healthy-path promotion and QUIC payload traffic |
| Changed endpoint/network | Old session/path retired; new validated path carries traffic |
| LAN return | LAN discovery and local path recovery without manual rescue |
| Multiple networks | Independent identities, listeners, authorization and packet delivery |
| Android update | Profile/identity preserved; actual preferred-path payload traffic |

## Risks And Limits

- An observed public IP plus a local port is not proof of a usable NAT mapping.
- Libp2p QUIC connections are distinct from owned Quinn packet sessions.
- Established path counts alone do not prove packet selection or delivery.
- Default QUIC startup must not make usable fallback fail unnecessarily.
- Extra listeners must not collide across NixOS instances or explicit overrides.
- Existing emulator and transport results retain their original revisions and limits.
- The Pixel process-termination restart finding remains a separate issue.

## Verification And Resources

- Run affected Rust unit/integration tests, formatting and required Clippy groups.
- Inspect formal models; check NixOS rendering/firewall and cached source parity.
- Run Android ARM64 compilation and affected Java/lifecycle checks.
- Freeze bounded scenario windows before measurement; preserve failed evidence.
- Use cached tools, at most two Cargo jobs and downloads capped at 10 Mbps.
- Keep all `/tmp/p2p-vpn-*` below 10 GiB with full storage accounting.
- No physical deployment, personal-flake mutation or remote-host access without authorization.
- Publish atomic verified Conventional Commits through Jujutsu to `main`.
