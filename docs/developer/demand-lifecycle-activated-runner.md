# Activated Runner Validation

## Activation Boundary

| Surface | Contract |
| --- | --- |
| Ordinary startup | Keep lifecycle advertisement disabled by default |
| Controlled embedding | An explicit `RuntimePlatform` builder enables the existing lifecycle capability |
| Epoch | Use the runner's fresh nonzero epoch; never a persistent identity or caller-provided authority |
| Eligibility | The existing authenticated bilateral negotiation and native readiness gates remain mandatory |
| Configuration | No new JSON, Nix, pairing, or network identity field |
| Deployment | An opt-in test is not a production rollout or an energy result |

The builder selects the implemented policy. It cannot inject quiet permission,
pretend an incoming path exists, skip authentication, shorten idle eligibility,
or replace native transport ownership.

## Controlled Application Builds

| Boundary | Contract |
| --- | --- |
| Cargo feature | `demand-lifecycle-trial`, absent from default features |
| Application entry points | CLI, Android network supervisor, and Android E2E overlay endpoint explicitly apply the build policy |
| Embedding default | `RuntimePlatform::new` stays passive, even when the feature is compiled |
| Shared policy builder | `with_build_lifecycle_policy` applies the feature without changing installed TUN state, control ownership, or explicit embedding opt-in |
| Android feature | Forward the Android crate feature to the shared Rust dependency |
| Test counterpart | Forward the fixture crate feature; enable only its overlay endpoint, not its bootstrap/relay infrastructure |
| Nix outputs | Separate Linux, fixture, Android native, and APK trial packages; ordinary package outputs stay passive |
| Wire/configuration | No new protocol version, JSON field, NixOS option, identity, or pairing requirement |
| Compatibility | Quiet still requires both authenticated peers to support and agree to the current lifecycle generation |

### Trial Commands

```sh
cargo build --locked --features demand-lifecycle-trial --bin p2p-vpn
cargo build --locked -p p2p-vpn-android --features demand-lifecycle-trial
cargo build --locked -p p2p-vpn-android-e2e-fixture --features demand-lifecycle-trial
nix build .#demand-lifecycle-trial
nix build .#android-e2e-fixture-demand-lifecycle-trial
nix build .#android-native-arm64-demand-lifecycle-trial
nix build .#android-debug-apk-demand-lifecycle-trial
```

- Verify both feature-off and feature-on startup selection, plus explicit embedding opt-in under either build.
- Preserve existing normal/mixed-version fixtures; enabling a feature must not silently activate every test runner.
- Record source revision, feature selection, hashes, signer, ABI, and functional results for any measured APK.
- Build one candidate at a time with bounded jobs and storage; inspect substitutions before restoring a large toolchain.
- A host test, source evaluation, or successfully built APK is not a cellular energy result.
- The ARM64 cross-stdlib bootstrap explicitly honors `NIX_BUILD_CORES`; its platform uses the same pinned host Cargo as the x86_64 build, with no compiler/NDK version change.
- Compiler and sysroot must reference the same bounded cross-stdlib output; reject a full compiler bootstrap or duplicate original sysroot build.

### Build Verification

| Check | Scope |
| --- | --- |
| Build-policy unit tests | Passive embedding under either build, exact feature selection, retained TUN/control state, and idempotent explicit opt-in |
| Running endpoint | The Android test counterpart queries the real capability endpoint, not an injected capability or state label |
| Enabled workspace | 2,112 passed, zero failed, 50 ignored across 16 suites |
| Default workspace | Clean final rerun: 2,112 passed, zero failed, 50 ignored across 16 suites |
| Enabled lint | Correctness, suspicious-code, and performance error groups pass; advisory warnings remain |
| Nix packaging check | Default/trial feature separation, check features, custom x86_64 build flags, and distinct APK native dependencies |
| Other systems | Packaging assertions evaluate on aarch64 Linux and Darwin; no cross-system execution claim |
| Android source | ARM64 derivation includes the feature and byte-identical current shared/JNI/fixture Rust sources |
| Device/energy | Host verification alone proves neither device execution nor the 10x target |

- The first default endpoint assertion queried `state` instead of `capabilities`; the corrected fixture suite passes.
- Keep failed and corrected run records separate; do not report the first default run as successful.
- Whole-file Nix formatting already fails on the base flake; preserve unrelated formatting instead of mixing it into trial packaging.

### Measurement Admission

