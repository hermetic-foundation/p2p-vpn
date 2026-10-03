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

## Verification

```sh
nix develop -c cargo test --locked --lib runtime::control
```

Coverage includes authenticated multi-page transfer over TCP/Noise, codec bounds,
spoofed requesters, corrupted offers, mixed pages, monotonic expiry, cancellation,
replay slots, scope validation, and legacy descriptor decoding.
