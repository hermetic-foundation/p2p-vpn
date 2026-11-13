# Activated Runner Validation

## Activation Boundary

| Surface | Contract |
| --- | --- |
| Ordinary startup | Keep lifecycle advertisement disabled by default |
| Controlled embedding | An explicit `RuntimePlatform` builder enables the existing lifecycle capability |
| Epoch | Use the runner's fresh nonzero epoch; never a persistent identity or caller-provided authority |
| Eligibility | The existing authenticated bilateral negotiation and native readiness gates remain mandatory |
| Configuration | No new JSON, Nix, pairing, or network identity field |
| Deployment | An opt-in test is not a production rollout or an energy result |

The builder selects the implemented policy. It cannot inject quiet permission,
pretend an incoming path exists, skip authentication, shorten idle eligibility,
or replace native transport ownership.

## Observable State

- Startup identifies passive versus explicitly enabled lifecycle policy.
- Peer state reports remote lifecycle support, maintenance pause, and committed quiet permission separately.
- Quiet permission comes from the current application-demand generation, not a cached phase label.
- Read-only diagnostics must not create application demand or renew a path.

## Native Scenario

1. Start a real encrypted circuit-relay server and two production shared runners.
2. Give each endpoint a real reservation and enable lifecycle negotiation through the builder.
3. Exchange real IPv4 packets through packet channels instead of modifying host TUN routes.
4. Stop application traffic and wait through the unchanged 45-second idle threshold.
5. Require both peers to commit quiet and retire their overlay transports while retaining incoming relay access.
6. Send one new IP packet from only one endpoint; require delivery without repeated test probes or destination-side traffic.
7. Repeat with the other endpoint initiating after a new idle period.
8. Verify ordinary passive startup and mixed enabled/passive compatibility remain reachable and do not grant quiet.

Use normal time and real sockets. Do not set quiet flags, inject readiness,
advance a virtual clock, or manually reconnect the peers to make the scenario
pass. Keep all waits bounded and preserve failure state in test evidence.

## Follow-On Gates

| Gate | Evidence Still Required |
| --- | --- |
| Preferred path | Active QUIC promotion, retirement, and recovery alongside TCP/relay fallback |
| Control changes | Membership, revocation, hostname/DNS, and pairing delivery during dormancy |
| Incoming lifetime | Extended idle beyond transport and reservation renewal boundaries |
| Network changes | Automatic LAN-first recovery across controlled outages and real mobile roaming |
| Device integration | Enabled Linux and Android candidates, including screen-off and always-on VPN |
| Energy acceptance | Fresh replicated paired/reversed cellular app mAh/hour at least 90% lower, corroborated by modem energy |

Native tests establish only the boundaries they exercise. Production activation
and the 10x battery goal remain unproven until their own gates pass.
