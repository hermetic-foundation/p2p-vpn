# Android Architecture

The Android target reuses the Rust protocol and runtime.

Java owns Android lifecycle, permissions, persistence, and the VPN interface.

## Component Map

```text
MainActivity
  -> HomeScreen / AddNetworkScreen / NetworkDetailScreen
  -> desired-state network switches
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
| `android/app/src/main/java/.../MainActivity.java` | Navigation, permissions, and screen coordination |
| `android/app/src/main/java/.../HomeScreen.java` | Network list and per-network desired-state switches |
| `android/app/src/main/java/.../AddNetworkScreen.java` | Create and profile-free code-join forms |
| `android/app/src/main/java/.../NetworkDetailScreen.java` | Identity, pairing, peers, state, and removal |
| `android/app/src/main/java/.../DeviceHostname.java` | DNS-safe Android device-name normalization |
| `android/app/src/main/java/.../NetworkUiState.java` | Desired-versus-observed state projection |
| `android/app/src/main/java/.../PeerSnapshot.java` | Bounded native peer-snapshot parser |
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
| `nativeValidateStartNetworks` | Preflights an enabled runtime set without a TUN |
| `nativeStartNetworks` | Validates and starts an isolated runtime set over one TUN |
| `nativeStatus` | Aggregate and per-network phase plus control status lines |
| `nativeNetworkChanged` | Invalidates stale paths and rediscovers without stopping TUN |
| `nativeStop` | Requests shutdown and joins the runtime thread |
| `nativePairRpc` | Calls the existing daemon pairing state machine |
| `nativePairRpcForNetwork` | Calls one selected network's pairing state machine |
| `nativeApplyPairingArtifacts` | Applies signed artifacts to the profile |
| `nativeJoinProfileByCode` | Discovers an inviter and returns one authenticated profile |
| `nativeCancelProfileJoin` | Cancels the active profile-free discovery operation |

Every JNI response uses a bounded JSON envelope:

```json
{"ok":true,"value":{}}
```

Native panics are caught before crossing JNI.

`nativeStart` remains a compatibility entry point for one network.

Single-network status and `nativeNetworkChanged` retain their legacy aggregate
detail, line, and direct change-object shapes. Multi-network change responses
wrap successful per-network results in a `networks` array.

`nativeStartNetworks` accepts schema version `1`, one stable presentation
address pair, and 1 to 16 network records. Each record carries a canonical UUID,
validated config JSON, and an isolated pairing and membership state directory.

Unknown fields, future schemas, repeated identities, network names, DNS zones,
UUIDs, or state directories are rejected before the TUN workers start.

The service calls `nativeValidateStartNetworks` before establishing Android's
TUN. The preflight also builds the dispatch registry, so overlapping addresses
or routes fail before Android replaces the active VPN interface.

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
JNI supervisor creates one port and runtime task per validated network.

The runtime tasks share one bounded Tokio worker pool. They do not share
identity, control channels, state files, packet queues, or route ownership.

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

Route preparation records the target network generation and installed routes.
The commit checks those network-local values again under the write lock.

An unrelated network update can advance the global dispatch generation first.
The pending update then rebases onto the latest map and validates all overlaps
again before commit.

A stale update for the same network still fails closed.

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

Native status is an internal app contract. Per-network runtime lines can include
peer IDs and path endpoints, while supervisor lines use stable network UUIDs.

The exported Android diagnostic report does not serialize those raw lines. It
parses only bounded aggregate counters and fixed event names.

| Scope | Counters |
| --- | --- |
| Supervisor | Malformed and unroutable outbound packets |
| Supervisor | Source-ownership mismatch drops |
| Per network UUID | Outbound enqueued, queue drops, oversized drops |
| Per network UUID | Outbound and inbound presentation-translation drops |
| Per network UUID | Inbound malformed, ownership, queue, and oversized drops |
| Per network UUID | Packets discarded during removal or shutdown |
| Per network UUID | Inbound written, backpressure drops, and write failures |
| Per network UUID | Rejected live route updates |

These counters distinguish route failures from queue pressure. Raw status must
not be copied into an exported diagnostic artifact.

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

The native lifecycle can activate concurrent networks and isolate a failed
runtime task. The Android service launches every enabled collection entry with
`nativeStartNetworks`; `nativeStart` remains a compatibility entry point.

The shared Android TUN uses the collection's stable presentation IPv4 and IPv6
addresses. It adds each enabled network's extra local addresses, deduplicates
their Android routes, and uses the smallest enabled MTU.

Disabled entries remain encrypted and inspected but receive no runtime, packet
queues, discovery work, or TUN routes. A collection with no enabled entries
stays disconnected without entering the reconnect backoff loop.

`VpnService.Builder` routes are fixed when the interface is established.
Runtime-learned custom prefixes currently update native dispatch only. The
multi-network lifecycle must re-establish or replace the TUN before reporting
those prefixes as active Android routes.

## Profile Lifecycle

```text
no collection
  -> create locally or join from only an authenticated pairing code
  -> normalize a user-visible Android device hostname
  -> allocate stable presentation addresses
  -> store a disabled entry in the versioned network collection
  -> add up to 15 more isolated networks
  -> enable any non-overlapping subset
  -> reconcile the enabled set through one VpnService
