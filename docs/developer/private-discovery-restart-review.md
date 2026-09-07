# Private Discovery Restart Review

## Finding

Identify handling treated pairing-protocol support as a reason to postpone
Kademlia routing admission. A private bootstrap therefore retained otherwise
eligible routing clients as temporary membership probes.

After 30 seconds, those probes expired and the bootstrap quarantined the
clients. A restarted client could then lose bootstrap access despite supporting
the same private Kademlia protocol.

## Correction

- Advertised pairing support no longer suppresses routing admission.
- An active code-pairing session retains its existing probe treatment.
- Routing admission remains capacity-bounded and does not grant VPN membership.
- Packet authorization, pairing validation, and wire formats are unchanged.

## Reproduction

The opt-in fixture test runs three real runtimes in a user/network namespace.
The namespace contains only loopback and `192.168.250.1/32`; it has no WAN route.

1. Start a private bootstrap and two explicitly authorized peers.
2. Disable multicast and omit peer addresses.
3. Require a supported peer path, then wait 40 seconds.
4. Restart one peer with the same identity and a different TCP endpoint.
5. Require automatic path recovery within the unchanged 40-second deadline.
6. Stop all runtimes and release the packet-reader channels.

The runtime still attempts public discovery even with automatic relay candidates
disabled. The namespace blocks those attempts; this check does not establish
that private discovery is free of unnecessary public-discovery work.

## Commands

Run inside `nix develop`, with build concurrency limited:

```bash
export CARGO_BUILD_JOBS=2
test_binary=$(cargo test -p p2p-vpn-android-e2e-fixture \
  --no-run --message-format=json | jq -r \
  'select(.reason == "compiler-artifact" and .profile.test) | .executable // empty')
bash scripts/private-discovery-restart.sh "$test_binary"
```

Requires Linux unprivileged user/network namespaces, `ip`, and `unshare`.
No host interfaces, routes, services, or physical devices are modified.

## Evidence

| Case | Result |
| --- | --- |
| Immediate same-endpoint restart | Passed in 10.56 seconds |
| Immediate changed-endpoint restart | Passed in 20.24 seconds |
| Delayed changed-endpoint restart before fix | Failed in 90.15 seconds; bootstrap quarantined both clients |
| Identical delayed test after fix | Passed in 60.22 seconds; clients classified as routing peers |
| Final wrapper repeat | Passed in 60.23 seconds with fresh identities and ports |

- Before: `/tmp/p2p-vpn-review-private-restart-aged.log`.
- After: `/tmp/p2p-vpn-review-private-restart-fixed.log`.
- Repeat: `/tmp/p2p-vpn-review-private-restart-fixed-repeat.log`.
- Earlier loopback/documentation-address attempts were invalid publication setups.
- This checks path restoration, not packet delivery, loss, carrier NAT, or Android lifecycle.
- The Android process/reboot/isolation failures remain open pending device verification.

## Verification

- Workspace: 1,231 passed; 20 opt-in tests ignored, with this regression run separately.
- Required Clippy categories, Rust formatting, and Nix `rust-test-sources`: passed.
- Wrapper shell syntax and ShellCheck: passed.
- No formal model covers this runtime admission decision.
