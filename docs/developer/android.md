# Android Architecture

The Android target reuses the Rust protocol and runtime.

Java owns Android lifecycle, permissions, persistence, and the VPN interface.

## Component Map

```text
MainActivity
  -> P2pVpnService
     -> Android VpnService.Builder
     -> encrypted ProfileStore
        -> versioned ProfileCollection
     -> JNI NativeBridge
        -> p2p-vpn-android
           -> shared Android packet supervisor
              -> one physical TUN reader and writer
              -> isolated per-network packet ports
           -> shared p2p-vpn runtime
           -> in-process runtime control channel
           -> libp2p discovery and packet paths
```

## Source Layout

| Path | Responsibility |
| --- | --- |
| `android/app/src/main/java/.../MainActivity.java` | Native pair-and-connect UI |
| `android/app/src/main/java/.../P2pVpnService.java` | VPN and recovery lifecycle |
| `android/app/src/main/java/.../VpnMode.java` | Always-on and lockdown mode policy |
| `android/app/src/main/java/.../UnderlayTracker.java` | Deterministic physical-network selection |
| `android/app/src/main/java/.../DiagnosticReport.java` | Bounded aggregate-only support report |
| `android/app/src/main/java/.../ProfileStore.java` | Keystore-backed persistence |
| `android/app/src/main/java/.../ProfileCollection.java` | Versioned network collection and legacy migration |
| `android/app/src/main/java/.../PairRpc.java` | Existing pairing RPC shapes |
| `android/app/src/debug/java/.../DebugAutomationReceiver.java` | ADB-only E2E control |
| `android/app/src/main/res/values[-night]/` | System-selected light and dark themes |
| `crates/p2p-vpn-android/src/lib.rs` | JNI and runtime adapter |
| `crates/p2p-vpn-android/src/supervisor.rs` | Shared TUN dispatch, bounded queues, and route isolation |
| `scripts/android-device-audit.sh` | Physical arm64 transition and endurance audit |
| `src/runtime/tun.rs` | Platform-neutral packet I/O and route hooks |
| `src/runtime/control.rs` | In-process runtime control channel |
| `nix/android.nix` | Cross build, SDK, APK, apps, and checks |

## Platform Boundary

The shared runtime receives a `RuntimePlatform`.

Android supplies supervisor-backed packet I/O. Java establishes the physical
TUN and initial routes; Rust validates packet dispatch and live route updates.

| Concern | Linux | Android |
| --- | --- | --- |
| TUN creation | Rust `tun` crate | `VpnService.Builder` |
| Interface addresses | Netlink commands | `Builder.addAddress` |
| Overlay routes | Netlink commands | `Builder.addRoute` |
| Physical packet read/write | Linux TUN file | One reader and one writer over the detached descriptor |
| Runtime packet I/O | Linux TUN file | Isolated supervisor port and queues |
| Runtime route updates | Netlink reconciliation | Atomic in-memory dispatch validation |
| Local control | Unix socket | In-process channel |
| Service lifecycle | systemd or CLI | Foreground `VpnService` |

Linux behavior and protocol encodings remain unchanged.

## Android Permissions

| Permission | Purpose | Runtime gate |
| --- | --- | --- |
| `INTERNET` | Public discovery, relay, and direct transports | Install time |
| `ACCESS_LOCAL_NETWORK` | LAN discovery and raw direct TCP/UDP | API and target 37+ |
| `POST_NOTIFICATIONS` | Foreground-service connection status | API 33+ |
| Android VPN consent | Create and own the TUN interface | Every revoked grant |

The UI requests local-network access before VPN consent.

The service rejects startup after revocation. A manual service stops; an
always-on service remains foreground and reports the missing permission.

## JNI Contract

| Method | Result |
| --- | --- |
| `nativeCreateProfile` | Minimal validated config and derived routes |
| `nativeInspectProfile` | Peer ID, MTU, addresses, and routes |
| `nativeStart` | Starts one runtime over the supplied TUN descriptor |
| `nativeStatus` | Runtime phase and control status lines |
| `nativeNetworkChanged` | Invalidates stale paths and rediscovers without stopping TUN |
| `nativeStop` | Requests shutdown and joins the runtime thread |
| `nativePairRpc` | Calls the existing daemon pairing state machine |
| `nativeApplyPairingArtifacts` | Applies signed artifacts to the profile |

Every JNI response uses a bounded JSON envelope:

```json
{"ok":true,"value":{}}
```

Native panics are caught before crossing JNI.

`nativeStart` remains a compatibility entry point for one network. It routes
that network through the shared supervisor seam; concurrent service and UI
activation is not implemented by this change.

## TUN Ownership

`ParcelFileDescriptor.detachFd()` transfers ownership to JNI.

JNI adopts the descriptor before any fallible string conversion.

The native adapter duplicates it once. The supervisor owns one physical reader
and one physical writer for the complete Android VPN interface.

