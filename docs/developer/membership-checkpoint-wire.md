# Checkpoint Snapshot Transfer

## Scope

| Surface | Contract |
| --- | --- |
| libp2p protocol | `/p2p-vpn/checkpoint-sync/1` |
| Transfer version | `1` |
| Membership authority | Cooperative checkpoint version `2` |
| Protected storage | Version `3`; credentials are never exported in offers. |
| Existing control | Optional `checkpoint` descriptor; old request/response variants are unchanged. |

This protocol transfers a frozen snapshot offer, not a mutation ledger.
See [Checkpoint Lifecycle](membership-checkpoints.md) for activation and remaining
integration work.

## Authentication

1. Bind a fresh core resync challenge to the pinned network anchor.
2. Derive a request-proof key from the separately protected network capability.
3. Authenticate the complete request and transport-authenticated requester ID.
4. Assemble only challenge-, publisher-, cursor-, and digest-matching pages.
5. Pass the decoded offer to core `collect_offer` before accepting authority.

An advertised rank is a scheduling hint, not admission or global-freshness proof.
A decoded offer is not trusted until core snapshot MAC, publisher signature,
scope, challenge, and admission checks succeed.

### Excluded Requesters

A requester with a valid capability proof may fetch its exclusion even when
absent from the publisher's current roster. That exception grants no packet,
route, mutation, or ordinary snapshot-publication authority.

## Messages

| Message | Fields |
| --- | --- |
| Request | Version, challenge, optional offer digest, byte cursor, request proof. |
| Page | Version, challenge, offer digest, byte cursor, total size, base64 page bytes. |
| Rejection | Version, challenge, cursor, bounded rejection reason. |
| Capability | Version, anchor, checkpoint boundary, member count, sync state. |

Requests use fixed 8 KiB page boundaries. JSON messages have a two-byte
big-endian length prefix; unknown fields and malformed page shapes are rejected.

## Bounds

| Resource | Maximum |
| --- | --- |
| Page payload | 8 KiB |
| Framed JSON body | 12 KiB |
| Complete offer | 4 MiB |
| Transfer slots | 32; default 16, including nonce-only retirement slots. |
| Slots per peer | 4 |
| Reserved transfer buffers | 16 MiB |
| Transfer lifetime | 60 seconds; fixed monotonic deadline. |

Completion and cancellation release buffers immediately. Bounded nonce-only
retirement slots expire at the original deadline; they are not durable
revocation records. Paging never extends a transfer's deadline.

## Compatibility

| Case | Result |
| --- | --- |
| Old control descriptor without `checkpoint` | Still decodes as legacy capabilities. |
| Unsupported or malformed descriptor | Reject; do not interpret as authority. |
| Wrong anchor or request proof | Reject before freezing roster data. |
| Busy or failed transfer | Retire affected resources; retry under a fresh resync challenge. |
| Legacy daemon | No checkpoint authority is inferred from its control fields. |

## Pairing Credentials

The signed pairing response can carry an optional `checkpoint` grant. This
contract is implemented and ordinary inviter `PairApprove` uses it for an existing
participating checkpoint instance. Joiner `Accepted` activation, fresh formation,
legacy migration, and export/artifacts remain incomplete.

| Field | Contract |
| --- | --- |
| Grant version | `1`; reject unknown versions. |
| Anchor | Immutable network identity and supported policy/rank versions. |
| Capability | Canonical base64 secret, 32-4,096 decoded bytes; redact diagnostics. |
| Minimum | Approved snapshot rank: revision, roster population, canonical digest. |
| Participants | Current inviter and admitted joiner descriptors, with keys and incarnations. |
| Grant size | At most 16 KiB. |
| Complete response | Existing 32 KiB pairing limit; no full roster in this frame. |

### Inviter RPC

1. Validate the signed request, scope, transport peer, and self-signed hostname intent.
2. Prepare the candidate, signed grant, and route update without publishing authority.
3. Persist protected transaction and admission/cleanup ownership before changing authority.
4. Durably install genuine admission changes before making the response available.
5. Revalidate prepared retries; an unchanged active member does not advance revision.

Changed active-member grants are rejected through re-pair. Cancellation preserves
prior membership, but reconciles a newly owned admission before discarding the
transaction; uncertain removal durability must keep cleanup ownership pending.

### Joiner Activation Contract

1. Validate the approved offer, complete response signature, scope, and actual joiner key.
2. Persist credentials and the minimum rank in owner-only pending state.
3. Fetch the complete authenticated snapshot through paged checkpoint transfer.
4. Require an observed remote offer at or above the pinned rank before activation.

Protected staging and validated fresh-solo replacement are implemented. The
ordinary joiner `Accepted` handler does not activate this sequence yet;
cancellation and startup finalization must precede packet authority.

Empty or self-only legacy startup state can be replaced only through the explicit
approved-solo API. Foreign, revoked, invalid, or future history requires migration;
an existing version-3 scope and secret remain pinned across retries.

Descriptors are provisional discovery/sync seeds, not packet or mutation grants.
An isolated timeout cannot activate this seed; a restart retains the pending gate.
The selected newer snapshot may already exclude the joiner.

### Pairing Compatibility