```

Each entry contains its private libp2p identity and learned membership config.

Private state never enters Android resources or the APK.

| Stored form | Load action |
| --- | --- |
| Legacy raw JSON profile | Wrap unchanged config in one enabled entry. |
| Collection schema 1 | Preserve entries and add stable presentation addresses. |
| Collection schema 2 | Validate and load directly. |

A legacy network UUID derives from its existing network name and peer ID.

The deterministic UUID makes migration restart-safe if Android terminates the
process between moving runtime state and writing the collection.

Every new entry receives a random canonical UUID. The collection stores one
selected UUID and an independent enabled flag per entry.

Selection scopes identity display and pairing. It does not change activation.

New user-created and profile-free joined entries start disabled. Legacy raw
profiles remain enabled during migration to preserve deployed behavior.

`DeviceHostname` prefers `Settings.Global.DEVICE_NAME`, then manufacturer and
model, then `android-device`. It emits one lowercase DNS label of at most 63 bytes.

Existing profiles are inspected without rewriting their signed hostname.

The detail screen may explicitly rename an existing profile. JNI rewrites only
`network.dns.hostname` and verifies that Peer ID, addresses, and routes match.

The daemon then signs a monotonic hostname record with the existing identity.
Enabled profiles reconnect so the new record propagates without deleting state.

Protocol network names are immutable in the UI. Renaming changes the overlay
and DNS namespace, so users create and pair a replacement network.

The profile stores `network.dns.hostname` while leaving the Android DNS
listener disabled. Pairing establishes the initial label; later labels are
self-signed by the unchanged member identity.

### Desired and Observed State

The switch is the sole normal desired-state control. It never represents
observed connectivity.

| Projection | Inputs |
| --- | --- |
| Disabled | Desired state is off. |
| Starting | Desired state is on and the runtime phase is starting. |
| Connected | Desired state is on and the runtime phase is running. |
| Recovering | Runtime detail reports bounded recovery or retry. |
| Degraded | Desired state is on without a complete running path. |

`MainActivity` tracks permission and mutation requests separately from snapshots.
The UI disables conflicting mutations until the service publishes the result.

Enabling the first network requests permissions and starts the shared service.
Changing later entries reconciles the complete enabled set through one TUN.

Disabling the last entry stops a manually owned service. Always-on ownership may
retain an idle foreground service while every network remains disabled.

## Always-On Lifecycle

The manifest explicitly advertises always-on support.

`P2pVpnService` is exported only through the signature-protected
`android.permission.BIND_VPN_SERVICE` boundary.

Android starts `P2pVpnService` without a network-switch action. The service
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
| Process killed with connection requested | Return `START_STICKY` and restore enabled set |
| App update or reboot | Load the same collection and enabled flags |
| API 33+ mode event | Apply authoritative always-on and lockdown state |

Android API 29 exposes `isAlwaysOn()` and `isLockdownEnabled()`.

System starts recognize both actionless intents and `VpnService.SERVICE_INTERFACE`.
The latter is used when Android restarts the VPN after an app update.

API 33 and newer also deliver `VPN_MANAGER_EVENT` with
`EVENT_ALWAYS_ON_STATE_CHANGED` and `VpnProfileState`.

Polling can briefly report manual mode while Android replaces the service.
While a connection is requested, `VpnMode.stabilize` retains the last positive
API 33+ ownership observation until the manager event resolves the transition.

Older system starts retain always-on ownership from the start intent.

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

## Pairing Transactions

Android reuses the platform-neutral pairing protocols. No Android-specific
wire protocol exists.

### Profile-Free Join

The add workflow accepts a code and proposed hostname. It does not accept a
network name, overlay address, route, or bootstrap peer.

```text
generate provisional identity
  -> search mDNS during the LAN-first grace period
  -> search public IPFS Kademlia providers
  -> authenticate the inviter with pairing-code protocol v2
  -> submit the signed hostname request
  -> wait for inviter approval
  -> verify signed artifacts and conflicts
  -> encrypt one disabled profile atomically
