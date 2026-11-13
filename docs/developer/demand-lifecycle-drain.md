# Maintenance Drain Before Idle

## Status

Implemented in the shared runtime; production lifecycle advertisement remains
disabled. Local draining is not negotiated dormancy, does not retire a path,
and is not evidence of cellular energy savings.

## Problem

| Owner | Timing |
| --- | --- |
| Path-probe tick | Every five seconds |
| Native stream probe request | Can remain pending for ten seconds |
| Datagram confirmation | Twelve-second timeout |

Overlapping maintenance can keep native work busy at every idle decision.
Waiting for empty work without stopping new maintenance can therefore prevent
bilateral quiet even when no application is using the VPN.

## Local Preparation

| Gate | Requirement |
| --- | --- |
| Support | Current authenticated local/remote lifecycle epochs; no legacy fallback |
| Incoming path | Verified native incoming-anchor ownership |
| Activity | Normal 45-second eligibility based on actual demand, not maintenance |
| Application work | No queued payload or in-flight application delivery |
| Permission | Pause only unnecessary maintenance; do not claim peer agreement |

- Stop starting path/MTU probes and automatic packet-plane hello/capability-refresh retries.
- Let already-owned codec/stream requests finish or fail; never erase native slots to manufacture readiness.
- Do not convert an old unconfirmed probe into new failure/recovery work while draining.
- Continue accepting authorized payloads and completing incoming/existing negotiations.
- Keep current sessions, paths, memberships, DNS, and control connections intact.
- Keep incoming-anchor renewal and bounded incoming-path recovery independent.
- Only committed bilateral quiet can trigger physical retirement.

## Activity Fencing

- Store one paused-generation value in the existing per-peer application-demand record.
- Bind preparation to both record identity and the current non-wrapping demand generation.
- A new accepted payload changes that generation, immediately invalidating the pause.
- No driver tick is required to resume maintenance after real demand.
- Capability/authority invalidation and legacy fallback clear local preparation.
- Exhausted generations never permit a pause or quiet grant.
- Preserve the original last-demand timestamp; draining and wake retries are not activity.

This is a bounded local policy field, not a new request ledger, timer, wire
message, configuration option, or substitute for the authenticated idle exchange.

## Validation

| Scenario | Required Evidence |
| --- | --- |
| Busy maintenance | Drain starts despite pending probes/control work; unfinished native requests still prevent commitment/closure |
| New payload | Generation change immediately resumes maintenance without waiting for a tick |
| Legacy/no anchor/recent activity | No preparation or changed interoperability policy |
| Inbound work | Preparation alone does not reject incoming negotiation or discard payload |
| Quiet protocol | Once work drains, automatic idle completes and physical retirement keeps its native fences |
| Cancellation | Capability changes, authority replacement, and exhaustion cannot preserve a stale pause |
| Resource | One fixed-size field per existing peer; no extra timer or background packet |
| Energy | Fresh replicated cellular mAh/hour comparisons remain mandatory after activation |

## Implementation Evidence

| Check | Result |
| --- | --- |
| Focused lifecycle run | 93 passed: 34 policy, 23 protocol, 10 activity, and 26 dispatcher tests |
| Maintenance drain | Pending maintenance permits preparation but still prevents idle commitment until native work drains |
| Payload race | Real demand immediately invalidates preparation; stale concurrent preparation cannot restore it |
| Incoming negotiation | Preparation accepts incoming hello processing; only committed quiet rejects dormant-path negotiation |
| Probe expiry | Draining does not demote the path, record failure, or start another recovery round |
| Legacy races | Crossed offers and late unsupported results clear quiet for the current epoch; stale epochs cannot disable a replacement |
| Dormant scheduling | 1,000 virtual five-second ticks do not generate lifecycle work or renew demand |
| Physical retirement | Preparation alone cannot authorize closing a path; native transport/control fences remain |

Virtual idle time tests policy and dispatch, not real long-idle reachability.
No updated APK, service deployment, or cellular energy comparison was performed.
