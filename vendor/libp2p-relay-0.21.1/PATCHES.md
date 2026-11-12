# Local Patch

- Base: crates.io `libp2p-relay` 0.21.1; unchanged wire protocol and event types.
- Archive SHA-256: `d8b9b0392ed623243ad298326b9f806d51191829ac7585cc825c54c6c67b04d9`.
- Expose confirmed reservation connection IDs from the existing native maps.
- Allow selection of an authenticated direct connection for future reservations and circuits.
- Keep existing reservation/circuit/renewal ownership and default selection unchanged.
- No extra reservation ledger, connection allocation, transport engine, or timers.
- Regression coverage: repository `tests/relay_ownership.rs`.
