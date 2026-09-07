# p2p-vpn Patch Record

## Provenance

| Field | Value |
| --- | --- |
| Crate | `libp2p-kad` |
| Upstream version | `0.48.0` |
| Registry archive | `libp2p-kad-0.48.0.crate` |
| SHA-256 | `13d3fd632a5872ec804d37e7413ceea20588f69d027a0fa3c46f82574f4dee60` |
| License | MIT; original notices retained in source files |
| Upstream revision | Retained in `.cargo_vcs_info.json` |

The archive checksum was verified against the original root Cargo lock before
import. All archive files are retained, including the upstream manifest, lock,
changelog, generated protocol source, and tests.

## Changes

| File | Patch |
| --- | --- |
| `src/behaviour.rs` | Expose automatic-bootstrap control; apply routing-address limits to explicit, confirmed, and pending entries; support protected seeds. |
| `src/addresses.rs` | Bounded insertion/replacement, category-aware churn rotation, size rejection, and explicit seed protection. |
| `src/lib.rs` | Export the optional `AddressLimits` configuration type. |

The library default is unchanged. p2p-vpn disables automatic and periodic bootstrap
explicitly so its scheduler owns bootstrap initiation. Explicit `bootstrap()` is
unchanged; DHT wire messages and protocol names are unchanged.

p2p-vpn opts into 64 retained routing addresses per peer and 2,048 encoded bytes
per address. Configured seeds count toward the same budget, survive churn, and
remain explicitly removable. Query caches are separate and not bounded by this patch.

## Build Integration

- The root `[patch.crates-io]` selects this source for the entire Cargo dependency graph.
- Desktop and Android Nix source filesets include this directory.
- The vendored package is excluded from root workspace membership, not from compilation.
- Application regressions exercise both shared-public and separate-public-pairing DHTs.

## Maintenance

1. Verify a replacement archive against its registry checksum before import.
2. Compare all local changes with the pristine archive; do not update generated protocol files casually.
3. Reapply only required fixes and update this record.
4. Run discovery, pairing, recovery, source-parity, and native-target checks.

Aggregate routing storage, query-cache limits, and sustained measurements remain
tracked in `docs/developer/kademlia-resource-plan.md` at the repo root.
