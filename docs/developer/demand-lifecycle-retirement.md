# Dormant Path Retirement

## Ownership

The negotiated [quiet permission](demand-lifecycle-activity.md) is necessary,
but not sufficient, to close a peer's transports. Recheck outstanding work
and infrastructure dependencies at the point of retirement.

| Resource | Dormant Action | Preserved State |
| --- | --- | --- |
| Owned UDP session | Forget current/retiring sessions | Authorization and cached endpoint metadata |
| Owned QUIC session/connection | Close connection and forget session | Listener, certificate, authorization, cached endpoints |
| Pending packet negotiation | Abort owned tasks; remove pending hellos/cooldowns | Task generations never reset/reused |
| Datagram path inventory | Mark unavailable without failure penalty | RTT/MTU/history; stream candidates remain intact |
| Path probes | Clear pending probes without declaring failure | No maintenance-generated demand |
| Libp2p peer connections | Close only after control-work and infrastructure dependency checks | Network incoming anchor and active relay dependencies |
| Membership/DNS/routes | No removal | Dormant is not revoked or offline membership |

## Fences

- Accepted payload demand clears quiet permission before subsequent retirement decisions.
- Queued or in-flight payload prevents retirement even if a quiet bit remains set.
- Track payload separately from probe requests; a maintenance request cannot indefinitely fence retirement.
- Cancelling connection tasks invalidates their ownership; a late result cannot install a new idle session.
- Reject inbound and completed packet hellos while quiet; maintenance cannot reopen a dormant path.
- Use the existing unsupported-path wire rejection; authenticated wake or payload demand restores eligibility.
- Repeated retirement with no resources is a no-op, not periodic network traffic.
- New demand may establish a fresh path; quiescence does not introduce failure penalties.

## Rollout

1. Implement and test owned datagram retirement behind the existing quiet permission.
2. Native control work and infrastructure-safe stream retirement are implemented behind quiet permission.
3. Wire real incoming-anchor readiness and bounded control-update delivery to dormant disconnected members.
4. Validate incoming wake after public relay idleness before advertising lifecycle support.
5. Measure paired cellular energy only after the production lifecycle is active.

No battery improvement follows merely from implementing these cleanup operations.

## Implementation Evidence

| Check | Result |
| --- | --- |
| Workspace/all-target tests | 1,936 passed; 49 ignored, including opt-in public idle test |
| Payload/probe accounting | Pending probes do not count as application work; actual payload does |
| Real UDP/QUIC regression | Sessions close; listener/certificate persist; new demand permits fresh sessions |
| Cancellation | Both owned task roles cancel; another peer's task remains; old generation is rejected |
| Hello fencing | Quiet rejects inbound/completed hellos; demand restores eligibility |
| Clippy | Correctness/suspicious/performance groups pass; existing advisory warnings remain |
| Formatting | Changed Rust files pass rustfmt |
| Nix | Package source evaluation passes; no full package/Android build in this milestone |
| Formal proof | No Lean project exists; executable regressions are not a proof |
| Activation/energy | Passive/unadvertised; no deployed savings claimed |

Validation used the cached Nix Rust 1.97.1 toolchain and the existing shared
target directory. A fresh flake-shell build would require uncached dependencies;
it was not substituted with a claim of full reproducible-build verification.