| Case | Result |
| --- | --- |
| Legacy response | Omit `checkpoint` entirely; preserve existing signed JSON bytes. |
| Checkpoint plus legacy key/records | Reject mixed authority. |
| Generic JSON/Nix config import | Reject checkpoint enrollment instead of exporting its capability. |
| Checkpoint RPC artifacts | Return `Unavailable`; do not emit a legacy-only Nix plan. |
| Old response decoder | Ignoring the new signed field changes signing bytes; verification fails. |
| Superseded approval | Preserve newer pinned authority; do not reinstall its old roster. |

Checkpoint credentials belong in protected runtime state, not Nix store paths,
plain configuration exports, or public snapshot offers. Joiner activation,
formation, migration, and export/artifact integration remain acceptance work.

## Mutation Handoff

| Surface | Contract |
| --- | --- |
| Separate protocol | `/p2p-vpn/checkpoint-mutation/1` |
| Request | Wire version `1`, complete core `SignedMembershipMutation`. |
| Response | Wire version, command digest, applied boundary or bounded rejection. |
| Request timeout | Fixed 5 seconds; no per-request extension. |
| Concurrent streams | At most 32; caller limit is clamped to `1..=32`. |
| JSON body | At most 16 KiB, with a two-byte big-endian length prefix. |
| Command/retry retention | Ephemeral only; one monotonic handoff deadline, at most 60 seconds. |

Outgoing requests carry a sender-local monotonic deadline that is neither signed
nor serialized. The codec rejects expired queued writes before emitting bytes
and bounds header, body, and close writes by the same deadline.

Reattaching a deadline may shorten it, never extend it. The current daemon uses
a 15-second connected-only handoff; this wire contract adds no automatic redial
or persistent command outbox.

### Receiver Contract

1. Decode a bounded frame and validate the core signing/shape contract.
2. Bind the signed issuer to the authenticated libp2p sender and pinned anchor.
3. Apply to a clone of the currently selected state using core `apply_mutation_at`.
4. Persist and install successfully before replying `Applied(current_boundary)`.
5. Otherwise return a bounded rejection; never treat signature validity as admission.

Wire validation does not establish current membership or global freshness.
Existing control and snapshot-transfer messages remain unchanged. Runtime
persistence and command scheduling are separate from this codec.

### Rejections And Replies

| Outcome | Meaning |
| --- | --- |
| `applied` | Receiver durably committed and installed the reported boundary. |
| `stale_base` | Exact base no longer matches; optionally report current boundary. |
| `resync_required` | Receiver is gated until authenticated synchronization finishes. |
| `unauthorized` / `invalid` | Transport, scope, admission, signature, or command rejected. |
| `busy` / `persistence_failed` | No successful durable application is acknowledged. |

Replies bind the complete signed command digest. The caller must also match the
libp2p request ID and authenticated target peer. A different boundary, even with
the same authority revision, does not acknowledge the expected result.

### Reply Ownership And Identify

| Surface | Runtime Contract |
| --- | --- |
| Ownership key | Inbound request ID, authenticated peer, and connection ID. |
| Eligible reply | Valid scoped request with `applied`, or `stale_base` carrying a current boundary. |
| Capacity | At most 32 owners; capacity exhaustion returns `busy` before mutation application. |
| Lifetime | Fixed five seconds from receipt; repeated tracking cannot extend it. |
| Send failure | Release ownership if the response channel refuses the reply. |
| Terminal event | Release only on matching `ResponseSent` or `InboundFailure`. |
| Expiry | Retire ownership automatically; no persistent per-device entry. |
| Identify | Preserve checkpoint-only catch-up/reply connections without promoting packet authority. |

An excluded peer may still own a pending control reply after its removal commits.
Premature Identify rejection can cancel that ACK; bounded ownership permits
delivery without re-admitting the peer or treating it as routing infrastructure.

Outgoing handoffs also retain control-only peers while bounded requests are in
flight. These exceptions do not authorize packets, routes, ordinary publication,
or a permanent departed-publisher profile.

### Departure And Lost Acknowledgments

- Capture the signed command and bounded recipients before local self-removal.
- After exclusion, deliver only that final exact-base command within its deadline.
- Recipients authorize against their old exact base; stale recipients resync normally.
- No general excluded-publisher snapshot privilege or permanent replay log is added.

Successful application advances the exact base, so duplicate application is
rejected. After a lost reply, an authenticated response reporting the exact
expected resulting boundary can confirm synchronization without command history.

Competing branches still follow cooperative fork selection. Losing decisions,
including revocations, may be discarded; this protocol adds no irreversible
revocation or Byzantine consensus guarantee.

## Verification

```sh
nix develop -c cargo test --locked --lib runtime::control
```

Coverage includes authenticated multi-page transfer over TCP/Noise, codec bounds,
spoofed requesters, corrupted offers, mixed pages, monotonic expiry, cancellation,
replay slots, scope validation, and legacy descriptor decoding.

Mutation coverage includes every core change variant, transport issuer binding,
signature and scope rejection, strict bounded codecs, exact-base replay, lost
acknowledgments, conflicting replies, and TCP/Noise self-departure delivery.

Deadline tests verify expired queues, stalled writes, unchanged signatures/digests,
rejected injected deadline fields, and the non-extending lifetime cap.

Runtime tests cover bounded reply-owner cleanup and real TCP/Noise delivery
across Identify after durable departure. Current results and remaining gaps are
in the [acceptance audit](membership-checkpoint-acceptance.md).

Three-daemon intermittency remains open, including a focused run with zero
captured recipients. A passing full-suite rerun does not establish reliable
connected delivery or deployed-network convergence.