| Descriptor | Use |
| --- | --- |
| Original | Shared polling packet writer |
| Duplicate | Shared polling packet reader |

Both descriptors use nonblocking I/O with bounded polling. Shutdown signals
the workers, closes queues, and joins the runtime and TUN threads.

Rust RAII then closes both descriptor owners.

Physical TUN write backpressure is bounded to 250 ms per packet. A timeout
drops that packet and releases any network-removal gate.

## Shared Packet Supervisor

The supervisor separates physical TUN ownership from network runtimes.

Each network receives an isolated `PacketIo` port and route controller. The
current JNI compatibility path creates one port and starts one runtime.

### Packet Flow

```text
physical TUN reader
  -> parse inner destination address
  -> deterministic route lookup
  -> selected network outbound queue
  -> network runtime
  -> network inbound queue
  -> fair physical TUN writer
```

The outbound dispatcher uses the packet's inner IPv4 or IPv6 destination.
Routes are ordered by longest prefix, then stable network ID order.

Cross-network overlap is rejected, so one destination cannot belong to two
active networks.

The packet source must belong to the selected network. An unowned source or a
source owned by another network is dropped before it reaches either runtime.

Runtime-to-TUN packets are parsed again at the supervisor boundary. Their
source must be remote-owned and their destination must be local to that port.

### Queue Bounds

Every network has independent queues in both directions.

| Direction | Packet limit | Byte limit |
| --- | ---: | ---: |
| TUN to runtime | 256 | 1 MiB |
| Runtime to TUN | 256 | 1 MiB |

Full and oversized queues drop the packet without failing the runtime. Queue
pressure in one network does not consume another network's queue allowance.

Packets larger than a network's MTU are dropped before its runtime reader.
Disabling marks its writer inactive, removes its routes, then closes and
discards its packet queues.

The inbound writer visits ready network queues in round-robin order. A busy
network cannot continuously take the first write slot.

### Route Validation

The supervisor validates the complete candidate route map before activation.

| Conflict | Result |
| --- | --- |
| Duplicate network ID | Reject supervisor creation |
| Local address or prefix overlaps its remote route | Reject supervisor creation |
| Any local or remote prefix overlaps another network | Reject supervisor creation |
| Live update conflicts with another network | Reject update; fail affected network closed |
| Live update is based on stale installed routes | Reject update; fail affected network closed |
| Live update changes any local address | Reject update; require a TUN rebuild |
| More than 1,024 prefixes in one network | Reject activation or update |
| More than 4,096 prefixes across the supervisor | Reject activation or update |

Live updates replace the dispatch map atomically only after validation.

Removing a network removes its routes and closes only its packet queues.
Rejected reconciliation also closes only that network, even if the core
membership source had already committed its candidate records.

### Network Isolation

Isolation is enforced at the packet port, route, and queue boundaries.

| Boundary | Guarantee |
| --- | --- |
| Runtime input | Receives only packets dispatched to its network routes |
| Runtime output | Must match current remote-source and local-destination ownership |
| Queue capacity | Cannot consume another network's packet or byte allowance |
| Route ownership | Cannot activate a prefix owned by another network |
| Removal | Closes only the removed network's routes and queues |

All runtimes remain in one native process. The supervisor is packet-plane
isolation, not an operating-system process sandbox.

### Counters

Native status exposes aggregate supervisor counters without network IDs.

| Scope | Counters |
| --- | --- |
| Supervisor | Malformed and unroutable outbound packets |
| Supervisor | Source-ownership mismatch drops |
| Per network index | Outbound enqueued, queue drops, oversized drops |
| Per network index | Outbound and inbound presentation-translation drops |
| Per network index | Inbound malformed, ownership, queue, and oversized drops |
| Per network index | Packets discarded during removal or shutdown |
| Per network index | Inbound written, backpressure drops, and write failures |
| Per network index | Rejected live route updates |

These counters distinguish route failures from queue pressure while preserving
network identity privacy in status output.

### Current Boundary

The supervisor can model multiple isolated network ports, validates their
combined route ownership, and accepts one stable IPv4/IPv6 presentation pair.

At the shared TUN boundary, it translates that pair to each network's primary
overlay addresses. IPv4 header, transport pseudo-header, and quoted ICMP
checksums are updated in place.

The concurrent data plane supports TCP, UDP, UDP-Lite, ICMP, ICMPv6, and packets
without a next header. The policy applies uniformly to every active network.

IP source routing and IPv6 Home Address options fail closed in the ownership
parser and count as malformed packets. Mobility, HIP, Shim6, invalid mandatory
checksums, and unsupported transports count as per-network translation drops.

Fragment and quoted ICMP handling preserve checksums and presentation-side flow
identity after reassembly. Queue or translation failure drops one packet without
stopping another network.

The Android service, JNI lifecycle, and UI still activate one selected network.
Concurrent multi-network lifecycle support remains follow-up work.

