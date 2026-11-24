# Browser WASM Validation

The [conformance audit](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/browser-conformance.md) records which
features have live interoperability evidence. Fixtures are not qualification
for public NAT traversal or unattended background operation.

## Repeatable Linux Tests

The Linux harness builds the production WASM and a separate test-hook package.
It checks real Chromium-to-native streams, relay fallback, recovery, membership,
LAN discovery through the companion, and browser pairing against a native daemon.
Each test group runs in a private user, process, and network namespace.

```sh
npm ci --prefix crates/p2p-vpn-browser
P2P_VPN_CHROMIUM="$(command -v chromium)" bash scripts/test-browser-linux.sh
```

For iteration, pass `host`, `relay`, `wire`, or `enrollment` to run
one group. Omit the argument to run the full gate before publishing.

Use the repository toolchain plus Chromium, iproute2, unprivileged user
namespaces, and `/dev/net/tun`. The checks use isolated fixture traffic, not
real-world IPFS relay availability or Internet NAT traversal.

## Normal Daemon Smoke Test

This test runs the browser app against `p2p-vpn up`, not the fixture. It creates
temporary static peers, uses a Linux TUN, checks unauthorized-config rejection,
and sends two HTTP requests across connect, disconnect, and reconnect.

```sh
bash scripts/build-browser.sh
cargo build --locked -p p2p-vpn --features webrtc
export P2P_VPN_BIN="$(realpath target/debug/p2p-vpn)"
cd crates/p2p-vpn-browser
npm ci
unshare --user --map-root-user --mount --net --pid --fork --kill-child=SIGKILL --mount-proc -- bash -ec '
  ip link set lo up
  ip link add br-test type dummy
  ip address add 192.0.2.1/24 dev br-test
  ip link set br-test up
  npm run test:native-app
'
```

