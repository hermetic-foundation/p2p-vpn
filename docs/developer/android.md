# Android Architecture

The Android target reuses the Rust protocol and runtime.

Java owns Android lifecycle, permissions, persistence, and the VPN interface.

## Component Map

```text
MainActivity
  -> P2pVpnService
     -> Android VpnService.Builder
     -> encrypted ProfileStore
     -> JNI NativeBridge
        -> p2p-vpn-android
           -> shared p2p-vpn runtime
           -> in-process runtime control channel
           -> libp2p discovery and packet paths
```

## Source Layout

| Path | Responsibility |
| --- | --- |
| `android/app/src/main/java/.../MainActivity.java` | Native pair-and-connect UI |
| `android/app/src/main/java/.../P2pVpnService.java` | VPN and recovery lifecycle |
| `android/app/src/main/java/.../ProfileStore.java` | Keystore-backed persistence |
| `android/app/src/main/java/.../PairRpc.java` | Existing pairing RPC shapes |
| `android/app/src/debug/java/.../DebugAutomationReceiver.java` | ADB-only E2E control |
| `crates/p2p-vpn-android/src/lib.rs` | JNI and runtime adapter |
| `src/runtime/tun.rs` | Platform-neutral packet I/O and route hooks |
| `src/runtime/control.rs` | In-process runtime control channel |
| `nix/android.nix` | Cross build, SDK, APK, apps, and checks |

## Platform Boundary

The shared runtime receives a `RuntimePlatform`.

Android supplies packet I/O and marks TUN routes as preconfigured.

| Concern | Linux | Android |
| --- | --- | --- |
| TUN creation | Rust `tun` crate | `VpnService.Builder` |
| Interface addresses | Netlink commands | `Builder.addAddress` |
| Overlay routes | Netlink commands | `Builder.addRoute` |
| Packet read/write | Linux TUN file | Detached Android TUN descriptor |
| Local control | Unix socket | In-process channel |
| Service lifecycle | systemd or CLI | Foreground `VpnService` |

Linux behavior and protocol encodings remain unchanged.

## JNI Contract

| Method | Result |
| --- | --- |
| `nativeCreateProfile` | Minimal validated config and derived routes |
| `nativeInspectProfile` | Peer ID, MTU, addresses, and routes |
| `nativeStart` | Starts one runtime over the supplied TUN descriptor |
| `nativeStatus` | Runtime phase and control status lines |
| `nativeStop` | Requests shutdown and joins the runtime thread |
| `nativePairRpc` | Calls the existing daemon pairing state machine |
| `nativeApplyPairingArtifacts` | Applies signed artifacts to the profile |

Every JNI response uses a bounded JSON envelope:

```json
{"ok":true,"value":{}}
```

Native panics are caught before crossing JNI.

## TUN Ownership

`ParcelFileDescriptor.detachFd()` transfers ownership to JNI.

JNI adopts the descriptor before any fallible string conversion.

The native adapter duplicates it once:

| Descriptor | Use |
| --- | --- |
| Original | Packet writer |
| Duplicate | Blocking packet reader |

Shutdown signals the reader and closes both owners through Rust RAII.

## Profile Lifecycle

```text
no profile
  -> create minimal Rust config
  -> encrypt profile atomically
  -> restore and inspect on startup
  -> connect through VpnService
```

The profile contains the private libp2p identity and learned membership.

It never enters the Android resources or APK.

## Persistence

`ProfileStore` uses separate magic values and authenticated-data domains.

| File | Maximum plaintext | Purpose |
| --- | ---: | --- |
| `profile.enc` | 2 MiB | Identity and runtime config |
| `pairing-operation.enc` | 16 KiB | Active operation metadata |

Both files use `AtomicFile` and AES-GCM.

The AES key is non-exportable in `AndroidKeyStore`.

Files are under `noBackupFilesDir`, and application backup is disabled.