`VpnService.Builder` routes are fixed when the interface is established.
Runtime-learned custom prefixes currently update native dispatch only. The
multi-network lifecycle must re-establish or replace the TUN before reporting
those prefixes as active Android routes.

## Profile Lifecycle

```text
no profile
  -> create minimal Rust config and stable identity-derived hostname
  -> wrap it in a versioned network collection
  -> encrypt the collection atomically
  -> restore and inspect every entry on startup
  -> connect through VpnService
```

The profile contains the private libp2p identity and learned membership.

It never enters the Android resources or APK.

An existing raw JSON profile migrates without re-encoding its config. Its
network UUID is derived from the existing network name and peer ID.

The deterministic UUID makes migration restart-safe if Android terminates the
process between moving runtime state and writing the collection.

The profile stores `network.dns.hostname` while leaving the Android DNS
listener disabled. Pairing authenticates that label independently of serving DNS.

## Always-On Lifecycle

The manifest explicitly advertises always-on support.

Android starts `P2pVpnService` without the app's connect action. The service
enters the foreground immediately, then queues profile restoration.

```text
system start
  -> foreground notification
  -> encrypted profile restore
  -> VPN and native runtime start
  -> bounded recovery after process death or network change
```

| State | Service action |
| --- | --- |
| Profile ready | Establish TUN and start the Rust runtime |
| Profile absent | Stay foreground and wait for profile creation |
| Profile unreadable | Stay foreground and expose reset recovery |
| Local-network permission absent | Stay foreground and expose permission state |
| Always-on disconnect request | Ignore it; Android owns lifecycle |
| Android VPN permission revoked | Stop the runtime and service |

Android API 29 exposes `isAlwaysOn()` and `isLockdownEnabled()`.

System starts recognize both actionless intents and `VpnService.SERVICE_INTERFACE`.
The latter is used when Android restarts the VPN after an app update.

Mode reporting on Android 10 and newer is authoritative. Older system starts
retain always-on ownership from the start intent.

### Transport Isolation

`Builder.addDisallowedApplication(getPackageName())` excludes the app UID from
the TUN before `establish()`.

| Socket owner | Routing behavior |
| --- | --- |
| Rust libp2p TCP and QUIC | Physical Android networks |
| Owned packet-plane QUIC and UDP | Physical Android networks |
| Discovery and resolver sockets | Physical Android networks |
| Other app overlay traffic | TUN routes |

This process-level boundary covers sockets created internally by libp2p. It
also preserves simultaneous LAN discovery across available interfaces.

### Lockdown Boundary

Lockdown is intentionally rejected on API 29 and newer.

Android lockdown blocks traffic outside the VPN for other apps. The owning VPN
process remains able to reach its physical underlay.

p2p-vpn installs only overlay routes and cannot carry default internet traffic.

The service remains foreground with a fixed error and a 30-second mode poll.
It performs no transport reconnect loop while lockdown remains enabled.

The user must disable **Block connections without VPN**.

## Persistence

`ProfileStore` uses separate magic values and authenticated-data domains.

| File | Maximum plaintext | Purpose |
| --- | ---: | --- |
| `profile.enc` | 8 MiB | Legacy config or versioned profile collection |
| `pairing-operation.enc` | 16 KiB | Active operation metadata |

Both files use `AtomicFile` and AES-GCM.

The AES key is non-exportable in `AndroidKeyStore`.

Files are under `noBackupFilesDir`, and application backup is disabled.

### Runtime State

Runtime state is isolated by the stable network UUID.

| Path | Purpose |
| --- | --- |
| `runtime/<network-id>/pairing-state.json` | Native pairing state machine |
| `runtime/<network-id>/membership-state.json` | Learned membership state |
| `runtime/<network-id>/membership.key` | Transient enrollment secret; removed on load |

Legacy state files at `runtime/` move into the migrated network directory.

Pairing recovery metadata uses schema version 2 and records the network UUID.
Version 1 metadata binds to the sole migrated profile and is rewritten.

## Pairing Transaction

Android uses `/p2p-vpn/pairing-code/1` through the existing runtime RPC.

No Android-specific pairing protocol exists.

Both pairing roles include the stable Android hostname in signed membership
records. DNS-listener enablement does not control identity claims.

```text
persist operation intent
  -> replay idempotent open or join
  -> poll status
  -> require inviter approval
  -> persist completion transcript
  -> fetch and apply artifacts
  -> encrypt updated profile
  -> acknowledge transcript
  -> clear operation metadata
```

The operation ID makes open and join replay-safe after process death.

Application and acknowledgment resume after reconnect.

The received `membership.key` must be a regular file at the managed runtime
path. Rust unlinks it before reading and embedding it in the encrypted profile.

## Route Installation

Rust derives the local addresses and effective route set from the profile.

Java installs each result through `VpnService.Builder`.

