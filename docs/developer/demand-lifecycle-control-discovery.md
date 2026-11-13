# Bounded Control Discovery

## Status

The contract was published before implementation. Finite control-owned lookup
now extends the shared native recovery owner. Production quiet support remains
disabled; activation, reachability, and energy gates are still open.

## Protocol Audit

The [IPFS DHT specification](https://specs.ipfs.tech/routing/kad-dht/)
defines provider records separately from value records. Public value storage
accepts public-key and IPNS namespaces, not application-specific values.

Client-mode peers are excluded from routing buckets. Peer-ID lookup can still
find them through server peerstores, but must not be the only recovery path.
The existing network provider key supplies the complementary discovery path.

| DHT | Publication And Lookup |
| --- | --- |
| Public IPFS | Publish the existing normalized network provider key; first query its providers, then use bounded peer-ID lookup if needed |
| Private/custom | Preserve signed peer-address value publication and authenticated value lookup |
| Provider hint | Capture native query addresses for only the expected peer before retiring the query; never trust the provider list as membership |
| Packet authority | Secure identity and existing forwarding authorization remain mandatory; discovery never installs a packet grant |

## Ownership

| Resource | Bound |
| --- | --- |
| Native query IDs | Extend the existing `RecoveryQueries` owner; no second query registry |
| Targeted concurrency | One shared slot for data and control recovery |
| Control eligibility | Existing authorized recipient and a live coalesced delivery round, not a roster or dirty marker alone |
| Scope | Exact recipient, capability generation, and original delivery deadline |
| LAN priority | Preserve the existing 15-second LAN-first window before public lookup |
| Lookup stages | At most two per public round; one for private signed values |
| Stage deadline | At most ten seconds and never beyond the original 45-second control round |
| Failure history | Preserve existing per-peer cooldown across new rounds; only the next stage of the same live round may continue within its original deadline |
| RPC writes | Existing four-flight control/page budget, three attempts, and original absolute native deadline |

- Do not renew a round or application demand from a lookup, address hint, tick, or failed RPC.
- An established connection may deliver immediately; discovery must not hold up a usable path.
- Stop or cancel current query ownership on completion, expiry, supersession, revocation, or network change.
- Quiet-mode cancellation removes ordinary data recovery while preserving only currently owned, valid control recovery.
- Recheck scope before applying each result; do not route controlled results through generic full-mesh/infrastructure discovery.
- Retain only supported, correctly targeted addresses through the existing bounded address cache.
- Use the existing native cancellation API to purge queued query results/actions; already dispatched handler work drains through native work fences.

## Publication

- Reuse the existing address-change/refresh owner and its capacity/backoff limits.
- Public address changes refresh provider addresses through `ADD_PROVIDER`, not unsupported custom `PUT_VALUE` requests.
- Keep the existing private signed-record freshness contract.
- Public ordinary maintenance has one provider task, not a second address-value publisher.
- Preserve explicit discovery/advertisement disable settings and minimal JSON/Nix configuration.
- Do not add another periodic provider publisher or user-managed relay requirement.

## Validation

| Boundary | Required Evidence |
| --- | --- |
| Shared budget | Data/control lookups cannot exceed the existing targeted-query slot |
| Quiet behavior | Exact current control work survives suppression; unrelated data discovery is cancelled |
| Bounds | Original deadline, two-stage limit, cooldown, supersession, revocation, and network reset |
| Security | Wrong key, peer, network/tag, expired signature, private public-hint address, and unsupported transport cannot expand discovery |
| Public client | Real local IPFS-protocol lookup finds a client-mode provider through a router without its target address configured locally |
| Scope isolation | Other providers/closest peers do not become mesh dials or control recipients |
| Private DHT | Signed address lookup and legacy publication remain compatible |
| Native cancellation | Queued results/actions are purged; native I/O fences still protect already dispatched work |
| Packaging | Linux and Android source parity; bounded cached builds |
| Energy | Fresh matched paired/reversed cellular measurements remain required after functional activation |

The namespace fixture uses simulated public IPs and no external route. It is
evidence of native protocol behavior and scope isolation, not public-WAN or
cellular battery acceptance.

## Native Fixture

| Role | Evidence |
| --- | --- |
| Router | Real IPFS-protocol DHT server in a disposable network namespace |
| Publisher | Client-mode peer publishes its TCP listener; router stores it outside its routing buckets |
| Recipient | Starts with only the router address; control target address is absent from configuration and native lookup cache |
| Delivery | Scoped provider lookup learns the target; native encrypted control request receives an accepted reply |
| Isolation | Unrelated provider is returned but never dialed; target is not promoted to a DHT routing server |
| Quiet | Application-demand version is unchanged and packet path remains dormant |
| Timing | LAN grace is injected; TCP publication, discovery, secure connection, and acknowledgement use real sockets |

- Wait for the router's stored record, not just local publication completion; `ADD_PROVIDER` has no remote acknowledgement.
- Compare normalized `/p2p/<peer-id>` addresses, not bare listener addresses.
- Replace inherited bootstrap address books; allow dials only to the isolated router and intended target.
- On timeout, report the active fixture stage rather than a generic elapsed error.

These checks do not establish first IP payload recovery, long dormancy,
public-WAN availability, mobile roaming, or battery savings.

## Verification Record

| Check | Result |
| --- | --- |
| Scoped recovery driver | Seven focused tests pass: LAN priority, scope checks, cancellation, deadlines, fallback, signed values, and target-only retention |
| Shared native query owner | Thirteen tests pass, including data/control capacity, quiet cancellation, cooldown, expiry, and network reset |
| Public publication | Three tests pass: provider records, explicit disable/private compatibility, and one ordinary public provider task |
| Native public client | Three repeated namespace runs pass on final source; real TCP control delivery and scope isolation |
| Lifecycle dispatcher | Twenty-seven tests pass, including collision-safe fixture admission and unchanged request budgets |
| Workspace/all targets | 2,086 passed; zero failed; 50 existing/opt-in exclusions |
| Clippy | Correctness, suspicious, and performance groups pass; other advisory warnings remain |
| Formatting | Workspace Rust passes |
| Package sources | Linux and Android ARM64/x86-64 evaluate; runtime, patched native dependencies, and lockfile match byte-for-byte |
| Linux test inventory | All 41 Rust integration/support files match the evaluated source |
| Resources | Cached Nix tools, one existing approximately 7 GiB target, two jobs, no incremental compilation or downloads |
| Not exercised | Full Nix/APK builds, device/service deployment, long dormancy, public-WAN activation, and cellular energy |

The first broad run exposed an unrelated random derived-address collision in
the rate-budget fixture. Its corrected setup filters occupied derived addresses
without overriding IPs or weakening production route validation.

The superseded alias-address attempt was stopped, not counted as a passing run.
Full Nix builds remain outside cached-only verification: the earlier offline dry
run required 493 uncached derivations, including bootstrap compilers.

These executable checks are not a formal proof or a battery comparison.
