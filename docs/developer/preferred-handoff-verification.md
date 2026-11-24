# Preferred Handoff Verification

Verified November 24, 2026, on `browser-wasm`. This milestone covers
peer-initiated preferred-interface handoff, not the broader browser adapter
refactor or battery efficiency.

## Result

The production browser and native VPN switched before the healthy old path
failed. A fresh HTTP nonce completed on the preferred native interface within
the 30-second acceptance deadline.

| Run | Post-commit HTTP completion |
| --- | --- |
| Initial implementation 1 | 1,741 ms |
| Initial implementation 2 | 1,797 ms |
| After review fixes | 1,834 ms |
| Final, with explicit resource-retirement checks | 2,345 ms |

The final two runs use the review-corrected behavior. These are local isolated
network measurements, not promises of physical Android or public-relay latency.
The earlier controlled trace completed after about 80 seconds because it waited
for circuit expiry; that is not the new handoff trigger.

## Final Timeline

Times start after Linux route lookup confirms the new default is usable.

| Event | Elapsed |
| --- | --- |
| Native owner sends authenticated offer | 1,100 ms |
| Authorized candidate IP packet written to TUN | 1,270 ms |
| Payload-gated native commit | 1,280 ms |
| Old member circuit closes | 1,292 ms |
| Unneeded old outer relay socket closes | 1,293 ms |
| Fresh post-commit HTTP response completes | 2,345 ms |

No browser reset, manual endpoint replacement, forced circuit expiration, or
phone management was used. Both physical-underlay equivalents stayed reachable
for at least 31 seconds. The controlled relay circuit lifetime was 180 seconds.

## Evidence Checks

| Assertion | Proof |
| --- | --- |
| OS preference | Kernel route selects `underlay2`, source `11.255.52.2`. |
| Actual receiving interface | Match inner circuit to outer TCP owner, source, interface index, and route generation. |
| Real payload | Normal packet authorization, replay checks, and successful TUN write precede native payload verification. |
| End-to-end service | Fresh HTTP nonce is returned by a service bound only to the native overlay IP. |
| No premature retirement | Original member circuit and TCP socket remain until payload proof; explicit closure follows commit. |
| Stable overlap | Source-bound traffic reaches the relay on both interfaces for more than 30 seconds. |
| No timeout-driven migration | No relay reservation or circuit timeout occurs before acceptance. |
| Legacy behavior | A pre-extension browser receives one unsupported offer, keeps the original circuit/socket, and serves HTTP beyond 32 seconds. |
| Idle scheduling | No active round means no handoff deadline; unchanged route audits do not make the runner continuously ready. |

## Regression Coverage

| Check | Result |
| --- | --- |
| Native library | 2,241 passed, 41 existing ignored namespace/long-running cases. |
| Shared handoff codec | Eight tests included in the native library run. |
| Event-driven scheduling | Four tests included in the native library run. |
| Relay server/client unit tests | 23 passed via an isolated manifest pointing to the exact vendored source. |
| Native relay ownership/drain integration | 19 passed. |
| Production companion pairing | Pinned and isolated unpinned cases passed. |
| Browser library | 28 passed after rebasing over the concurrent test-only commit; includes capability, payload, expiry, cancellation, stale receipt, and independent-member checks. |
| Post-rebase native characterization | The added surviving-binding test passed. No production code changed in that rebase. |
| No-relay WASM configuration | `--no-default-features` compilation passed. |
| Static checks | Rust formatting, shell/JavaScript syntax, and native Clippy completed; Clippy reports repository warnings. |

The standalone vendored integration harness additionally has a server-selection
test. It was not run here because its separate dev-dependency set was not cached.
Server selection is exercised by the Chromium test and the relay unit tests.

The repository has no Lean/TLA+ model for this handoff. Verification here is
executable unit, socket, and end-to-end testing, not a formal proof.

## Reproduce

Run the relay suite with the repository's flake tooling. It builds the app CLI
and takes the browser bindings and pairing seed fixture from the pinned SDK.

```sh
nix develop
npm ci --prefix crates/p2p-vpn-browser
bash scripts/test-browser-linux.sh relay
```

- Install the browser package's locked Playwright dependencies and Chromium.
- Set `P2P_VPN_CHROMIUM` to select a nonstandard Chromium executable.
- Set `P2P_VPN_LEGACY_APP_ASSETS` to a pre-extension build to run the legacy
  compatibility case; otherwise that one case explicitly skips.

For this run, the full Nix development shell attempted uncached dependency
builds. Those builds were cancelled. Tests used the already installed Nix-store
Rust toolchain, cached Cargo dependencies, and existing Chromium instead.

## Checkpoint Pairing Follow-Up

The native offer and its replies now use the active membership scope, even
when checkpoint pairing generated the credential outside static config.
Chromium returns `unauthorized_peer` for early requests on the known
candidate without admitting their payload.

These runs used real Chromium and the same controlled, dual-interface Linux
topology. Times start when the kernel selects the new interface.

| Scenario | Result |
| --- | --- |
| Checkpoint pairing with no configured membership key | Authorized payload at 1.24 s; commit at 1.25 s; fresh HTTP at 2.23 s. |
| Same pairing with a deliberately early native probe | Explicit rejection, then authorized payload at 1.35 s; commit at 1.36 s; fresh HTTP at 2.41 s. |
| Legacy membership with current Chromium | Fresh HTTP on the new interface at 2.65 s. |
| Pre-handoff browser build | Original HTTP circuit still usable at 32.77 s. No payload proof or commit on the candidate. |

Both underlays remained usable for at least 31 seconds. In successful
migrations the old resources retired only after commit.
With the same injected probe and a browser built before the reply fix, the
native peer saw an unexpected end-of-stream instead of a typed rejection.

The relay suite includes the checkpoint case. To exercise the rejection
independently of the current sender guard:

1. Create a writable checkout at the SDK revision in `flake.lock`. Apply
   [the early-probe fixture](../../crates/p2p-vpn-browser/tests/fixtures/early-handoff-probe.patch)
   using `patch -p1`. In a disposable app checkout, point its Cargo SDK
   dependency at the patched checkout and rebuild the native CLI.
2. Run the handoff test in an isolated namespace against the production
   browser assets, with that binary
   in `P2P_VPN_BIN` and set `P2P_VPN_TEST_EARLY_HANDOFF_PROBE=1`.
3. Check that the native peer received the typed rejection on the same
   candidate that later delivered the authorized payload and committed.

Do not deploy the fault-injection binary. The Android ARM64 crate passed a
cross-target Cargo check. This follow-up did not test Android devices or
public relays.

## Platform Limits

| Platform/path | Status |
| --- | --- |
| Chromium, Linux companion, dual-interface Linux native peer | Implemented and verified as above. |
| Linux legacy browser interoperability | Verified with the actual pre-extension WASM build. |
| Android native initiator | Uses the shared native runtime; no Android cross-build or device verification in this milestone. |
| Physical cellular/Wi-Fi and arbitrary public relays | Not measured by these tests. Different relay reservation selection may reject a candidate; retain fallback. |
| Standalone browser first-hop interface enforcement | Still limited by browser APIs; unchanged. |
| Other browser engines or native OSes | Not qualified by this milestone. |

## Artifact Identity

| Artifact | SHA-256 |
| --- | --- |
| Native acceptance binary | `c9acc5e0463af029b85e35c56bc4a79d6b7792b937c87a485949ac3ee1a20156` |
| Production browser WASM | `83cb6696f809681dbff282e18fc35f156bbf3b51c14a9a41fac8975a13327fd2` |
