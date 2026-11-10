# Windows connection-change validation — 2026-11-10

Three independent runs passed both production-runtime fault scenarios, using
fresh identities and sockets each time. This is a native Windows transport test,
with packet channels at the TUN boundary and real authenticated TCP and owned
QUIC datagrams. It does not replace a physical adapter/hotspot handoff test.

## Acceptance and measured recovery

Every recovery requires ten consecutive fresh bidirectional packet pairs. Old
queued packets cannot satisfy the gate because each probe has a new sequence.
QUIC recovery additionally requires increasing owned-QUIC payload counters in
both directions for those probe pairs. Both runtimes remain running throughout.
The complete-outage check verifies that packets cannot bypass the disabled TCP
proxies. The QUIC fixture verifies traffic traverses its proxies before dropping
datagrams; each successful run recorded 106 actual UDP datagrams dropped.

| Injected condition | First fresh bidirectional traffic, across three runs |
| --- | --- |
| Active TCP connection dropped; alternate IP/port enabled | 9.0–11.2 seconds |
| Both TCP paths unavailable, followed by path restoration | 7.45–7.48 seconds after restoration |
| TCP socket remains open but forwarding stops | 10.55–10.57 seconds to alternate path |
| Explicit runtime network-change notification | 0.616–0.626 seconds |
| Active QUIC path silently blackholed | 19.24–19.26 seconds to TCP fallback |
| QUIC reestablished through a different advertised endpoint while proxies stay blackholed | Approximately 44 seconds from the original UDP fault |

The last interval combines the completed fallback gate and the subsequent QUIC
recovery interval; diagnostics between gates add a small amount of overhead.
Individual stage durations, qualifying intervals, probe counts, and run evidence
paths are in [windows-recovery-results.json](windows-recovery-results.json).
The figures describe these runs, rather than a latency guarantee or loss-free
handoff. Probes can be dropped or expire during an outage.

## Failures found and changes

1. The initial TCP scenario recovered from disconnect, total outage, and silent
   loss, but failed its explicit network-reset recovery gate after 60 seconds.
   Windows reported socket error 10048 during reconnects. Loopback addresses
   were excluded from advertised interface networks and therefore missed the
   existing local-TCP fresh-source-port policy. Loopback TCP now uses fresh
   source ports for ordinary peer dials. Mixed/off-link targets and the separate
   DCUtR dial path retain their existing behavior.
2. The corrected QUIC fault fixture delivered TCP fallback after about 19
   seconds, but did not return to QUIC within the additional 90-second gate.
   The connector selected only the first ranked advertised endpoint on every
   retry. It now gives that endpoint priority, then races authenticated
   alternatives within the remaining original connection deadline. A single
   endpoint keeps its full budget. Certificate and binding verification remain
   in place, and the existing pending-negotiation expiry/cancellation still owns
   the connection attempt.

The first QUIC fixture attempt was invalid: port-based endpoint ranking chose
the direct listener instead of the fault proxy. Its traffic guard rejected the
setup. The fixture now reserves all ports together and assigns lower ports to
the primary proxies, keeping the production ranking policy intact. That setup
failure is separate from the subsequent demonstrated QUIC recovery failure.

Failed-attempt logs were retained locally as
`windows-connection-recovery-first.log`,
`windows-connection-recovery-fixed-01.log`, and
`windows-connection-recovery-quic-02.log`. Successful evidence is under
`target/connection-recovery-quic-fixed-03`,
`target/connection-recovery-repeat-04`, and
`target/connection-recovery-repeat-05`. These raw logs and target artifacts are
ignored by Git; the condensed results above are included with the source.

## Direct path to circuit relay follow-up

Three additional native Windows runs passed
`relay::direct_link_loss_uses_relay_bridge_and_returns_to_direct`. Each uses
three production runtimes: two authorized overlay peers and a real libp2p
circuit relay, with an accepted destination reservation before the outage.
The direct peer connection passes through a controllable TCP proxy. No runtime
network-change hook or daemon restart is used.

Each stage requires ten consecutive fresh bidirectional packet pairs, the
required selected path on both peers, and increasing payload counters for that
path in both directions. Cutting the direct connection switched traffic to the
circuit relay in 16.16–16.21 seconds. Restoring the direct connection led to
direct selection in 11.86–11.99 seconds; that latter interval is a path promotion
interval, not a measured outage, because the relay remains available.

The relay runs on loopback and packet devices are in-process. This verifies
real circuit-relay protocol and automatic path selection, but does not verify
WAN/NAT traversal, relay provider availability, or physical adapter changes.
Condensed evidence is in [windows-relay-recovery-results.json](windows-relay-recovery-results.json);
raw snapshots are under `target/connection-recovery-relay-01` through `-03`,
with logs `windows-relay-recovery-01.log` through `-03.log`.

## Reproduce

```powershell
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:P2P_VPN_RECOVERY_OUTPUT = 'target/recovery-new-run'
.\scripts\build-windows.ps1 -Configuration debug -RecoveryTest
```

Or run `cargo test --locked --test connection_recovery -- --nocapture --test-threads=1`
with the Windows linker configured as in the build script. Recovery tests need no
physical NIC changes or administrator-only route changes.

Windows and Linux/WSL `cargo check --tests --locked` passed after the fixes.
The runnable Windows executable was rebuilt against the updated runtime.

## Remaining boundaries

- The Windows CLI does not subscribe to OS network-change notifications. The
  explicit-notification stages call the existing in-process reset hook; other
  stages demonstrate automatic transport-driven recovery without that call.
- These fixtures use known alternate endpoints, configured peer authorization,
  unchanged identities, and in-process packet devices. They do not test a real
  Wi-Fi/Ethernet/hotspot handoff, native default-route changes, discovery of a new
  unadvertised address, public Internet relay availability, or Wintun operation
  during a physical adapter change. The local circuit-relay follow-up above
  covers automatic direct-to-relay-to-direct selection only.
- Existing application TCP-flow continuity, persistent membership resynchronization
  across a partition, and larger-topology/resource qualification are separate
  gates. The full historical test suite was not rerun.
