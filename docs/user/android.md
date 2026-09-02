# Android

The Android app stores up to 16 p2p-vpn networks.

Each network has its own identity and membership. Enabled networks connect
concurrently through one Android VPN interface.

The current artifact is a debug build for test environments.

## Support Matrix

| Item | Requirement |
| --- | --- |
| Build host | `x86_64-linux` with Nix flakes |
| Android ABI | `arm64-v8a` or `x86_64` |
| Android version | Android 8.0 / API 26 or newer |
| Always-on status reporting | Android 10 / API 29 or newer |
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

## App Layout

| Screen | Purpose |
| --- | --- |
| **Networks** | Lists saved networks, hostnames, live states, and enable switches. |
| **Add network** | Opens the create and code-join choices. |
| **Create network** | Creates a new independent overlay. |
| **Join by code** | Discovers an inviter and creates the signed local profile. |
| Network details | Shows identity, addresses, pairing, peers, and removal. |

Tap a network row to open its details. Use its switch only when changing
whether that network should run.

## Join an Existing Network

Joining needs only the pairing code. Do not create an empty Android network
first, and do not enter the Linux network name by hand.

### 1. Open Pairing on Linux

```sh
sudo p2p-vpn pair open --instance runners
```

Keep the displayed operation ID and pairing code.

### 2. Join on Android

1. Open **Networks** and select **Add network**.
2. Select **Join by code**.
3. Check or edit the proposed device hostname.
4. Enter the code and select **Join by code**.

Android searches the LAN first, then public libp2p discovery. The pairing code
authenticates the inviter before any profile is accepted.

### 3. Approve on Linux

Inspect the candidate:

```sh
sudo p2p-vpn pair status OPEN_OPERATION --instance runners
```

Compare the peer ID and requested hostname, then approve it:

```sh
sudo p2p-vpn pair approve \
  OPEN_OPERATION APPROVAL_ID \
  --instance runners
```

The signed network profile appears on Android in the disabled state. The app
does not require a network name, overlay IP, route, or bootstrap address.

### 4. Enable the Network

Turn on the switch on the network detail page or the **Networks** page.

For the first enabled network, Android may request:

1. Local-network access on Android API 37 or newer.
2. Android VPN consent.
3. Connection notifications on Android API 33 or newer.

The switch records desired state. Confirm the adjacent status reaches
**Connected** before treating the overlay as available.

## Create a New Network

1. Open **Networks** and select **Add network**.
2. Select **Create network**.
3. Enter the exact overlay network name.
4. Select **Create network** again.

The new network opens disabled. It receives a separate identity, membership
state, routes, hostname, and pairing state.

Enable it before creating a pairing code for another member.

## Hostnames

New profiles propose a DNS-safe hostname from the Android device name.

| Priority | Source |
| ---: | --- |
| 1 | User-visible Android device name |
| 2 | Manufacturer and model |
| 3 | `android-device` |

Letters become lowercase, separators become `-`, and the label is limited to
63 ASCII bytes. The join form allows changing it before signing.

An inviter rejects a conflicting hostname. Choose another label and retry.

Existing profiles and signed hostnames are never renamed automatically.

## Enable or Disable

Each network switch is the normal connection control. There is no separate
Connect or Disconnect action.

| Desired state | Runtime behavior |
| --- | --- |
| On | Starts or joins the shared Android VPN. |
| Off | Performs no discovery, routing, or packet queue work. |

Turning on another network rebuilds the shared TUN with the complete enabled
set. Each enabled network keeps isolated identity, routes, and queues.

Turning off the final network stops a manually owned VPN. Android always-on
ownership may retain an idle foreground service for later restoration.

### Live Status

| Status | Meaning |
| --- | --- |
| **Disabled** | Desired state is off. |
| **Starting** | Runtime setup or discovery is in progress. |
| **Connected** | The network runtime has a usable peer path. |
| **Degraded** | Desired state is on, but service or path state is incomplete. |
| **Recovering** | A failed path or underlay is being replaced. |

The switch shows desired state. The text status shows observed connectivity.

## Network Details

