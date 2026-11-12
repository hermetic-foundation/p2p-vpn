# Local Patch

- Base: crates.io `libp2p-request-response` 0.29.0, unchanged wire protocol and public types.
- Archive SHA-256: `a9f1cca83488b90102abac7b67d5c36fc65bc02ed47620228af7ed002e6a1478`.
- Addition: read-only `Behaviour::has_pending_requests(peer)` over existing ownership and event queues.
- Share a per-connection native worker-count gauge to include codec I/O before request decoding.
- Purpose: prevent deliberate VPN dormancy from closing unfinished control exchanges.
- Regression coverage: repository `tests/control_work.rs` and runtime retirement tests.
- No replacement protocol engine, request ledger, altered timeouts, or upstream version upgrade.
