# Browser Code Reuse

The browser, native runtime, and Linux companion share the
[protocol](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/README.md). This refactor does not change its wire format
or supported profiles.

## What Changed

| Area | Before | After |
| --- | --- | --- |
| WebRTC Direct addresses | Callers repeated checks for numeric IP, UDP port, and certificate hash. | `path::webrtc` parses the prefix. Callers still enforce peer IDs, circuit suffixes, and LAN or public eligibility. |
| Legacy membership pages | The browser and native runtime separately tracked cursors, advertised counts, and restart limits. | `runtime::control::MembershipSnapshotFetch` owns those transitions. Both runtimes use the shared inventory builder. |
| Browser enrollment | Production compiled a convenience method used only by checkpoint tests. | The helper is test-only; production still calls the bound approval path. |

## What Stays Separate

| Shared Core | Browser Adapter | Native Or Companion Adapter |
| --- | --- | --- |
| Pairing messages, checks, and response import | Browser identity storage, approval UI, and enrollment recovery | CLI and daemon operations, credentials, and durable receipts |
| Checkpoint verification and membership authorization | Browser sync scheduling and persistence before forwarding | Native sync scheduling, cancellation, and persistence |
| Address and route validation, packet framing, and path ranking | WebRTC event loop, connection-scoped sessions, and in-app packet I/O | TUN, physical route selection, and companion egress enforcement |

## Regression Coverage

| Check | Evidence |
| --- | --- |
| Legacy membership | Native paging tests and Chromium tests cover continuation, restart, revocation, and failed persistence. |
| Durable approval | Chromium completed code enrollment after checkpoint sync and recovered a committed approval after losing its reply. |
| Checkpoint recovery | Reopening pending credentials left forwarding disabled. An authenticated removal survived another reopen; no VPN packet reached the peer. |
| Companion routing | Isolated Chromium tests covered relay fallback, default-route switch and return, and IPv6 underlay to an IPv4-only reservation owner. |

Keep the existing [browser conformance limits](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/browser-conformance.md).
The Chromium IPv6-only underlay results do not qualify hosts with no
non-loopback IPv4, and isolated DHT tests do not qualify public IPFS.
