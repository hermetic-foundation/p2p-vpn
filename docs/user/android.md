# Android

The Android app creates one p2p-vpn identity, pairs by code, and connects one
saved overlay through Android's VPN interface.

The current artifact is a debug build for test environments.

## Support Matrix

| Item | Requirement |
| --- | --- |
| Build host | `x86_64-linux` with Nix flakes |
| Android ABI | `arm64-v8a` or `x86_64` |
| Android version | Android 8.0 / API 26 or newer |
| Installation | ADB with the device authorized |
| Application ID | `org.hermeticfoundation.p2pvpn.debug` |

## Appearance

The app follows Android's current light or dark system appearance.

Changing the system appearance recreates the activity without replacing the
running VPN service.

## Build

Build the APK from the repository root:

```sh
nix build .#android-debug-apk
```

The result is:

```text
result/p2p-vpn-debug.apk
```

The APK uses a public development signing key.

Do not distribute it as a production release.

## Install

Enable developer options and USB debugging on the device.

Confirm ADB access:

```sh
nix develop .#android -c adb devices -l
```

Install or update the app:

```sh
nix run .#android-install
```

Select one device when several are connected:

```sh
ANDROID_SERIAL=DEVICE_SERIAL nix run .#android-install
```

## Create a Profile

1. Open `p2p-vpn`.
2. Enter the exact overlay network name.
3. Select **Create profile**.

The network name must match the Linux or NixOS member.

For a NixOS instance named `runners`, the default network name is `runners`.

The app creates and displays a new libp2p peer ID and p2p-vpn hostname.

The default hostname is `android-` plus 16 identity-derived hexadecimal
characters. It remains stable while the profile identity is preserved.

It restores the same encrypted profile on later launches.

## Connect

1. Select **Connect**.
2. Allow local-network access when Android requests permission.
3. Approve the Android VPN prompt.
4. Allow connection notifications when Android requests permission.

Local-network access is required for LAN discovery and direct peer paths on
Android API 37 and newer. Denying it leaves the profile disconnected.

The app installs only the overlay IPv4 and IPv6 routes.

It does not replace the device's default internet route.

## Join a Linux Member

### 1. Open Pairing on Linux

```sh
sudo p2p-vpn pair open --instance runners
```

Keep the displayed operation ID and pairing code.

### 2. Join on Android

1. Connect the Android profile.
2. Enter the code under **Pair**.
3. Select **Join by code**.

### 3. Verify on Linux

Inspect the pending request:

```sh
sudo p2p-vpn pair status OPEN_OPERATION --instance runners
```

Compare the candidate peer ID with the ID shown by Android.

The request also shows the Android hostname. It is covered by the signed
pairing request and accepted by default.

### 4. Approve on Linux

```sh
sudo p2p-vpn pair approve \
  OPEN_OPERATION APPROVAL_ID \
  --instance runners
```

The Android app polls, applies the membership, and acknowledges it.

Linux DNS members can then resolve:

```text
ANDROID_HOSTNAME.runners.p2p-vpn.internal
```

Follow [Pairing](pairing.md) to render permanent native Nix artifacts on Linux.

## Invite a Linux Member

### 1. Create a Code on Android

Connect the profile and select **Create code**.

### 2. Join on Linux

```sh
sudo p2p-vpn pair join ANDROID_CODE \
  --instance runners \
  --no-wait
```

### 3. Verify on Android

Compare the candidate with:

```sh
sudo p2p-vpn instance show runners
```

The Android request shows the peer ID, key fingerprint, requested hostname,
and requested overlay IP.

### 4. Approve on Android

Optionally enter an assigned hostname, then select **Approve**.

Select **Reject** for an unexpected request.

## Verify Traffic

List the converged members on Linux:

```sh
sudo p2p-vpn peers --instance runners
```

Ping the Android overlay address from Linux:

```sh
ping -c 5 ANDROID_OVERLAY_IPV4
```

Ping Linux from the Android shell:

```sh
nix develop .#android -c adb shell ping -c 5 LINUX_OVERLAY_IPV4
```

## Physical Device Audit

The audit requires an authorized arm64 device and a reachable Linux member.

Run its non-mutating preflight first:

```sh
nix run .#android-device-audit -- --preflight
```

For an existing paired profile, choose a scenario:

```sh
nix run .#android-device-audit -- \
  --scenario core \
  --network runners \
  --peer-ipv4 LINUX_OVERLAY_IPV4 \
  --peer-ipv6 LINUX_OVERLAY_IPV6 \
  --output ./android-core-evidence
```

Add `--serial DEVICE_SERIAL` when multiple ADB devices are attached.

Use `--pair` only when the app has no saved profile:

```sh
nix run .#android-device-audit -- \
  --pair \
  --scenario core \
  --network runners \
  --peer-ipv4 LINUX_OVERLAY_IPV4 \
  --peer-ipv6 LINUX_OVERLAY_IPV6 \
  --output ./android-core-evidence
```

The pairing code is read from the terminal without echoing or storing it.

### Audit Scenarios

| Scenario | Device transitions | Lifecycle checks |
| --- | --- | --- |
| `full` | LAN to separate hotspot, enable its upstream VPN, return to LAN | All |
| `core` | LAN to cellular or separate hotspot, return to LAN | All |
| `upstream-vpn` | Wi-Fi to VPN-routed hotspot, return to Wi-Fi | None |

`full` remains the default for compatibility.