```

At most 128 discovered candidates, eight addresses per peer, 32 pending hellos,
and 512 total attempts are retained. The complete operation has a fixed timeout.

The API 35 harness may inject one bounded direct candidate because emulator NAT
does not bridge host mDNS and the private fixture cannot publish to public IPFS.

That input is available only through the `DUMP`-protected debug receiver. It is
an untrusted dial hint; the pairing code still authenticates the inviter.

### Existing-Profile Pairing

An enabled network uses `/p2p-vpn/pairing-code/1` through its runtime RPC.

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

Run the nested network workflow:

```sh
nix run .#android-e2e -- \
  --scenario network-workflow \
  --output ./android-network-workflow-evidence
```

| Area | Assertion |
| --- | --- |
| Navigation | Home, add, create, join, and detail screens are distinct. |
| Enrollment | A code and hostname create the signed profile without a placeholder. |
| Activation | Detail and home switches drive desired state and VPN consent. |
| Status | The enabled network reports observed `Connected` state. |
| Peers | Identity, addresses, path, and provenance render from a live snapshot. |
| Hostname | The managed device name normalizes to a DNS-safe label. |
| Theme | System dark/light changes preserve desired and runtime state. |
| Traffic | IPv4 and IPv6 pass 5/5 in both directions. |

Run concurrent multi-network coverage:

```sh
nix run .#android-e2e -- \
  --scenario multi-network \
  --output ./android-multi-network-evidence
```

The scenario runs two independent rootless Linux fixtures against one Android
VPN service.

| Area | Assertion |
| --- | --- |
| Migration | Legacy raw profile becomes schema 2 without identity change. |
| Pairing | Two networks pair independently through separate discovery routers. |
| TUN | Both active runtimes share one physical descriptor pair. |
| Traffic | Eight concurrent IPv4/IPv6 directions pass 5/5. |
| Overlap | Conflicting network is rejected before activation. |
| Enable state | Disable, update, re-enable, and restore preserve the intended set. |
| Underlay | Wi-Fi/cellular round trip does not restart the runtime. |
| Lifecycle | Process death, APK update, lockdown release, and reboot restore traffic. |
| Isolation | One failed runtime becomes unreachable while its sibling keeps traffic. |
| Bounds | Threads, PSS, queue packets, and queue bytes remain below fixed limits. |
| Privacy | Evidence and diagnostics contain no identities, addresses, or secrets. |
| Cleanup | Always-on state, fixtures, emulator, and private state are removed. |

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
| `select-network` | Network UUID | Selects the identity used by pairing controls |
| `set-network-enabled` | UUID and Boolean | Changes one collection entry's enabled flag |
| `set-profile-join-candidate` | Peer ID and multiaddress | Sets one bounded, one-shot profile-join dial hint |
| `remove-network` | Network UUID | Removes one network; the final entry resets the collection |
| `connect` | None | Starts the normal VPN flow after user consent |
| `disconnect` | None | Stops the normal VPN flow |
| `stage-legacy-profile` | None | Rewrites one test profile into the real legacy format |
| `terminate-process` | None | Terminates the debug process after acknowledging the command |
| `open-pairing` | None | Opens the existing pairing protocol |
| `join-pairing` | `code` | Joins through the existing pairing protocol |
| `approve-pairing` | Optional `hostname` | Approves the visible candidate |
| `reject-pairing` | None | Rejects the visible candidate |

`connect` and `disconnect` remain debug compatibility commands for older
scenarios. The production UI exposes only per-network switches.

Responses are schema-versioned JSON encoded as base64 in broadcast result data.

Status never includes config JSON, private keys, membership keys, or receipts.
It can include an active pairing code; the harness keeps that in private state.

Diagnostics uses the production report and cannot include those status-only
identity or pairing fields.

The pairing-traffic scenario can supply one private bootstrap router. The
network-workflow scenario can supply one direct profile-join candidate.

Neither debug-only input configures the Linux overlay peer as a trusted static
peer. Signed pairing artifacts remain the membership authority.

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
| Manifest | Protected VPN service events, LAN permission, always-on, and `DUMP` automation |

## Device E2E

1. Build and install with `nix run .#android-install`.
2. Open **Add network**, then **Join by code**.
3. Pair from only the code and approve the candidate on Linux.
4. Enable the joined network and approve Android VPN permissions.
5. Read both overlay addresses with `p2p-vpn peers`.
6. Ping Android from Linux and Linux from `adb shell`.
7. Change the Android underlay and confirm automatic recovery.

