# Shared Path Policy

Status: **Draft**. This generalizes the existing [physical network policy](physical-network-policy.md)
and [public discovery contract](public-discovery-relay-acceptance.md).
See the [browser conformance audit](https://github.com/hermetic-foundation/libp2p-vpn/blob/4ba644cb8835dfaa2b4b39b05821af392164ed93/docs/developer/protocol/browser-conformance.md) for
separate evidence on browser-managed paths and Linux companion egress.

## Egress Rules

| Work | Rule |
| --- | --- |
| Automatic public bootstrap, DHT, and relay infrastructure | Use the OS-selected default physical network. Skip ineligible endpoints instead of leaking onto a secondary interface. |
| Direct member recovery | Try a usable LAN path first. For other direct paths, prefer the OS-selected route; an eligible secondary path may carry member traffic. |
| Explicitly configured relay | Apply explicit configuration and normal path authorization; do not treat it as permission for automatic public work. |
| No verifiable default-network binding | Do not dispatch automatic public work from that adapter. Do not claim strict physical-interface parity. |

DNS, candidate lookup, connection reuse, and relay reservation are subject to
the same rules as their initiating work. Being connected to a peer for another
purpose does not make that connection eligible for public discovery.

## Shared Handoff

| Event | Required behavior |
| --- | --- |
| Active path fails | Recheck eligible LAN candidates, then use bounded recovery on other eligible paths. |
| Preferred route becomes available | Keep the working fallback while authenticating and validating the replacement on its exact transport connection. |
| Remote member must open the replacement | The changed peer requests it over the existing authenticated circuit; the member must not wait for fallback failure. |
| Current replacement validates | Move new sends to the preferred path. Drain bounded in-flight work and retire redundant fallback resources. |
| Replacement fails or route changes again | Keep a still-valid fallback. Ignore results from the obsolete route generation. |

Keep peer identity, membership, and overlay addresses across handoff. Availability,
an ICE candidate pair, and successful authentication are separate facts: none
alone proves traffic followed the current preferred physical route.

The optional [preferred-interface handoff protocol](https://github.com/hermetic-foundation/libp2p-vpn/blob/4ba644cb8835dfaa2b4b39b05821af392164ed93/docs/developer/protocol/preferred-handoff.md)
specifies coordinated relay replacement. The changed peer verifies its own
physical owner and real IP delivery before acknowledging promotion.

## Protocol Boundary

- Keep the existing control and packet wire formats. Reuse authenticated
  capability exchange and connection-scoped validation where they suffice.
- Add versioned, optional signaling only for behavior that requires both peers
  to coordinate. A legacy peer must keep working without the new capability.
- Never take an interface preference or egress claim from a remote peer as
  permission. The local adapter owns OS selection, binding, and invalidation.

## Browser Boundary

The [standard WebRTC API](https://www.w3.org/TR/webrtc/#rtcconfiguration-dictionary)
does not let a page bind a socket to an OS-selected interface. A remote gateway
can enforce its own egress but cannot enforce the browser's first hop.

| Deployment | What It Can Provide |
| --- | --- |
| Standalone WASM page | Shared peer authentication and recovery decisions; direct WebRTC paths remain browser-managed. Strict public operations stay disabled when binding cannot be verified. |
| Local native adapter | Can enforce physical routing for the sockets it owns, with the browser as a frontend. It must provide route changes and connection-bound validation to the shared policy. |

## Acceptance

1. Run the same policy tests across the native and browser adapters, including
   LAN-first recovery, bounded fallback, failed promotion, and stale events.
2. On two real or emulated uplinks, capture packets per interface. Automatic
   public work must put no packets on the secondary while default is usable.
3. Move the default route and restore it. Prove authenticated preferred-path
   recovery, payload continuity, and retirement of redundant fallback traffic.
4. A standalone browser must report an unenforceable policy as unsupported,
   not pass strict tests based on browser candidate statistics alone.