| Route class | Android action |
| --- | --- |
| Built-in IPv4 overlay | Add overlay prefix |
| Built-in IPv6 overlay | Add overlay prefix |
| Signed member host route | Add effective prefix |
| Default internet route | Never add |

Android system DNS is outside the current scope.

## Runtime Recovery

The service polls native health every two seconds.

Three consecutive control failures mark the native runtime failed.

Physical-network callbacks exclude VPN networks and rank candidates:

| Priority | Network |
| ---: | --- |
| 1400 | Validated Ethernet |
| 1300 | Validated Wi-Fi |
| 1200 | Validated cellular |
| 1100 | Validated Bluetooth |
| 0-400 | Equivalent unvalidated transports |

The first callback establishes baseline state.

The app UID remains outside the VPN route set. The selected underlay is used
for recovery decisions, not as a hardcoded socket route.

A selected-network change, loss, or recovery is debounced for 500 milliseconds.

The service then calls `nativeNetworkChanged` without replacing the TUN,
profile, process, or native runtime.

The runtime performs one recovery transaction:

1. Advance the connection epoch.
2. Disconnect active and pending connections to every known peer.
3. Ignore connection-bound events from the previous epoch.
4. Retire relay listeners and learned external endpoints by ID.
5. Invalidate selected paths and packet-plane sessions.
6. Clear stale in-flight packet state and recovery backoff.
7. Restart LAN discovery and ordered direct dialing.
8. Resume public routing, hole punching, and relay fallback as needed.

Recovery submits one known-peer dial per peer.

Its addresses are ordered fallbacks with dial concurrency set to one.

| Dial Rule | Effect |
| --- | --- |
| `NotDialing` | Prevents parallel recovery attempts for one overlay peer |
| Ordered addresses | Tries direct candidates before circuit-relay fallback |
| Deterministic initiator | Keeps one direct connection when both peers dial |
| Pre-selection deduplication | Never selects a connection already retiring |

The service retries a failed JNI recovery signal twice. A third failure falls
back to the existing bounded native-runtime restart path.

Startup and native-health failures still restart the runtime with exponential
backoff capped at 30 seconds.

## Diagnostic Report

`DiagnosticReport` serializes a fixed schema capped at 64 KiB.

| Scope | Data |
| --- | --- |
| Service | Uptime, profile state, connection, always-on, lockdown, generation |
| Network | Coarse underlay kind and aggregate recovery counters |
| Runtime | Aggregate path, queue, drop, fallback, and demotion counters |
| Process | CPU time, PSS, private dirty memory, heap, threads |
| History | Bounded ring of 64 allowlisted event names |

The schema contains no free-form runtime error text.

It excludes peer IDs, overlay addresses, hostnames, pairing codes, membership
material, underlay addresses, network handles, and SSIDs.

The Android document picker writes the report to a user-selected destination.

## Build Outputs

Android outputs exist on `x86_64-linux`.

| Output | Purpose |
| --- | --- |
| `.#android-native-arm64` | API 26 arm64 JNI library |
| `.#android-native-x86_64` | API 26 x86_64 JNI library |
| `.#android-native` | Combined arm64 and x86_64 libraries |
| `.#android-rust-tests` | Host-side bridge tests |
| `.#android-debug-apk` | Signed dual-ABI debug APK |
| `.#android-e2e` | Managed Android/Linux E2E harness |
| `.#android-device-audit` | Guarded physical arm64 audit launcher |
| `.#android-emulator` | API 35 x86_64 emulator package |
| `.#android-sdk` | Pinned SDK and platform tools |
| `devShells.android` | Gradle, JDK, SDK, NDK, and ADB |
| `apps.android-install` | Build and install through ADB |
| `apps.android-emulator` | Boot the managed test emulator |
| `apps.android-update-deps` | Refresh pinned Gradle dependencies |
| `checks.android` | Full Android build and verification gate |
| `checks.android-device-audit-structure` | Fake-device contract and cleanup gate |

## Toolchain

| Layer | Version |
| --- | --- |
| Rust targets | `aarch64-unknown-linux-android`, `x86_64-linux-android` |
| Rust minimum API | 26 |
| Arm64 Rust toolchain | nixpkgs prebuilt NDK 27 target |
| x86_64 Rust toolchain | `build-std` with NDK 28.2 |
| Android compile/target SDK | 37 |
| Emulator image | Android 15 / API 35, x86_64 |
| Android build tools | 37.0.0 |
| Gradle | 9.5.1 |
| Android Gradle Plugin | 9.3.2 |
| Java | 17 |

Both JNI libraries and their APK entries support 16 KiB Android page sizes.

The NDK 27 arm64 build uses explicit page-size linker flags. The Android check
validates every ELF `LOAD` segment and the packaged APK alignment.

Gradle dependencies are pinned in `nix/android-gradle-deps.json`.

Refresh them only after an intentional Gradle dependency change:

```sh
nix run .#android-update-deps
```

## Managed Emulator