Record peer IDs, overlay addresses, selected paths, and packet counts.

Do not record the private identity, membership key, or pairing code.

## Recorded E2E

The managed runs through 2026-09-02 used a clean API 35 x86_64 emulator.

### Network Workflow

| Check | Result |
| --- | --- |
| Navigation | Empty home, add, create, join, detail, and populated home rendered |
| Profile-free join | Code alone created the signed disabled profile |
| Hostname | `Managed Test Phone` normalized to `managed-test-phone` |
| Activation | Detail switch requested VPN consent and reached `Connected` |
| Home control | Disable and re-enable changed only the selected network |
| Peer detail | Live identity, addresses, QUIC path, discovery origin, and membership rendered |
| Appearance | Dark and light system modes preserved desired state |
| Traffic | IPv4 and IPv6 passed 5/5 in both directions |

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

### Multi-Network Lifecycle

The 2026-09-02 run used two isolated Linux fixtures and one Android TUN.

| Check | Result |
| --- | --- |
| Legacy migration | Identity preserved in encrypted schema 2 collection |
| Concurrent activation | Two independently paired identities and runtimes |
| Initial readiness | Every leg converged within three bounded attempts |
| Traffic | Every stage passed 5/5 in eight directions and address families |
| Overlap rejection | Candidate, collection, runtime generation, and traffic stayed unchanged |
| Enable state | Disabled sibling route disappeared and restored after re-enable |
| Underlay change | Wi-Fi/cellular/Wi-Fi passed without runtime restart |
| Lifecycle | Process death, update, lockdown release, and reboot restored both networks |
| Failure isolation | One runtime failed; sibling traffic and process generation continued |
| Resource bound | 6 threads, 48,187 KiB PSS, and empty final queues |
| Cleanup | Always-on, emulator, fixtures, logs, and private state passed cleanup |

This proves always-on lifecycle recovery, concurrent isolated networks,
bidirectional dual-stack traffic, owned QUIC, compatibility streams, relay
behavior, and underlay recovery in the managed emulator.

It does not prove public NAT traversal or physical-device behavior.

### Physical App Workflow

An API 37 arm64 device was validated on 2026-09-02.

| Check | Result |
| --- | --- |
| Installation | `adb install -r` installed the validated APK without clearing app data |
| Migration | Existing profile, hostname, identity, addresses, and enabled state were preserved |
| Navigation | Home, add, create, join, and network-detail screens rendered without overlap |
| Hostname | A new join proposed the DNS-safe device hostname `pixel-8-pro` |
| Activation | The per-network switch reached `Connected` and idled correctly when disabled under always-on ownership |
| Peers | Seven bounded rows exposed names, dual-stack addresses, path state, origin, and membership provenance |
| Traffic | IPv4 and IPv6 passed 5/5 in both directions; post-update traffic passed 3/3 |
| Update | A second `adb install -r` restored the enabled network with the same identity and traffic |
| Boot restoration | Android started the service and restored identity plus 20/20 dual-stack replies without opening the app |
| Resources | Connected snapshot reported six Java threads and 81,746 KiB PSS |

This proves the physical app workflow and Linux interoperability.

The interactive mobility, Doze, and 30-minute endurance audit remains separate.

## Current Exclusions

| Area | State |
| --- | --- |
| Multiple simultaneous networks | Included; 1 to 16 entries share one TUN |
| Cross-network forwarding | Excluded by route and packet ownership checks |
| In-place protocol network rename | Excluded; replace and pair a new network |
| Android system overlay DNS | Excluded |
| Always-on split tunnel | Included; API 29+ mode detection |
| Lockdown / blocked connections | Excluded |
| Custom route-grant UI | Excluded |
| Identity import/export UI | Excluded |
| Play Store release pipeline | Excluded |
| Automated emulator underlay changes | Proven on API 35 x86_64 |
| Physical arm64 app workflow | Proven on API 37 |
| Physical mobility and endurance | Requires the interactive device audit |