| Boundary | Requirement |
| --- | --- |
| Existing connected-idle harnesses | Wi-Fi-only admission or requiring one live QUIC session per peer cannot validate a dormant cellular candidate unchanged |
| Matched topology | Retain the same identities, network membership, authorized peer inventory, and workload across original and candidate builds |
| Candidate idle | Require authenticated bilateral quiet and a usable incoming anchor; zero per-peer sessions alone proves neither dormancy nor reachability |
| Feature preservation | Verify real incoming/outgoing delivery before and after the capture, outside its energy interval; retain failures rather than dropping them from results |
| Observation | No recurring status polling, wake probes, or deliberate application traffic inside an idle capture |
| Acceptance | Use the existing [battery measurement contract](android-battery-efficiency.md): fresh paired/reversed cellular app mAh/hour, replicated reduction of at least 90%, and modem-energy corroboration |

Do not keep a candidate active merely to satisfy an old connected-session
counter. Do not count disconnected or unreachable peers as successful low-power
behavior either. Physical paths may differ by lifecycle state; network authority
and demonstrated incoming functionality must not.

### Retained Android Candidate

| Check | Result |
| --- | --- |
| ARM64 native | Trial feature compiled; bounded cross-stdlib and matching sysroot; Cargo auditing retained |
| APK | ARM64-only debug phone artifact; existing package and development signer |
| Java verification | Fresh run: 158 tests pass, no failures/skips, zero lint errors, seven lint warnings |
| Packaging | Four 16 KiB-aligned load segments, APK alignment valid, packaged library hash and executable code match the verified native output |
| Linux counterparts | Matched opt-in CLI and rootless endpoint binaries retained separately from the mutable Cargo output aliases |
| Device/energy | Not deployed or measured; no battery-improvement claim |

The [build record](android-lifecycle-trial-build-2026-11-13.json) contains hashes,
source/recipe identifiers, raw-log locations, invalidated attempts, resource
limits, and remaining acceptance gates. This phone-test APK is not a newly
verified dual-ABI release.

## Observable State

- Startup identifies passive versus explicitly enabled lifecycle policy.
- Peer state reports remote lifecycle support, maintenance pause, and committed quiet permission separately.
- Quiet permission comes from the current application-demand generation, not a cached phase label.
- Read-only diagnostics must not create application demand or renew a path.

## Native Scenario

1. Start a real encrypted circuit-relay server and two production shared runners.
2. Give each endpoint a real reservation and enable lifecycle negotiation through the builder.
3. Exchange real IPv4 packets through packet channels instead of modifying host TUN routes.
4. Stop application traffic and wait through the unchanged 45-second idle threshold.
5. Require both peers to commit quiet and retire their overlay transports while retaining incoming relay access.
6. Send one new IP packet from only one endpoint; require delivery without repeated test probes or destination-side traffic.
7. Repeat with the other endpoint initiating after a new idle period.
8. Verify ordinary passive startup and mixed enabled/passive compatibility remain reachable and do not grant quiet.

Use normal time and real sockets. Do not set quiet flags, inject readiness,
advance a virtual clock, or manually reconnect the peers to make the scenario
pass. Keep all waits bounded and preserve failure state in test evidence.

## Activated QUIC Scenario

| Stage | Required Evidence |
| --- | --- |
| Active | Fresh bidirectional IP packets increment both owned-QUIC datagram counters |
| Idle | Real bilateral negotiation; zero UDP/QUIC sessions and zero overlay connections on both endpoints |
| Quiet interval | Allow terminal close traffic to drain, then observe at least one normal transport idle interval without new proxy datagrams or recreated sessions |
| Wake fallback | Blackhole advertised QUIC paths; one IP packet must arrive over the retained circuit-relay route without destination application traffic |
| Promotion | Restore the UDP paths; require fresh bidirectional traffic over QUIC again |
| Reverse | Repeat real idle, single-packet relay wake, and QUIC promotion with the other endpoint initiating |

- Use the existing real UDP proxy, encrypted QUIC transport, and circuit relay.
- Bind QUIC listeners on wildcard addresses and advertise only proxy endpoints; do not accidentally leave a direct bypass in the fixture.
- Keep idle eligibility, retention, peer authorization, and first-packet checks unchanged.
- Count actual proxied datagrams and delivered IP packets, not merely a selected-path label.
- Quiet sampling is not a battery measurement or proof of public-WAN/mobile reachability.

### Relayed Control Admission

The first trial carried application traffic over the relay but started no QUIC
negotiation within 90 seconds. The packet-plane admission gate required an
existing direct libp2p stream, conflating control transport with UDP reachability.

