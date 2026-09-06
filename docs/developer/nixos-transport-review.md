# NixOS Transport Review

## Scope

On 2026-09-06, four exported transport VM checks passed at `859b29d5`.
The movement test was then strengthened without changing the runtime,
NixOS module, topology, discovery settings, or recovery timeouts.

| Check | Initial Script Time | Assertions |
| --- | --- | --- |
| `nixos-vm-quic-stream` | 20.24 s | Direct QUIC stream carries IPv4 traffic; connection IDs present; no packet-plane sessions, TCP fallback, or relay fallback packets |
| `nixos-vm-quic-datagram` | 23.69 s | QUIC datagram selected over available QUIC stream; session established; datagram payload counters increase without stream fallback |
| `nixos-vm-forced-relay` | 20.51 s | Separate VLANs, circuit establishment, IPv4 traffic, overlay IPs excluded from packet endpoint advertisements |
| `nixos-vm-network-move` | 126.86 s | Minimal peer configuration, direct LAN, relay recovery, direct LAN return, unchanged config hashes |

These are whole-script durations including waits and assertions, not precise
outage durations or throughput benchmarks. Each scenario ran alone with the
same optimized VM runtime recorded in the [workflow report](nixos-workflow-review.md).

## Movement Assertion Repair

The first strengthened run failed at the old `relay_paths 1` assertion, before
the new assertions ran. Traffic had recovered, but each edge had two healthy
relay paths and one inbound plus one outbound circuit.

| Evidence | Observation |
| --- | --- |
| Recovered ping | A's retry succeeded after 113.06 s; B's after 4.05 s |
| Circuit logs | Both nodes established inbound and outbound relayed connections |
| Healthy relay paths | Two on both edges near the failure |
| Payload counter | Seven outbound relay packets on each edge |
| Failure | Exact-one check timed out after 90.17 s |

The replacement assertion requires the intended peer's selected path to be
`circuit_relay` and its relay-path count to be positive. It no longer assumes
there can be only one path; it does not establish a resource upper bound.

Additional assertions require:

- Relay payload counters increase on both nodes during an additional bidirectional ping interval.
- Both systemd invocation IDs stay unchanged through relay recovery and LAN return.
- Existing config-hash, circuit-establishment, connectivity, and direct-return checks still pass.

The corrected run passed all four subtests in 73.09 s; its relay phase took
46.05 s. It had one healthy relay path. The differing durations do not establish
a runtime improvement: the production binary did not change.

A fresh `nix build --rebuild` reran the same check and passed in 71.83 s,
including a 46.08 s relay phase. Both corrected runs had one relay path;
the two-path case was observed in the failed run, not reproduced afterward.
All VM drivers cleaned up successfully; no QEMU process remained.

## Reproduction

```bash
nix build --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link --print-out-paths \
  .#checks.x86_64-linux.nixos-vm-quic-stream \
  .#checks.x86_64-linux.nixos-vm-quic-datagram \
  .#checks.x86_64-linux.nixos-vm-forced-relay \
  .#checks.x86_64-linux.nixos-vm-network-move
```

The initial build planned 172 small configuration/test derivations with cached
dependencies. Each movement-script update required four driver/test derivations.
Builds were offline with substitutions disabled; no new downloads were needed.

## Limits

- The movement relay is discovered in the controlled LAN fixture; this is not discovery of public IPFS relay operators.
- QUIC fixtures use explicit transport endpoints. Forced-relay fixtures disable DCUtR and AutoNAT.
- The movement fixture keeps default discovery behavior, but does not require successful hole punching.
- Traffic is IPv4 ICMP, not IPv6, DNS, SSH continuity, saturation, battery, or public NAT evidence.
- Counter increases and invocation IDs establish this scenario's behavior, not general bounded resource use or every recovery race.
- No runtime code changed; the full Rust suite and Android tests were not rerun for this test-only change.
- Nix formatting and whitespace checks pass. No applicable Lean model exists; these are executable tests, not formal proofs.

## Evidence

The [artifact record](nixos-transport-review-sample.json) retains successful and
failed log hashes, output paths, and test-source hashes. Check outputs are empty
markers; detailed evidence is in the compressed logs.

| Invocation | Log |
| --- | --- |
| Initial four checks | `/tmp/p2p-vpn-review-transports-current.log` |
| Exact-one failure | `/tmp/p2p-vpn-review-move-assertions-current.log` |
| Corrected movement | `/tmp/p2p-vpn-review-move-assertions-fixed.log` |
| Repeated movement | `/tmp/p2p-vpn-review-move-assertions-repeat.log` |
