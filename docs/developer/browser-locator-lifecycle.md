# Browser locator lifecycle follow-on

The verified distribution batch is unchanged. Its controlled topology proves
one publication, readback, native code enrollment from an independent seed,
and closure of an in-flight stream on a default-link change. It does not show
public IPFS convergence or any remote deletion guarantee.

The [current audit](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/browser-conformance.md#clause-audit) records
the implemented scoped publication paths for fresh/rooted legacy and live
checkpoint issuers, including checkpoint reissue under separate child
leases. The design notes below preserve the implementation plan, not a
list of outstanding production changes.

## Cached Hints and Authority

A Kademlia provider response supplies a peer and a route to try. The remote
server owns its record TTL. Closing a browser invitation, stopping a publisher,
and revoking the companion lease are not remote withdrawal. A fresh lookup
may return the exact old record while enrollment must fail.

The follow-on native test publishes to the independent storage peer, confirms
the exact alias, retires the invitation, and looks up the same key through the
independent seed. A new profile-free native join must see the cached record
and produce neither a verified grant nor an imported config. The four cases
are browser cancellation, browser deadline (advanced independently of the
storage clock), release of the reservation's scoped ingress, and default-link
invalidation followed by restoring reachability without restoring the lease.
Releasing ingress tests loss of the reservation path, not a relay-side
reservation-termination notification.

Controlled follow-ups:

- Relay-side loss with a readable cached locator and no grant, and stale
  local SDK approval on a confirmed replacement, are covered separately.
  Neither sends the old uncommitted Poll on the replacement. The
  [audited browser dispatch guards](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/browser-conformance.md#exact-scope-disposition)
  and typed lifecycle tests cover the owner boundary separately. Further
  typed dispatch coverage is optional; do not replay on the wire or
  conflate this with valid committed receipt recovery.
- Give the storage peer a short test TTL. Prove the record is present before
  its deadline and absent from a fresh lookup after it, even without a new
  browser operation. Use a fresh lookup so client caches cannot hide expiry.
- Before adding refresh, specify its minimum interval, maximum count per
  invitation, and the per-lease dial and byte budget. Test refresh before
  record expiry, and no refresh after cancellation, approval, invitation
  expiry, reservation loss, or route-generation change.
- Distinguish withdrawal of local admission from deleting a provider from
  remote stores. Standard Kademlia does not provide a portable immediate
  removal RPC. Verify that stale records cannot grant authority while those
  servers age them out.

## Historical Publication Plan

**Rooted legacy.** The rooted legacy issuer already starts through the same
companion invitation lease and checks the same confirmed native alias. Route
its scoped UI path through `publish_locator_distributed` with that lease,
keeping the shared once-per-invitation guard and the existing transaction and
approval code. Update the rooted UI approval and denial fixtures to bootstrap
through the independent storage topology; keep their pending, failed save,
prior-signer preservation, and denial assertions. Leave explicit-address
pairing unchanged. On distributed failure, show a publication error rather
than silently falling back to the reservation relay.

**Live checkpoint.** A confirmed live reservation alone is not an OS-default
scope proof. Today the app can reserve relay transport without an incoming
companion lease; the outbound member lease is not a substitute. Establish
companion-scoped incoming WebRTC before the live reservation starts. Bind the
publication session to the exact incoming entrance, reservation listener,
and route generation. Do not retrofit a second reservation onto an already-live
issuer or change its Noise identity. An unscoped issuer should offer only the
explicit route, with no claim of locator distribution.

The existing publisher registry is one-shot per lease. A live checkpoint
session must be able to reissue on the same reservation: give each invitation
generation a one-shot child publication session under the incoming lease.
Allow one active child, cap starts per lease and over time, and bind it to
the browser's current invitation. Cancel or expire the child without releasing
the active member reservation; a route change or reservation loss must revoke
both. Keep code, credentials, signing keys, and policy in WASM. The companion
receives only the public derived v2 lookup key transiently, the local lease
and child identifiers, and the exact entrance. It continues to select
and authenticate eligible DHT peers and to forward only indexed destinations.

Checkpoint coverage must include two distinct locators on the same live
reservation, refusal of a repeat for the same invitation, explicit readback
from off-relay storage, and denial of a stale first locator while the second
invitation and existing member circuits still work. Test that a scope change
retires publication and that approval still requires durable roster commit
and matching authenticated sync. Coordinate the incoming-lease and live
checkpoint changes with the checkpoint owner and integrator before editing
shared lifecycle code.