It requires a separately managed hotspot whose upstream VPN can change while
the Android test device stays connected.

Use two devices when one phone supplies its own cellular connection:

1. Run `core` on the cellular phone.
2. Run `upstream-vpn` on a phone that can join the VPN-routed hotspot.

The two evidence files cover the same physical topologies without requiring a
phone to join its own hotspot.

Run the second scenario with a separate output directory:

```sh
nix run .#android-device-audit -- \
  --scenario upstream-vpn \
  --network runners \
  --peer-ipv4 LINUX_OVERLAY_IPV4 \
  --peer-ipv6 LINUX_OVERLAY_IPV6 \
  --output ./android-upstream-vpn-evidence
```

The `full` scenario prompts for these transitions:

1. LAN to a separately managed hotspot.
2. Hotspot with its upstream routed through a VPN.
3. Return to the original LAN.
4. Screen-off and forced Doze.

Do not start another Android VPN app for step 2.

Android permits only one active VPN service. Apply the VPN to the hotspot's
upstream connection instead.

The `full` and `core` proofs include:

| Check | Default |
| --- | ---: |
| Recovery deadline | 180 seconds per transition |
| Forced Doze | 300 seconds |
| Sustained connection | 1,800 seconds |
| Traffic sampling | Every 60 seconds |
| Packet-loss ceiling | 1 percent |

Those scenarios also recreate the app process and perform `adb install -r`.

The `upstream-vpn` scenario records only its baseline and two transitions.

Every scenario requires identity and bidirectional traffic to persist through
each included transition.

`--allow-short` permits development smoke runs.

Their evidence is explicitly marked `proof_eligible: false`.

| Proof | Interactive transitions | Endurance minimum |
| --- | ---: | ---: |
| `full` | 3 | 30 minutes plus 5-minute Doze |
| `core` | 2 | 30 minutes plus 5-minute Doze |
| `upstream-vpn` | 2 | None |

Automated test confirmations require `--allow-short`.

The result is `OUTPUT/evidence.json`, capped at 2 MiB.

It excludes device serials, models, peer IDs, overlay addresses, pairing codes,
identity material, and underlay addresses.

The runner never creates ADB forwarding or reverse-forwarding rules.

It preserves the installed profile, releases forced Doze, and wakes the screen.

## Connection Status

The status view reports:

| Field | Meaning |
| --- | --- |
| Identity | Network, stable hostname, and peer ID |
| Connection | Starting, connected, recovering, or failed |
| Overlay peers | Members with a supported active path |
| Direct paths | Healthy direct stream or datagram paths |
| Relay paths | Healthy circuit-relay paths |
| Public routers | Public Kademlia routing peers |
| Pairing | Discovery, approval, application, or completion |

## Network Changes

The service watches non-VPN networks with internet capability.

It prefers validated Ethernet, then Wi-Fi, cellular, and Bluetooth.

The TUN interface and native runtime stay active when the selected network
changes or disappears.

Existing transports migrate when possible. Failed transports are rediscovered
through LAN, public routing, hole punching, or relay fallback.

If the in-place recovery signal fails three times, the app restarts only the
native runtime and continues automatically.

After complete connectivity loss, recovery starts when Android reports a new
physical network. Validation affects which available network is preferred.

No profile change or service restart is required.

Public discovery starts after a 60-second LAN-first grace period.

Traffic may pause during that convergence window after an underlay change.

## Export Diagnostics

1. Open `p2p-vpn`.
2. Select **Export diagnostics**.
3. Choose where to create `p2p-vpn-diagnostics.json`.

The report is capped at 64 KiB.

| Section | Included Data |
| --- | --- |
| Lifecycle | Service uptime, profile readability, connection state, runtime generation |
| Underlay | Coarse transport kind, validation, selection, loss, and recovery counts |
| Paths | Aggregate direct, relay, routing-peer, and packet-session counts |
| Queue and drops | Aggregate queue gauges, drops, expiry, fallback, and demotion counts |
| Resources | Process CPU, memory, Java heap, and thread counts |
| Pairing | Whether an operation or candidate is pending |
| Events | Up to 64 coarse lifecycle and recovery event names |

Runtime queue and drop counters describe the current native runtime generation.

| Excluded Data | Examples |
| --- | --- |
| Identity material | Private key, public peer ID, membership key, certificates |
| Peer details | Peer IDs, hostnames, overlay addresses |
| Pairing secrets | Pairing code, fingerprints, signed artifacts |
| Underlay details | Network handles, local addresses, gateways, SSIDs |

Review the file before sharing it outside your organization.

## Stored Data

| Data | Storage |
| --- | --- |
| Identity and network profile | AES-GCM encrypted with Android Keystore |
| Pending pairing operation | Separately encrypted atomic file |
| Runtime pairing state | Private app storage |
| Runtime membership state | Private app storage |
| Received membership key | Consumed into the encrypted profile and unlinked |

Android backup is disabled for the application.

Clearing app data or uninstalling destroys the saved identity.

If the Keystore entry becomes unusable, the app offers **Reset identity**.

## Current Limits

| Limit | Current Behavior |
| --- | --- |
| Simultaneous networks | One saved profile |
| Android system DNS | Not integrated; Linux members still resolve Android hostnames |
| Always-on VPN | Explicitly unsupported |
| Custom route grants | No Android UI |
| Identity import/export | No Android UI |
| Release distribution | Debug APK only |
| Hardware validation | API 35 x86_64 emulator; physical device not recorded |
