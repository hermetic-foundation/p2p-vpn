# Capability Retirement Review

## Finding

The sustained-resource fixture's idle compatibility run failed before sampling:
both peers retained TCP connectivity but had no validated capabilities or UDP
packet session within the existing startup deadline. The original outer log is
`/tmp/p2p-vpn-sustained-traffic-idle-regression.log`; its artifact suffix is
`1.749665621a19b660` under the direct-overlay namespace fixture directory.

## Mechanism

| Stage | Pinned Source / Observation |
| --- | --- |
| Duplicate retirement | Application marks a redundant connection retiring and asks Swarm to close it |
| Request dispatch | libp2p-request-response selects among its connected entries by request ID |
| Race window | A request may select the retiring entry before its terminal removal |
| Removal | Request-response removes the entry and fails its outstanding requests |
| Application notification | libp2p-swarm emits `ConnectionClosed` after notifying the behavior |
| Missing action | Previously, the application did not retry unvalidated capabilities at that removal |

Inspected pinned sources: `libp2p-swarm` 0.47.1 and
`libp2p-request-response` 0.29.0 in the local Cargo registry. The original logs
do not contain exact request-to-connection IDs; the controlled test establishes
the mechanism, not the historical assignment of every failed request.

## Correction

Retry capabilities once when all conditions hold:

- The closed connection belonged to the current epoch and was deliberately retiring.
- The peer remains configured and has no accepted capabilities.
- Another established connection remains, with a usable current-epoch owner.

The old connection is removed before retrying. Repeated close events cannot
retry again because the retirement marker is consumed. No new timer, retry map,
queue or recurring background work is added; capability validation is unchanged.

## Verification

| Check | Evidence |
| --- | --- |
| Before-change regression | Expected one retry, observed zero; failed on unchanged runtime |
| Guard coverage | Retry, validated peer, stale epoch, ordinary close, no replacement, retiring replacement, unconfigured peer |
| Duplicate close | No second retry in every guard scenario |
| Pinned behavior dispatch | Two behavior-level connections; removed request fails, retry targets survivor |
| Recovery-event suite | 13 passed before the additional dispatch test; dispatch test separately passed |
| Workspace | Final run: 1502 passed, 39 ignored |
| Required Clippy / format | Final checks passed, including both new regressions |
| Android-native | Offline locked x86_64/API 26 build passed in 2m28s |
| Nix source parity | Evaluated assertions passed with cached tools; not a sandboxed package build |
| Live idle follow-up | Two passes, 56.59 and 81.68 seconds, unchanged deadlines |
| Corrected-runtime traffic smoke | Passed in 101.31 seconds: 500/500, fixed transport, unchanged processes, strict final pings |

None of these live passes logged the new retry; these are compatibility evidence, not
proof that the branch recovered the original race. The deterministic dispatch
test covers retry routing, not real network delivery or all reconnect failures.

## Artifacts

- Before log: `/tmp/p2p-vpn-capability-retirement-before.log`.
- Guard suite: `/tmp/p2p-vpn-capability-retirement-after-registered.log`.
- Dispatch test: `/tmp/p2p-vpn-capability-retirement-dispatch-run.log`.
- Live runs: `/tmp/p2p-vpn-capability-retirement-idle-{1,2}.log`.
- Idle artifact suffixes: `1.e1bacaa302afa731`, `1.7b4cbbbe4550fd6a`.
- Source-check directory: `/tmp/p2p-vpn-capability-retirement-source.oayK5WSF`.
- Final source-check directory: `/tmp/p2p-vpn-capability-retirement-final-source.NngbPvD2`; new regression included in Linux/Android inputs.
- Namespace binary SHA-256: `60deab7bb283ba183b2b34658de358e27372e1cf5bd5bee828fbb94ae5174d6e`.
- Android library SHA-256: `dc9a560a29c74c4d31153615afd4bda35351418091d56370373e9d4cb864dbcc`.

The Android build reused cached source/toolchains, two jobs and no debug symbols
or incremental state. Storage remained about 8.3 GiB. No physical device, public
network, deployment or personal configuration was accessed.

## Remaining Scope

The corrected-runtime smoke passed with eligibility false; artifact suffix
`1.27696ede4be78c2e`, log `/tmp/p2p-vpn-capability-retirement-traffic-smoke.log`.
The two full S3 captures remain unrun. Longer retry/recovery, transport allocation attribution,
lifecycle churn and Android background measurements remain separate open work.
