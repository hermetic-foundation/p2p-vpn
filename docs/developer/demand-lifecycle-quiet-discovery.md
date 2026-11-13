# Quiet Discovery Scheduling

## Status

Implementation contract, published before native/runtime changes. Production
lifecycle advertisement stays disabled until the integrated activation and
energy gates pass. This document is not a battery measurement.

## Entry Gates

| Gate | Requirement |
| --- | --- |
| Activation | Local capabilities advertise the existing authenticated lifecycle protocol |
| Incoming path | Existing readiness owner verifies a native relay reservation on a current, usable connection |
| Overlay paths | Every currently configured peer has maintenance paused for its current application-demand generation |
| Preparation | A negotiated idle preparation may pause new maintenance before quiet commitment; it cannot drop payloads |
| Pairing | No live code invitation or join operation |
| Serving role | Keep configured relay servers' ordinary maintenance unchanged |

- Cached public status, advertised addresses, accepted peers, and elapsed timers are not incoming proof.
- An unknown/legacy peer is not automatically dormant; preserve its existing compatibility behavior.
- No configured peers is allowed only after the activation and verified-incoming gates pass.
- Genuine application traffic invalidates the maintenance pause atomically; recheck before the next Swarm poll.
- Incoming-path loss, authorization changes, or a live code operation must re-evaluate the gate immediately.

## Native Boundaries

| Owner | Quiet Treatment |
| --- | --- |
| AutoNAT client | Pause starts from the existing probe timer, not the complete behaviour |
| AutoNAT serving | Continue inbound requests, dial-backs, replies, failures, and terminal events |
| Existing AutoNAT work | Drain normally; retain original request IDs, work fences, confidence, and throttle history |
| AutoNAT addresses | Retain observations and normal scheduling state; do not enqueue a timer debt per skipped tick |
| DHT automatic jobs | Pause new record/provider jobs and automatic bootstrap on client-mode DHTs |
| DHT foreground | Keep explicit control, pairing, incoming repair, and essential address/provider publication working |
| DHT serving | Continue inbound protocol handling and current queries; do not replace or disable the behaviour |
| DHT server role | Preserve normal background obligations |
| Diagnostic pause | Keep existing diagnostic query suspension separate; never use it as the production quiet switch |

Pause setters are additive native APIs, defaulting to unpaused. They retain no
second request registry or periodic timer. Resume wakes the existing poller;
existing per-poll and shared query budgets still apply.

## Per-Peer Recovery

- Exclude maintenance-paused peers from periodic targeted data discovery, even when a different peer still needs recovery.
- Cancel only those peers' ordinary data-query owners through the existing native cancellation path.
- Preserve scoped, live control-query ownership and failure cooldowns.
- Keep explicit application recovery, LAN-first selection, incoming-relay repair, and configured reservations available.
- Maintenance events cannot advance application demand or clear negotiated quiet permission.

This change does not declare an unseen/offline legacy peer dormant. Broader
offline-peer retry policy and integrated first-packet recovery remain activation
requirements; suppressing reachability is not an energy improvement.

## Validation

| Boundary | Required Evidence |
| --- | --- |
| Native AutoNAT idle | Repeated polls past a due timer start no probe while paused; cached state is not pending work |
| Native AutoNAT drain | Pausing with an owned request preserves it through failure/completion and does not start a retry |
| Native AutoNAT serving | A paused server accepts a real encrypted probe and completes its dial-back/reply |
| Native resume | Unpause starts eligible work through normal scheduling without clearing confidence or throttles |
| Native DHT idle | Due provider/value jobs remain retained without new automatic queries |
| Native DHT foreground | Explicit queries complete and inbound requests still work while background jobs are paused |
| Native DHT resume | Deferred jobs resume under existing query/capacity/per-poll bounds |
| Runtime gates | Passive capability, missing/stale incoming owner, unpaused peer, pairing, and serving roles prevent entry |
| Mixed peers | Dormant peer is not a recovery target when another peer remains missing |
| Control isolation | Per-peer cancellation preserves exact control owners and their original deadlines |
| Compatibility | Existing default API behaviour, wire formats, minimal JSON, and native Nix options remain unchanged |
| Packaging | Shared Linux/Android runtime and patched native sources match evaluated package sources |
| Energy | Fresh matched replicated paired/reversed cellular app mAh/hour and modem evidence remain required |

Unit and local native socket checks do not prove extended dormancy, public-WAN
availability, mobile roaming, or the measured 10x target.