| Area | Contents |
| --- | --- |
| Identity | Hostname, overlay IPv4/IPv6 addresses, and peer ID. |
| Pair | Create and copy a code; review and approve incoming requests. |
| Peers | Hostnames, addresses, connection state, selected path, and provenance. |
| Removal | Confirmed deletion of identity, membership, and runtime state. |

Peer data is live only while the network is enabled. The list is bounded and
reports when additional peers were omitted.

### Remove

Open the network details, select **Remove network**, and confirm the warning.

Removing the final network returns the app to the empty **Networks** screen.
This operation permanently deletes that network's private identity.

### Rename

Protocol network names cannot be renamed in place.

The name identifies the overlay and DNS namespace. Create and pair a replacement
network before removing the old one.

## Always-On VPN

Enable at least one network before enabling always-on mode. This grants VPN
consent and confirms the saved network collection is readable.

### Enable

1. Open Android **Settings**.
2. Open **Network & internet** then **VPN**.
3. Open the settings for `p2p-vpn`.
4. Enable **Always-on VPN**.
5. Leave **Block connections without VPN** disabled.

Android starts the foreground service after reboot, process death, and app
updates. The app restores every enabled network without another prompt.

### Split-Tunnel Behavior

| Traffic | Route |
| --- | --- |
| p2p-vpn overlay prefixes | Android TUN interface |
| Normal internet traffic | Existing Wi-Fi, Ethernet, or cellular route |
| p2p-vpn transport sockets | Physical networks, outside the TUN |

The app status shows **Always-on VPN** while Android owns the lifecycle.

Per-network switches still control the desired set. Disable always-on mode in
Android Settings when Android should stop owning the VPN service.

### Blocked Connections

Do not enable **Block connections without VPN**.

p2p-vpn is an overlay split tunnel, not a default-route privacy VPN. Android
lockdown would block normal internet traffic for other apps because p2p-vpn
does not provide a default route.

On Android 10 and newer, the app detects lockdown and remains stopped with an
actionable status instead of entering a reconnect loop.

## Invite a Linux Member

### 1. Create a Code on Android

Open the enabled network's details and select **Create code**.

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

Follow [Pairing](pairing.md) to render permanent native Nix artifacts on Linux.

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

Linux DNS members can resolve the Android member as:

```text
ANDROID_HOSTNAME.runners.p2p-vpn.internal
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
| Network row | Desired switch, hostname, and observed per-network state |
| Detail identity | Network name, stable hostname, addresses, and peer ID |
| Peer state | Local, connecting, connected, recovering, or disconnected |
| Selected path | Datagram, QUIC stream, TCP stream, or circuit relay |
| Path origin | Configuration, LAN, public routing, hole punching, or relay |
| Membership | Local configuration, peer configuration, or signed record |
| Pairing | Discovery, approval, application, or completion |

## Network Changes

The service watches non-VPN networks with internet capability.

It prefers validated Ethernet, then Wi-Fi, cellular, and Bluetooth.

The app's own UID is excluded from the TUN. Internal QUIC, TCP, relay, DNS,
and discovery sockets therefore remain underlay traffic.

The TUN interface and native runtime stay active when the selected underlay
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
| Lifecycle | Service uptime, profile readability, connection, always-on, lockdown, runtime generation |
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
| Identities and network collection | AES-GCM encrypted with Android Keystore |
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
| Stored networks | 0 to 16 isolated identities |
| Simultaneous networks | Up to 16 enabled networks through one TUN |
| Route overlap | Rejected before activation; the previous active set remains intact |
| Inter-network forwarding | Never performed; networks are isolated |
| Network rename | Not supported; create and pair a replacement network |
| Android system DNS | Not integrated; Linux members still resolve Android hostnames |
| Always-on VPN | Supported as a split tunnel; Android 10+ reports mode state |
| Lockdown / blocked connections | Unsupported; normal internet must remain outside the overlay |
| Custom route grants | No Android UI |
| Identity import/export | No Android UI |
| Release distribution | Debug APK only |
| Hardware validation | API 35 x86_64 emulator and API 37 arm64 device |