| Boundary | Contract |
| --- | --- |
| Existing direct control | Preserve existing direct-path negotiation and legacy compatibility |
| Relay alternative | Only the identity-bound owned-QUIC backend, never legacy owned UDP |
| Opt-in | Both validated peers advertise lifecycle support; ordinary passive startup is unchanged |
| Authority | Configured authorized overlay peer, matching network and current membership scope |
| Cryptographic binding | Both sides advertise bound QUIC, endpoint candidates, and certificates; signed handshakes and existing TLS/channel-binding checks remain mandatory |
| Demand | A current, valid application-demand generation with activity less than 45 seconds old |
| Incoming access | A healthy authenticated circuit-relay control path; no disconnected negotiation-created dial |
| Quiet | No negotiation while maintenance is paused or quiet is committed |
| Rechecks | Apply admission at request creation, inbound hello acceptance, response completion, and capability-refresh retries |
| Lifetime | Existing handshake deadlines, task generations, retry backoff, and cancellation remain authoritative |

- Capability refresh, hello traffic, and expired attempts do not create or renew application demand.
- A validated lifecycle wake or actual inbound payload can supply the receiver's demand observation.
- Maintenance alone must not turn a relay-only idle peer into a permanent QUIC negotiation loop.
- Relay-only admission is permission to try authenticated QUIC, not proof that NAT traversal will succeed.
- Keep the relay data fallback usable while direct QUIC is attempted or blocked.
- Legacy UDP fallback priority must not mask a needed QUIC retry when only relay control is available.

## Follow-On Gates

| Gate | Evidence Still Required |
| --- | --- |
| Preferred path | Local activated QUIC promotion/retirement/relay-wake passes; public-WAN and mobile cases remain |
| Control changes | Membership, revocation, hostname/DNS, and pairing delivery during dormancy |
| Incoming lifetime | Extended idle beyond transport and reservation renewal boundaries |
| Network changes | Automatic LAN-first recovery across controlled outages and real mobile roaming |
| Device integration | Enabled Linux and Android candidates, including screen-off and always-on VPN |
| Energy acceptance | Fresh replicated paired/reversed cellular app mAh/hour at least 90% lower, corroborated by modem energy |

Native tests establish only the boundaries they exercise. Production activation
and the 10x battery goal remain unproven until their own gates pass.

## First Wake Packet

The first enabled-runner trial negotiated quiet and retired both real overlay
transports. A single wake packet then expired under the ordinary three-second
queue age before the existing 15-second LAN-first recovery window completed.

| Boundary | Required Treatment |
| --- | --- |
| Eligibility | Only a validated outbound IP packet that consumes current negotiated quiet permission |
| Retention | That first packet gets a 30-second allowance from original admission; an explicitly longer ordinary age remains unchanged |
| Bound | Fifteen seconds LAN-first grace, ten seconds protocol work, five seconds scheduling margin; not an availability guarantee |
| Other traffic | Keep the configured ordinary packet age, packet count, and byte limits unchanged |
| Repeat traffic | Subsequent packets do not acquire or renew the first packet's retention allowance |
| Retries | Never reset enqueue time, renew application demand, or create a new retention allowance |
| Failure | Expire normally at the fixed bound; no unbounded disconnected buffer |
| Authorization | Revocation and existing send-time validation still prevent delivery |
| Compatibility | Passive and legacy peers cannot acquire this allowance without negotiated quiet |

This is a bounded wake-up exception, not a larger steady-state queue. Preserve
LAN-first discovery and recovery rather than bypassing it to satisfy a local
latency test. The exception remains behind the explicit lifecycle activation.

## Native Trial Results

| Check | Evidence |
| --- | --- |
| Activated startup | Both real runners advertise their fresh lifecycle epochs only after explicit builder opt-in |
| Bilateral idle | Normal-time negotiation grants quiet on both sides; native overlay connections close while relay reservations survive |
| First inbound packet | One IPv4 packet arrives after approximately 20.0 seconds; the destination sends no application traffic |
| Reverse wake | A second complete idle/retirement cycle precedes one reverse IPv4 packet; delivery takes approximately 24.9 seconds |
| Mixed-version idle | Enabled/passive pair remains non-quiet across 50 seconds of idle and retains bidirectional delivery |
| Queue boundary | Seven demand tests and 17 queue tests pass, including exact expiry, non-renewal, interior removal, ordering, byte/count limits, and passive age |
| Observation window | Native receive wait covers the 30-second retention allowance plus five seconds of scheduler margin; queue tests check exact expiry separately |