## Pairing Transaction

Android uses `/p2p-vpn/pairing-code/1` through the existing runtime RPC.

No Android-specific pairing protocol exists.

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

The first callback establishes baseline state without restarting.

A better network, current-network loss, or total-loss recovery schedules a
runtime restart after 1.5 seconds.

Repeated startup failures back off to 30 seconds.

The restarted Rust runtime retains LAN-first discovery and relay fallback.

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
| `.#android-emulator` | API 35 x86_64 emulator package |
| `.#android-sdk` | Pinned SDK and platform tools |
| `devShells.android` | Gradle, JDK, SDK, NDK, and ADB |
| `apps.android-install` | Build and install through ADB |
| `apps.android-emulator` | Boot the managed test emulator |
| `apps.android-update-deps` | Refresh pinned Gradle dependencies |
| `checks.android` | Full Android build and verification gate |

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
| Cleanup | Emulator, fixture, and private runtime state are removed |

Select one compatibility stream path:

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
and cleanup results.

`emulator.log` and `fixture.log` are redacted and capped at 1 MiB each.

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
| Emulator | Uses a temporary AVD and removes it on every exit path. |
| Evidence | Redacted emulator log is capped at 1 MiB. |

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
| `status` | None | Structured profile, connection, pairing, and path state |
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
| Java unit tests | RPC JSON, approval, status parsing, recovery policy |
| Android lint | Debug variant static analysis |
| APK | Dual ABI, JNI entry, debug ID, signing, min/target SDK |
| Manifest | Always-on disabled; debug automation protected by `DUMP` |

## Device E2E

1. Build and install with `nix run .#android-install`.
2. Create the same network name as a Linux instance.
3. Connect and approve the VPN permission.
4. Pair from only the code and approve the candidate.
5. Read both overlay addresses with `p2p-vpn peers`.
6. Ping Android from Linux and Linux from `adb shell`.
7. Change the Android underlay and confirm automatic recovery.

Record peer IDs, overlay addresses, selected paths, and packet counts.

Do not record the private identity, membership key, or pairing code.

## Recorded E2E

The 2026-08-29 runs used a clean API 35 x86_64 emulator.

### Profile Lifecycle

| Check | Result |
| --- | --- |
| Creation | JNI created an encrypted dual-stack profile |
| Process death | The same identity and addresses were restored |
| Update install | `adb install -r` preserved the profile |
| Replacement install | A second replacement preserved the profile |
| Cleanup | Emulator and private harness state were removed |

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

### Stream Path Isolation

Each isolated run repeated all four traffic checks.

| Mode | Required Path | Excluded Paths | Result |
| --- | --- | --- | --- |
| `quic-stream` | One direct QUIC stream | TCP, datagram, relay | 20/20 replies |
| `tcp-stream` | One direct TCP stream | QUIC, datagram, relay | 20/20 replies |

### Cleanup

| Check | Result |
| --- | --- |
| Readiness | All four one-packet probes passed on the first attempt |
| Steady traffic | Every direction and address family passed 5/5 |
| Processes | Emulator and Linux fixture stopped |
| Private state | Pairing code, keys, and runtime state removed |
| Evidence | Both redacted logs remained below 1 MiB |

This proves isolated emulator pairing, bidirectional dual-stack traffic, and
the direct QUIC-stream and TCP-stream compatibility paths.

It does not prove underlay changes, owned QUIC, relay-only operation,
relay-to-direct promotion, public NAT traversal, or physical-device behavior.

## Current Exclusions

| Area | State |
| --- | --- |
| Multiple simultaneous profiles | Excluded |
| Android system overlay DNS | Excluded |
| Always-on VPN | Excluded |
| Custom route-grant UI | Excluded |
| Identity import/export UI | Excluded |
| Play Store release pipeline | Excluded |
| Automated emulator underlay changes | Not yet proven |
| Physical arm64 device | Not yet proven |