Boot the headless emulator and install the debug APK:

```sh
nix run .#android-emulator
```

The command prints the ADB serial after Android and the app are ready.

It remains attached to the emulator session.

Press `Ctrl-C` to stop the emulator and remove its temporary AVD state.

Use the printed serial from another shell:

```sh
ANDROID_SERIAL=EMULATOR_SERIAL nix develop .#android -c adb shell
```

## Automated Emulator Gate

Run the clean-emulator smoke scenario:

```sh
nix run .#android-e2e -- --scenario boot-smoke --output ./android-e2e-evidence
```

The command boots API 35, installs the real debug APK, checks the active app,
then removes the temporary emulator state.

Run encrypted profile lifecycle coverage:

```sh
nix run .#android-e2e -- \
  --scenario profile-persistence \
  --output ./android-profile-evidence
```

| Check | Assertion |
| --- | --- |
| Profile | Real JNI creates encrypted dual-stack identity state |
| Process death | Activity and service restore the same identity |
| Update | `adb install -r` preserves the same identity |
| Reinstall | A repeated replacement install preserves the same identity |

An uninstall or application-data clear still destroys the identity by design.

Run always-on lifecycle coverage:

```sh
nix run .#android-e2e -- \
  --scenario always-on \
  --output ./android-always-on-evidence
```

| Check | Assertion |
| --- | --- |
| Ownership | Android reports always-on mode while the tunnel remains connected |
| Disconnect | The app cannot stop an Android-owned tunnel |
| Update | A fresh service process restores the same encrypted profile |
| Lockdown | Unsupported blocked-connections mode stops with an actionable status |
| Recovery | Disabling lockdown restores the tunnel without app interaction |
| Cleanup | The harness clears Android's always-on settings before teardown |

Run isolated Linux pairing and traffic coverage:

```sh
nix run .#android-e2e -- \
  --scenario pairing-traffic \
  --output ./android-pairing-evidence
```

| Check | Assertion |
| --- | --- |
| Enrollment | Android receives only a code, not an overlay peer address |
| Discovery | A private Kademlia bootstrap locates the Linux inviter |
| Traffic | IPv4 and IPv6 pass 5/5 in both directions |
| Evidence | Logs are redacted, capped, and contain no pairing secret |
| Diagnostics | Exported report passes its bounded aggregate-only schema |
| Cleanup | Emulator, fixture, and private runtime state are removed |

Select one isolated data path:

```sh
nix run .#android-e2e -- \
  --scenario pairing-traffic \
  --path-mode quic-stream \
  --output ./android-quic-stream-evidence
```

| Mode | Required Final Path |
| --- | --- |
| `automatic` | Any supported path selected by the runtime |
| `quic-stream` | QUIC stream healthy; TCP, datagram, and relay paths absent |
| `tcp-stream` | TCP stream healthy; QUIC, datagram, and relay paths absent |
| `owned-quic` | Owned QUIC packet session carries measured packets |
| `relay-only` | Circuit relay carries packets; every direct path is absent |
| `relay-to-direct` | Relay carries baseline traffic, then promotes to direct TCP |

Run in-place underlay recovery coverage:

```sh
nix run .#android-e2e -- \
  --scenario underlay-recovery \
  --output ./android-underlay-evidence
```

| Transition | Assertion |
| --- | --- |
| Wi-Fi to cellular | Same process and runtime recover bidirectional traffic |
| Total loss | Runtime stays alive while all physical networks are absent |
| Cellular return | Discovery and traffic recover without intervention |
| Wi-Fi return | Preferred underlay and traffic restore automatically |

Run capability checks without starting Android:

```sh
nix run .#android-e2e -- --preflight --output ./android-e2e-preflight
```

| Exit | Meaning |
| ---: | --- |
| `0` | Scenario or preflight passed |
| `1` | Scenario started and failed |
| `2` | Invalid use or unavailable required capability |
| `77` | Explicit preflight or `--allow-skip` skipped |

`evidence.json` records preflight checks, the device contract, scenario steps,
the validated diagnostic report, and cleanup results.

`android.log`, `emulator.log`, and `fixture.log` are capped at 1 MiB each.

Runtime logs are redacted before evidence validation.

## Physical Arm64 Audit

Run the non-mutating device preflight:

```sh
nix run .#android-device-audit -- --preflight
```

Run a core proof against an existing Linux overlay member:

```sh
nix run .#android-device-audit -- \
  --scenario core \
  --network NETWORK \
  --peer-ipv4 LINUX_OVERLAY_IPV4 \
  --peer-ipv6 LINUX_OVERLAY_IPV6 \
  --output ./android-core-evidence
```

The safe launcher realizes `android-device-audit-runtime`.

That closure includes the APK and platform tools, but not the emulator image.

### Management Boundary

ADB controls the app and reads aggregate status.

It does not carry overlay or bootstrap traffic.