- The 20-second receive wait in an earlier trial ended before reverse recovery completed; retained evidence distinguishes that from packet expiry.
- Ordinary steady-state queue age remains three seconds by default; only the first packet consuming quiet permission gets the allowance.
- Queue expiry uses one allocation-free stable scan because a wake packet can outlive ordinary packets behind it.
- Packet admission time, application demand, byte limits, and send-time authorization remain authoritative.
- Evidence uses real encrypted native TCP and circuit-relay protocols, but loopback underlay and packet channels instead of host TUN routes.
- No synthetic readiness, quiet flags, virtual time, public Internet, phone deployment, or battery comparison is involved.

```sh
cargo test --offline --locked --test connection_recovery lifecycle:: -- --test-threads=2
cargo test --offline --locked --lib demand_tests -- --test-threads=2
cargo test --offline --locked --lib queue::tests -- --test-threads=2
```

These trials close local TCP/relay and owned-QUIC idle/wake gates only. Extended
idle, public-WAN, mobile roaming, feature preservation, and measured cellular
energy remain separate acceptance requirements.

## Activated QUIC Results

| Check | Focused Native Evidence |
| --- | --- |
| Relay-only control | Initial owned-QUIC traffic succeeds without a direct libp2p stream; both endpoints deliver ten consecutive fresh packet pairs |
| Normal-time idle | Both endpoints negotiate quiet and retire QUIC sessions and overlay connections |
| Quiet packet planes | After five seconds for terminal close traffic, each of two 15-second samples has unchanged proxy-datagram and path-probe counters, with no recreated sessions |
| First wake packets | Blackholed QUIC cannot deliver; single packets in each direction arrive through the relay in approximately 24.7 seconds |
| QUIC promotion | Restoring UDP returns fresh traffic to QUIC in approximately 10.0 and 35.1 seconds |
| Fixture integrity | Wildcard listeners advertise exactly the proxy endpoints; no advertised direct bypass |
| Admission gates | Six tests cover absent/stale demand, both-end capability/binding/scope checks, quiet/preparation, authority, disconnected metadata, and early idle rejection |
| Dual-backend repair | Real established/closed QUIC session: direct control keeps UDP priority; relay-only control selects QUIC repair instead of an unnegotiable UDP fallback |
| Compatibility | Existing direct UDP, unbound-QUIC compatibility, QUIC handshakes, and dormant-hello rejection pass |

- The initial 90-second trial failed to start QUIC because of the direct-control gate; it is retained as failure evidence, not a connectivity success.
- A preliminary full run was intentionally cancelled after review found the dual-backend selection issue; only the replacement run can verify final source.
- No steady-state timer, public wire/configuration field, cryptographic scheme, or native request-ownership rule changed.
- Ordinary startup stays passive; relay-only QUIC admission requires explicit lifecycle opt-in on both endpoints.
- The UDP fixture represents a reachable datagram path, not a successful public NAT hole punch.
- Quiet proxy counters do not measure total network traffic, cellular radio sleep, or energy consumption.

```sh
cargo test --offline --locked --test connection_recovery \
  lifecycle::quic_trials::activated_quic_retires_and_recovers_through_relay -- --exact
cargo test --offline --locked --lib packet_negotiation_path
```

## Verification Record

| Check | Result |
| --- | --- |
| Full workspace/all targets | 2,108 passed; zero failed; 50 existing or opt-in exclusions across 16 suites |
| Repeat native scenarios | TCP/relay and QUIC idle/wake pass; final QUIC relay wakes take approximately 24.8 and 19.7 seconds, followed by QUIC promotion |
| Formatting | Workspace Rust check passes |
| Clippy | Correctness, suspicious, and performance groups pass; advisory warnings remain |
| Package sources | Linux and Android ARM64/x86-64 runtime, four native patches, and lockfile match byte-for-byte |
| Integration packaging | All 43 Rust integration/support files match the Linux package source |
| Build resources | One approximately 7 GiB target, two Cargo build jobs, four independent test workers, no incremental compilation |
| Tool restoration | Restored missing matching Clippy from a 5.9 MiB binary substitute; source builds were disabled |
| Package/device scope | Host workspace compiled; full Nix packages, APK builds, and device deployments not performed |
| Goal acceptance | Ordinary startup stays passive; no new battery measurement or claimed energy improvement |

These executable checks are not a formal proof. The existing privileged and
device exclusions remain unverified operational gates, not passed tests.

The offline Android package plan contains many locally unrealized inputs.
A bounded online cache query did not finish within 90 seconds; this does not
establish which inputs lack binary substitutes. No package build was started.

