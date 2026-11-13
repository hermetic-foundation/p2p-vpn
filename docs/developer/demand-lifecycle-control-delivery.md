# Event-Driven Dormant Control Delivery

## Status

Implementation contract, published before the corresponding runtime changes.
Production quiet support remains disabled until delivery and recovery gates pass.
This contract is not evidence of battery savings.

## Ownership

| Resource | Owner And Bound |
| --- | --- |
| Latest capabilities | One shared current snapshot in the existing membership-record sync owner |
| Dirty recipients | Current authorized overlay peers only; bounded by the configured/learned peer admission limit |
| Native requests | Extend the existing control-sync pending map; share its four-flight budget with record pages |
| Per-recipient work | One latest dirty generation, one outstanding capability request, and one finite delivery round |
| Delivery round | At most three attempts within 45 seconds; failed attempts respect backoff |
| Native retirement | Exact peer/request terminal event; semantic expiry never recycles a native slot |
| Scheduling | Existing runtime events/ticks; no new timer or perpetual resend of unchanged capabilities |

- Coalesce superseded local changes before sending; do not retain a payload per queued generation.
- Keep old native IDs as inert tombstones after authority loss, expiry, or supersession.
- An old response cannot acknowledge the current dirty generation.
- Preserve bounded record paging, byte limits, restart limits, and failure history.
- Recheck current authority before dialing, sending, accepting, or retrying.
- Unchanged observations and maintenance do not reset deadlines, budgets, or application activity.
- Actual network changes or a fresh connection may start another finite delivery opportunity.

## Connection Policy

1. Reuse an existing usable secure connection when available.
2. For a disconnected dirty member, use the existing LAN-first discovery/recovery window.
3. Then allow retained direct/relay reachability through the ordinary native transport.
4. Stop initiating attempts when the round expires; retain only the latest bounded dirty marker.
5. Leave packet quiet permission and the application-demand timestamp unchanged.

Control delivery is not authority to install a packet path. Incoming capability
validation, network scope, current membership, and native stream limits still apply.
The minimal JSON and native Nix configuration surfaces do not change.

## Checkpoint Hostnames

| Boundary | Requirement |
| --- | --- |
| Trigger | A validated event can indicate hostname changes at unchanged checkpoint authority rank |
| Compatibility | Put any additive hint on the permissive outer capabilities object, not the strict checkpoint descriptor |
| Authority | A hint is not a signed hostname claim or permission to change membership |
| Transfer | Fetch through the existing authenticated, bounded checkpoint transfer owner |
| Verification | Install only verified monotonic hostname claims; preserve authority rank and incarnation checks |
| Deduplication | Repeated identical hints and completed/no-op transfers cannot restart catch-up forever |
| Failure | Bound attempts/deadlines and keep native request ownership until terminal delivery |
| Legacy | Missing hints retain the existing periodic/connection catch-up behavior |

- Keep a failed hint dirty across at most three 15-second transfer rounds within one original 45-second window.
- Consume a hint only after a verified complete offer, including a valid no-op; bind completion to the inventory captured by that native transfer.
- A newer hint during paging cannot be acknowledged by an older transfer.
- When the finite window expires, idle ticks and repeated identical advertisements do not re-arm it.
- A new authenticated connection can re-arm a failed, unconsumed hint once; repeated events for the same connection cannot renew its window.
- Completed/no-op hints stay consumed across reconnection unless the advertised inventory changes.

## Quarantine Boundary

- Only an active, deadline-bound native control owner may temporarily suspend an old probe block.
- Roster membership, cached advertisements, dirty markers, and native tombstones are not such owners.
- Preserve failure count and the original retry deadline; do not turn the exception into authorization.
- Restore any still-live quarantine when the owning control window ends.
- Reply eligibility is connection-bound; keep packet, route, pairing, and snapshot authority checks unchanged.

## Native Queue And Write Deadline

- Attach the original monotonic control-round deadline to the existing native outbound message.
- Keep normal `send_request` and all existing protocol/frame formats unchanged.
- Refuse an expired message before requesting a substream and before invoking its codec.
- Bound the complete active write, close, and response operation by the earlier native timeout/deadline.
- Preserve exact native request IDs and normal terminal events; queued expiry is not synthetic cancellation.
- Store no second request registry and add no periodic timer or idle-peer keepalive.
- Reuse the same deadline mechanism for capability publication and checkpoint pages.
- A native transport queue may still retain bounded work until its normal terminal event; it must not transmit expired application messages.

## Acceptance

| Scenario | Required Evidence |
| --- | --- |
| Disconnected update | A real native reconnect delivers the latest capabilities without payload demand |
| Coalescing | Many local updates retain only current state and respect the shared four-flight bound |
| Supersession/expiry | Late events release only their native slot and cannot acknowledge replacement work |
| Revocation | Authority withdrawal prevents retry/acceptance and preserves inert native slots |
| Failure | Unreachable peers exhaust a finite round; repeated idle ticks cannot restart it |
| Hostname | Equal-rank rename catches up; unchanged/no-op hints do not create fetch loops |
| Security | Foreign scope, stale incarnation, unauthorized peer, and expired owners gain no rights |
| Quiet behavior | Control updates do not clear quiet or advance application-demand versions |
| Energy | Fresh matched replicated cellular measurements remain required after functional activation |