| Mechanism | Audit Policy |
| --- | --- |
| USB or wireless ADB | Management only |
| `adb forward` | Never used |
| `adb reverse` | Never used |
| Host overlay ping | Uses the host routing table |
| Android overlay ping | Uses the active `VpnService` interface |

### Audit Scenarios

| Scenario | Topology coverage | Endurance and lifecycle |
| --- | --- | --- |
| `full` | LAN, separate hotspot, upstream VPN, LAN return | Included |
| `core` | LAN, cellular or separate hotspot, LAN return | Included |
| `upstream-vpn` | LAN/Wi-Fi, VPN-routed hotspot, LAN/Wi-Fi return | Excluded |

`full` is the compatible default.

It requires an externally managed hotspot. The hotspot's upstream VPN must be
changeable while the Android device stays connected.

When the Android device supplies the cellular path, use two audit runs:

1. Run `core` on the cellular device.
2. Run `upstream-vpn` on another device that can join the VPN-routed hotspot.

Each evidence file identifies its scenario and exact coverage.

Together, proof-eligible `core` and `upstream-vpn` evidence cover the complete
physical topology and lifecycle contract.

### Scenario Sequences

| Phase | `full` | `core` | `upstream-vpn` |
| --- | :---: | :---: | :---: |
| LAN baseline | Yes | Yes | Yes |
| Separate hotspot or cellular | Yes | Yes | No |
| Hotspot upstream VPN | Yes | No | Yes |
| LAN return | Yes | Yes | Yes |
| Screen-off/Doze | Yes | Yes | No |
| Sustained run | Yes | Yes | No |
| Process recreation | Yes | Yes | No |
| APK replacement | Yes | Yes | No |

### Audit Assertions

| Phase | Required Evidence |
| --- | --- |
| LAN baseline | IPv4 and IPv6 pass 5/5 in both directions |
| Hotspot or cellular | Selection changes; traffic recovers without runtime restart |
| Hotspot upstream VPN | Traffic recovers without ADB tunneling or runtime restart |
| LAN return | Selection changes; traffic recovers without runtime restart |
| Screen-off/Doze | Five-minute hold, stable runtime generation, 20/20 traffic |
| Sustained run | Thirty minutes, sampled dual-stack traffic, bounded loss |
| Process recreation | New PID, same identity, restored traffic |
| APK replacement | `adb install -r`, same identity, restored traffic |

The upstream VPN belongs on the hotspot or router.

A second Android VPN app would replace p2p-vpn and invalidate the test.

### Evidence Contract

`evidence.json` is capped at 2 MiB.

It records only aggregate state:

| Scope | Recorded |
| --- | --- |
| Contract | Scenario, coverage flags, thresholds, operator confirmations |
| Device | arm64 contract and Android API |
| Recovery | Convergence time, underlay counters, runtime generation |
| Traffic | Sent and received packets per direction and family |
| Paths | Aggregate direct, relay, routing, and promotion counters |
| Resources | CPU delta, final PSS, private dirty memory, threads |
| Battery | Level and charge-counter deltas; plugged state qualifies the result |
| Lifecycle | Process recreation, APK replacement, identity-preserved booleans |

Serials, models, peer IDs, addresses, codes, and identity material are excluded.

`full` and `core` runs shorter than 30 minutes or five minutes of Doze require
`--allow-short`.

`upstream-vpn` has no endurance phase. Its proof eligibility requires two
interactive topology confirmations and omitting `--allow-short`.

Any run using `--allow-short` cannot set `contract.proof_eligible`.

`full` requires three interactive transitions.

`core` and `upstream-vpn` each require two.

The fake-device auto-confirm mode is accepted only with `--allow-short` and can
never produce proof-eligible evidence.

### Failure Cleanup

Every exit path releases forced Doze and wakes the screen.

Temporary host state is removed without clearing the Android profile.

The structure check injects a Doze failure and verifies this cleanup.

### Storage Safety

The app, SDK, cross toolchains, and emulator form a large Nix closure.

Configure the Nix daemon on Android development hosts:

```nix
nix = {
  gc = {
    automatic = true;
    dates = "daily";
  };
  settings = {
    min-free = 128 * 1024 * 1024 * 1024;
    max-free = 256 * 1024 * 1024 * 1024;
  };
};
```

| Layer | Guard |
| --- | --- |
| Safe launcher | Checks temporary and Nix-store free space before realization. |
| Physical closure | Omits the emulator image and system AVD. |
| Trusted caches | Uses the official cache plus `nix-community` when its signing key is trusted. |
| Source fallback | Rejects non-fixed third-party builds and realizes with `fallback = false`. |
| Local plan | Caps all builds at 512, non-fixed builds at 256, and permits reviewed classes. |
| Dependency preflight | Fetches missing fixed-output sources sequentially before the runtime closure. |
| Build jobs | Uses at most two local build jobs by default. |
| Growth budget | Cancels realization after 24 GiB of net Nix-store growth. |
| GC roots | Uses `--no-link`; the realized runtime remains garbage-collectable. |
| Nix realization | Daemon garbage collection starts below `min-free`. |
| Harness runtime | Requires 16 GiB free before creating emulator state. |
| Runtime growth | Stops the emulator run after 8 GiB of temporary or evidence growth. |
| ADB commands | Stop after 120 seconds; cleanup diagnostics stop after 5 seconds. |
| Emulator | Uses a temporary AVD and removes it on every exit path. |
| Evidence | Three logs are capped at 1 MiB; diagnostic JSON is capped at 64 KiB. |

