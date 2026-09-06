# Inventory Evaluation Measurement

## Scope

Measured on 2026-09-06 against the working change based on `255c188e`.
This compares joint and separate projections within one build, not two releases.

| Path | Work |
| --- | --- |
| Separate | Public audit projection, then public effective-membership projection. |
| Joint | One validated ledger evaluation, followed by both projections. |

## Method

- Installed Nix Rust 1.97.1, unoptimized test profile, one test thread.
- Fresh Ed25519 identities; one root and direct member grants, without expiry or route grants.
- Sizes include the root. Record generation and warm-up are outside measured intervals.
- Three rounds per size; alternate which path executes first.
- Assert exact effective-membership and audit equality after every measured call.
- No concurrent review builds; unrelated host activity was not isolated.

```bash
cargo test --offline --locked --lib measure_membership_inventory_evaluation \
  -- --ignored --nocapture --test-threads=1
```

The run used the compiled test executable from the shared target cache. The
command above selects the same test through Cargo. Total diagnostic time: 28.15 seconds.

## Results

Elapsed microseconds summed across three rounds:

| Signed Records | Joint | Separate |
| ---: | ---: | ---: |
| 8 | 379,431 | 764,775 |
| 32 | 1,515,288 | 3,034,367 |
| 128 | 6,131,190 | 12,224,799 |

Joint evaluation takes approximately half the time in this sample. This is
consistent with removing a repeated validation and ledger evaluation, not a change
to signature verification, trust policy, or projection results.

Raw log: `/tmp/p2p-vpn-review-inventory-views-measurement.log`.

## Limits

- One host and run; no variance estimate or release-profile measurements.
- No maximum-sized history, long delegation chains, conflicts, or concurrent requests.
- No daemon CPU, RSS, allocation count, DNS latency, or Android battery measurement.
- End-to-end peer-list latency also includes sorting, hostname processing, and serialization.

The correctness suite separately compares expiry/revocation behavior and rejects
tampered records and network mismatches. Timing alone does not prove correctness.