## Cellular Idle Diagnostics

The [first complete cellular pair](android-cellular-lifecycle-pair-2026-11-13.json)
failed the 10x target. The phone advertised support, but its counterpart reported
neither maintenance preparation nor committed quiet permission.

| Signal | Constraint |
| --- | --- |
| Idle preparation reason | Distinguish unsupported epoch/scope, missing incoming path, queued payload, and recent demand |
| Native work | Report separately from application work; never create demand to collect diagnostics |
| State changes | Bound output to one event per peer per idle interval; use the existing driver, no new timer |
| Privacy | Public peer ID and local booleans/enums only; no membership secrets, keys, addresses or packet contents |
| Behavior | No new dial, probe, keepalive, wire field, user option or relaxed eligibility check |

The `peer_lifecycle_idle_preparation` event reports the local phase, eligibility
reason, incoming-path availability, application work, outstanding native work
and maintenance pause. Passive runtimes emit no preparation event.

- Repeated unchanged state produces no event.
- Flapping state and remote-epoch replacement cannot bypass the per-peer rate limit.
- Missing native work is not inferred from an empty application queue.
- Preparation still does not grant permission to close a peer's path.

## Discovery Effect Audit

- Recheck each target before automatic data dials from late discovery results.
- Retain valid address observations without repairing a maintenance-paused peer.
- Keep explicit, scoped control delivery independent of data-path dormancy.
- Preserve unpaused/legacy behavior and LAN-first recovery.

These corrections require focused regressions and fresh device measurements.
They do not establish which component consumed the measured cellular energy.

## Reconnection Idle Timeline

The [first diagnostic deployment](android-idle-reconnect-diagnosis-2026-11-13.json)
repeatedly reported `RecentlyActive` after
relay circuits closed and reconnected. Capability invalidation removed the
lifecycle owner, restarting the 45-second idle grace without payload demand.

| Boundary | Required Behavior |
| --- | --- |
| Temporary capability absence | Revoke quiet and semantic exchanges immediately; retain the configured member's bounded idle timeline |
| Missing validation | No idle offer, acknowledgement commitment, wake acceptance or path retirement from cached capabilities |
| Same revalidated runtime epoch | Resume the existing application idle age, not the age of the new connection |
| New remote runtime epoch | Start a fresh conservative grace and reject previous exchanges |
| Real membership revocation | Remove policy state, quiet permission and semantic ownership |
| Existing native requests | Retain physical slots until terminal events; suspension must not free them early |
| Payload while disconnected | Record actual demand and honor its idle grace after revalidation |

Validation must reproduce repeated sub-45-second disconnects, capability
absence, same/new epochs, true revocation, stale replies and application demand.
Preserving idle age alone is not proof of public-relay dormancy or energy savings.

The runtime now suspends the configured member's policy instead of erasing its
application idle history when capabilities disappear. It retains no capability
authority: preparation and negotiation still require a current validated session.

- Per-peer session generations fence completions across capability invalidation.
- Revocation permanently invalidates retained flights without freeing native slots early.
- Re-admission cannot match a removed policy's flights; generation exhaustion fails closed.
- A remote restart still resets the idle grace; real payload demand is observed during absence.

### Reconnect Verification

| Check | Result |
| --- | --- |
| Feature-enabled workspace/all targets | 2,124 passed; zero failed; 50 existing/opt-in exclusions across 16 suites |
| Feature-off lifecycle regressions | 34 dispatcher tests and the suspension-model test pass |
| Native delivery regressions | Late ACK, reply-sent, and both unsupported terminal forms cannot cross capability absence; revoke/re-admit cannot revive an old flight |
| Compatibility | Explicit unsupported support and scope changes remain conservative; no wire/configuration field changes |
| Source/build | ARM64 Nix release build passes; eleven relevant runtime/JNI/lockfile sources match |
| Android packaging | 158 Java tests pass; zero lint errors, seven unchanged warnings; signing and 16 KiB alignment pass |
| Resources | One existing 7.9 GiB host target; one ARM64 build with two compiler jobs; no SDK/compiler/emulator rebuild |
| Formal verification | No Lean/model-checker project exists; these are executable regressions, not a formal proof |

The first full host invocation omitted the flake's `RUST_MIN_STACK=8388608`
and aborted in CLI parsing. The repeated invocation used the declared setting;
the earlier default build passed 2,119 tests before the reconnect correction.

The final post-correction full workspace run used the trial feature. The default
build's post-correction check covered the changed lifecycle scope, not a second
complete all-target run. Device and replicated energy acceptance remain separate.