Override the runtime threshold only for constrained test hosts:

```sh
P2P_VPN_ANDROID_E2E_MIN_FREE_BYTES=8589934592 \
  nix run .#android-e2e -- --preflight
```

`0` disables only the harness check. It does not change Nix daemon behavior.

Raise the realization budget only after reviewing the dry-run plan:

```sh
P2P_VPN_ANDROID_E2E_MAX_STORE_GROWTH_BYTES=34359738368 \
  nix run .#android-e2e -- --scenario pairing-traffic
```

The hard maximum is 64 GiB. Budget exhaustion cancels Nix and exits `75`.

Lower the emulator runtime budget on constrained hosts:

```sh
P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES=4294967296 \
  nix run .#android-e2e -- --scenario pairing-traffic
```

The runtime default is 8 GiB and its hard maximum is 32 GiB.

The watchdog checks both temporary state and the evidence filesystem. It
stops the run with exit `75`, then removes the emulator and private state.

Client `--option min-free` overrides may be ignored for untrusted users.
Set the values in NixOS configuration so they apply while realizing the closure.

Use `--no-link` for verification builds. It avoids a persistent `result` GC root.

`nix run .#android-e2e` realizes only a lightweight launcher first.

The launcher avoids unrelated configured caches. It stops if the official
cache is unavailable, the plan is unbounded, or realization exceeds its budget.

Crate and Maven inputs are fetched before large Android archives. A failed
source fetch therefore stops before the runtime closure is realized.

`android-e2e-runtime` is an internal package. Use the launcher instead of
building that package directly.

Inspect and reclaim unreachable paths:

```sh
df -h /nix/store
nix store gc
```

## Debug Automation

The debug APK exposes one ordered-broadcast receiver for the E2E harness.

The receiver requires `android.permission.DUMP`, which authorized ADB shell
holds. It is absent from non-debug source sets.

| Command | Input | Result |
| --- | --- | --- |
| `status` | None | Structured profile, VPN mode, connection, pairing, and path state |
| `diagnostics` | None | Production aggregate-only diagnostic report |
| `create-profile` | `network` | Queues normal encrypted profile creation |
| `connect` | None | Starts the normal VPN flow after user consent |
| `disconnect` | None | Stops the normal VPN flow |
| `open-pairing` | None | Opens the existing pairing protocol |
| `join-pairing` | `code` | Joins through the existing pairing protocol |
| `approve-pairing` | Optional `hostname` | Approves the visible candidate |
| `reject-pairing` | None | Rejects the visible candidate |

Responses are schema-versioned JSON encoded as base64 in broadcast result data.

Status never includes config JSON, private keys, membership keys, or receipts.
It can include an active pairing code; the harness keeps that in private state.

Diagnostics uses the production report and cannot include those status-only
identity or pairing fields.

The isolated E2E scenario can also supply one private bootstrap router.

That debug-only input never configures the Linux overlay peer as a static peer.

## Verification

Run the complete Android gate:

```sh
nix build .#checks.x86_64-linux.android --no-link -L
```

The gate covers:

| Layer | Assertions |
| --- | --- |
| Rust host tests | Profile, artifacts, validation, secret unlink, runtime context |
| Rust cross build | arm64 and x86_64 API 26 JNI libraries |
| Java unit tests | RPC JSON, approval, status, underlay selection, diagnostics |
| Android lint | Debug variant static analysis |
| APK | Dual ABI, 16 KiB alignment, debug ID, signing, min/target SDK |
| Manifest | LAN permission; always-on enabled; automation protected by `DUMP` |

## Device E2E

1. Build and install with `nix run .#android-install`.
2. Create the same network name as a Linux instance.
3. Connect and approve local-network access and VPN permission.
4. Pair from only the code and approve the candidate.
5. Read both overlay addresses with `p2p-vpn peers`.
6. Ping Android from Linux and Linux from `adb shell`.
7. Change the Android underlay and confirm automatic recovery.

Record peer IDs, overlay addresses, selected paths, and packet counts.

Do not record the private identity, membership key, or pairing code.

## Recorded E2E

The recorded runs through 2026-08-31 used a clean API 35 x86_64 emulator.

### Profile Lifecycle