Set `P2P_VPN_CHROMIUM` to an installed Chromium binary to override the default
Chrome channel. The existing fixture also checks both dials against the r2 ICE
credential rules in the [WebRTC Direct specification](https://github.com/libp2p/specs/blob/master/webrtc/webrtc-direct.md).

## Inbound Wire Checks

Run these checks from the repository root. The native fixture supplies a
disposable authorized peer and checks the browser's actual protocol responses.
It needs a test interface but not a TUN adapter or public discovery.

```sh
npm ci --prefix crates/p2p-vpn-browser
bash scripts/test-browser-linux.sh wire
```

The runner builds the fixture from the pinned SDK and keeps test-hook
bindings out of the app package.

## Linux Companion

The companion test runs Chromium and the native peer on opposite ends of an
isolated virtual LAN. It verifies mDNS lookup through the companion and WebRTC
authentication with an address-free browser config. It does not use the public DHT.

```sh
bash scripts/build-browser.sh
cargo build --locked --features webrtc
npm ci --prefix crates/p2p-vpn-browser
export P2P_VPN_BIN="$(realpath target/debug/p2p-vpn)"
export P2P_VPN_TEST_NETNS="$(readlink /proc/self/ns/net)"
unshare --user --map-root-user --mount --net --pid --fork \
  --kill-child=SIGKILL --mount-proc node --test crates/p2p-vpn-browser/tests/host-app.mjs
```

## Production Code Pairing

This test runs approval and activation against a native daemon. It checks
legacy activation and distinguishes checkpoint credentials from legacy config.
No public pairing providers are used.

```sh
bash scripts/build-browser.sh
cargo build --locked --features webrtc
npm ci --prefix crates/p2p-vpn-browser
export P2P_VPN_BIN="$(realpath target/debug/p2p-vpn)"
export P2P_VPN_PAIRING_PKG="$(realpath crates/p2p-vpn-browser/web/pkg)"
unshare --user --map-root-user --mount --net --pid --fork --kill-child=SIGKILL --mount-proc -- sh -ec '
  ip link set lo up
  ip link add pv-pairing type dummy
  ip address add 192.0.2.1/24 dev pv-pairing
  ip link set pv-pairing up
  node --test crates/p2p-vpn-browser/tests/pairing-joiner.mjs
'
```

## Earlier Windows Qualification

On 2026-11-13, installed Chrome 156.0.8078.82 ran the Rust VPN core and
smoltcp in `wasm32-unknown-unknown`. WebRTC Direct v2 connected it to the native
Windows daemon on the same computer, using the host's Ethernet IPv4 address.
The native daemon used a real Wintun device; its proof HTTP service bound only
`10.203.0.1`, not the browser asset server's loopback address.

The browser used the existing control and packet codecs, signed legacy
membership, route/source authorization, queue framing, and replay checks.
Both test members held signed authority for their explicit test host routes.
No public bootstrap or relay infrastructure was used.

The recorded run is `target/browser-validation-20261113-212224`.
[Machine-readable evidence](browser-wasm-results.json) retains public peer IDs,
unique response nonces, counters, artifact hashes and the changed-source digest.
Generated private test keys are not in the report.

| Gate | Observed result |
| --- | --- |
| Transport and identity | Actual WebRTC and libp2p identity authentication, then authorized VPN capability exchange. |
| Real application delivery | HTTP 200 with a newly generated nonce, carried as TCP/IP inside `/p2p-vpn/packet/1`. |
| Browser packet counters | Each HTTP transaction: 7 sent/accepted IP packets and 4 received IP packets. |
| Native backend evidence | 10 outbound packets through the selected WebRTC stream backend. |
| Negative authorization | Native returned `Rejected(UnauthorizedSource)`; native source-drop counter was 1. |
| Reconnect | Explicit close, fresh WebRTC/VPN session, same browser identity, different nonce and fresh HTTP 200. |
| JavaScript errors | Zero. |
| Cleanup | Native exit code 0; test adapter and narrowly scoped firewall rule removed. |

The page screenshot was visually inspected after the run. This proves local
browser/native interoperability and real OS IP delivery. It does not prove a
two-computer LAN path, Internet/NAT behavior, or browser background operation.

## Verification

From an elevated Windows PowerShell with Rust, Node.js and installed Chrome:

```powershell
.\scripts\test-browser.ps1
```

The script builds both peers, obtains the matching checksum-verified official
wasm-bindgen CLI, verifies Wintun, creates a disposable test network, runs the
browser checks, validates native counters, and checks cleanup. Use `-SkipBuild`
to qualify existing artifacts. Evidence uses a new timestamped directory.

Other checks completed:

- `cargo check --locked --tests`, with default native features and with `--features webrtc`.
- Browser library tests: real userspace TCP SYN/checksum, bounded packet input
  and HTTP header-injection rejection, supported DTLS curve selection (3 passed).
- Actual native ICE credential parser tests: v2, v1 compatibility, malformed
  credentials and unknown versions (3 passed via `rustc --test tests/webrtc_ice.rs`).
- `scripts/build-browser.ps1 -TestHooks -NativeFixture`, including package generation.
- PowerShell and JavaScript syntax checks, Rust formatting, and whitespace checks.

Existing compiler dead-code warnings remain. The full historical Rust suite,
Nix/Android builds, and Linux runtime qualification were not run in this milestone.
The Nix source fileset includes the new workspace manifest and vendored sources.

## Findings preserved from failed attempts

| Stage | Finding and correction |
| --- | --- |
| Original browser adapter | Timed out before VPN authorization on current Chrome. The replacement uses the [WebRTC Direct r2 handshake](https://github.com/libp2p/specs/blob/master/webrtc/webrtc-direct.md), preserving browser-generated ICE credentials. |
| Native DTLS | ICE connected, then DTLS failed with `invalid named curve`. Backported supported-group selection from [upstream WebRTC 0.13](https://github.com/webrtc-rs/webrtc/blob/v0.13.0/dtls/src/flight/flight0.rs), with explicit rejection when no supported group exists. |
| Fixture membership | Custom host-route advertisements were rejected until the fixture granted `RouteAuthority`. Production authorization was retained. |
| Windows HTTP policy | IP SYNs reached Wintun but the service did not respond until a temporary, narrowly scoped inbound firewall rule was applied. |
| Listener recovery | Windows UDP reset after disconnect could leave the listener without a receive waker. The receive loop now consumes recoverable errors and polls again. |
| Browser teardown | Reconnect delivered fresh HTTP, but stale DOM handlers invoked released WASM closures. Final shared owners now detach handlers; reset explicitly waits for connection closure. An attempted early data-channel close caused a control timeout and was removed. |
| Build harness | Windows parallel WASM linking stalled; one linker thread completed. PowerShell needed the child process handle retained to report its exit code reliably. |

Raw attempt logs and reports remain under `target`; they are not replaced by the
successful verdict. The three vendored patches have their own `P2P_VPN_PATCH.md`
and original licenses. The browser/native peers remain on libp2p 0.56; existing
locked package versions were retained while adding the required dependencies.

## Scope of the earlier Windows run

The earlier Windows run covered bounded IPv4 HTTP text in the foreground.
It did not test cooperation with the Linux discovery companion, IPv6,
pairing, or checkpoint enrollment.

The browser does not install OS routes or redirect normal browser
navigation. The fixture's disposable credentials do not establish
production key management or onboarding readiness.

The next [split-device LAN harness](browser-lan-validation.md) exports the browser
client for a second device and checks returned evidence against native logs.
Its local smoke verdict does not extend this record to two physical devices.