| Check | Result |
| --- | --- |
| Creation | JNI created an encrypted dual-stack profile |
| Process death | The same identity and addresses were restored |
| Update install | `adb install -r` preserved the profile |
| Replacement install | A second replacement preserved the profile |
| Cleanup | Emulator and private harness state were removed |

### Always-On Lifecycle

| Check | Result |
| --- | --- |
| Android ownership | Connected tunnel reported always-on mode |
| Disconnect guard | In-app disconnect left the same runtime connected |
| Update restart | Fresh process restored the same encrypted profile |
| System action | `VpnService.SERVICE_INTERFACE` started the replacement process |
| Lockdown guard | Runtime stopped with an actionable split-tunnel error |
| Lockdown recovery | Runtime reconnected without app interaction |
| Cleanup | Always-on setting and temporary AVD state were removed |

### Linux Pairing

The traffic run also used a rootless Linux fixture.

Android was given one private discovery bootstrap and no overlay peer address.

| Check | Result |
| --- | --- |
| Android workflow | Created profile, connected VPN, joined by code |
| Discovery | Private Kademlia provider lookup found the inviter |
| Approval | Linux verified and approved the Android candidate |
| Enrollment | Android applied artifacts and reconnected automatically |

### Overlay Traffic

| Direction | Protocol | Result |
| --- | --- | --- |
| Linux to Android | IPv4 ICMP | 5/5 replies |
| Linux to Android | IPv6 ICMP | 5/5 replies |
| Android to Linux | IPv4 ICMP | 5/5 replies |
| Android to Linux | IPv6 ICMP | 5/5 replies |

### Path Isolation

Each isolated run repeated all four traffic checks.

| Mode | Required Path | Excluded Paths | Result |
| --- | --- | --- | --- |
| `quic-stream` | One direct QUIC stream | TCP, datagram, relay | 20/20 replies |
| `tcp-stream` | One direct TCP stream | QUIC, datagram, relay | 20/20 replies |
| `owned-quic` | Owned QUIC packet session | UDP fallback, relay | 20/20 replies; 23 packet delta |
| `relay-only` | One circuit-relay path | Every direct path | 20/20 replies; 24 relay packets |
| `relay-to-direct` | Circuit relay, then direct TCP | QUIC and datagram paths | 40/40 replies; no restart |

The relay run also recorded two established circuits and no configured
overlay peer address.

### Relay Promotion

The promotion run began with the direct endpoint bound but unavailable.

Both runtimes selected circuit relay and carried 20 measured packets.

| Check | Result |
| --- | --- |
| Direct availability | Fixture opened a bounded TCP proxy after relay traffic |
| Convergence | Direct TCP selected in 21.670 seconds |
| Android runtime | Generation stayed at `2`; no restart occurred |
| Linux fixture | Original process remained active |
| Promoted traffic | 20/20 replies; 20 direct packets per endpoint |
| Relay backup | One healthy relay path remained available |

### Cleanup

| Check | Result |
| --- | --- |
| Readiness | All four one-packet probes passed on the first attempt |
| Steady traffic | Every direction and address family passed 5/5 |
| Processes | Emulator and Linux fixture stopped |
| Private state | Pairing code, keys, and runtime state removed |
| Evidence | Three redacted logs stayed below 1 MiB; diagnostic JSON stayed below 64 KiB |

### Underlay Recovery

The recovery run used the automatic path policy and one continuous runtime.

| Check | Result |
| --- | --- |
| Wi-Fi to cellular | Traffic resumed in 11.170 seconds |
| Total-loss detection | Loss was recorded in 1.070 seconds |
| Outage hold | Runtime stayed alive for 5 seconds without an underlay |
| Cellular recovery | Traffic resumed in 3.170 seconds |
| Wi-Fi restoration | Preferred-path traffic resumed in 6.270 seconds |
| Runtime generation | Stayed at `2`; no restart occurred |
| Recovery failures | `0` across four additional recovery requests |
| Traffic | 5/5 IPv4 and IPv6 replies in both directions after each usable transition |
| Final path | One direct QUIC stream; no duplicate TCP path |
| Process use | 2.333 CPU seconds, 51,933 KiB PSS, 12 threads |
| Run storage | 956 MB net growth; temporary source and evidence removed |

This proves always-on lifecycle recovery, isolated emulator pairing,
bidirectional dual-stack traffic, owned QUIC, compatibility streams, relay
behavior, and underlay recovery.

It does not prove public NAT traversal or physical-device behavior.

## Current Exclusions

| Area | State |
| --- | --- |
| Multiple simultaneous profiles | Excluded |
| Android system overlay DNS | Excluded |
| Always-on split tunnel | Included; API 29+ mode detection |
| Lockdown / blocked connections | Excluded |
| Custom route-grant UI | Excluded |
| Identity import/export UI | Excluded |
| Play Store release pipeline | Excluded |
| Automated emulator underlay changes | Proven on API 35 x86_64 |
| Physical arm64 device | Not yet proven |
