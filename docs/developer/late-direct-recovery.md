# Late Direct Recovery

## Status

RM-1 investigation is active. No runtime fix or final causal conclusion yet.
The original censored outcome and its excluded comparisons remain unchanged.

## Saved Evidence

| Observation | Result |
| --- | --- |
| Public recovery runs other than current repetition 3 | First direct success in 5-20 seconds |
| Current repetition 3 | First direct success at 360.12 seconds; confirmation misses the 375-second stage |
| Delayed stage connectivity | All 76 probes per endpoint succeed; relay remains usable before promotion |
| New LAN address dialing | Both endpoint logs record five transport handshake failures to the new peer LAN address |
| Eventual recovery | Direct TCP establishes, then owned UDP session negotiation succeeds |

Artifact directory: `/tmp/p2p-vpn-resource-cli-smoke.d52044ef81e4a036`.

| File | SHA-256 |
| --- | --- |
| `observations.jsonl` | `25a36d69999fd046038c95987b16a8ebafac41b1adf9cb853d6a0a4424535829` |
| `node-a.log` | `d5e6e9ddb73a7939d65b3f82a1e770693dcc1ed62d8cee0a9f68fc04e64eae33` |
| `node-b.log` | `b158bcf4e2b261f1f3ab74baa214825d48853dd7e1ad66739b8766ec7a3523d6` |

Both logs report `Handshake failed: input error` for the new LAN TCP endpoint.
They lack event timestamps; sampled state provides time bounds, not exact dial
instants. This is not evidence that discovery waited 360 seconds to learn an address.

## Diagnostic Protocol

Frozen before execution. Hypothesis: simultaneous ordinary TCP dials reuse
listener ports, producing a TCP simultaneous-open connection with incompatible
Noise initiator roles; synchronized retries may repeat the failure.

| Property | Declared Value |
| --- | --- |
| Isolation | Existing user/network/PID namespace launcher; loopback only, no Internet route |
| Transport | Production `build_node` TCP security/multiplexer; discovery and automatic producers disabled |
| Overlap | Loopback netem delay 25 ms; both outgoing dials queued before polling |
| Conditions | Reused listener ports versus fresh outbound ports |
| Repetitions | Three fresh node pairs per condition; no retries within a pair |
| Deadline | Three seconds per pair; 30-second outer watchdog |
| Evidence | Per-side authenticated success or transport-error outcome; bounded launcher output, no private keys |
| Storage | Existing target; at most 1 MiB new diagnostic output; total task storage below 10 GiB |

Support requires reproducing bilateral handshake failure under reuse and
authenticated success with fresh ports. If results differ, retain them and revise
the hypothesis; do not run until a desired result appears.

This transport diagnostic cannot by itself explain every historical retry timestamp,
prove the precise kernel path taken by the saved run, or establish full VPN recovery.
Any runtime change requires separate regression and recovery verification.

### Initial Diagnostic Result

| Condition | Three Pair Outcomes |
| --- | --- |
| Reuse | A authenticates; B reports outgoing `Handshake failed: input error` |
| Fresh ports | Both outgoing sides authenticate |

The bounded diagnostic completed in 0.90 seconds. Its test exit means collection
completed, not that the hypothesis passed. It records each side's first terminal
event; an outgoing error does not exclude a subsequent accepted inbound connection.

**Bilateral outage was not reproduced.** The TCP transport binds reused ports to
an unspecified local IP. In this single-namespace loopback topology, route-selected
source addresses need not match the node's listener address; this differs from the
saved two-namespace LAN and can create an unintended self-connection.

The initial root invocation failed before execution because its mapped user could
not traverse the user's private build directory. Running as the owning user
executed the same binary successfully; no artifact permissions were relaxed.

### Remaining Evidence

- Reproduce both outgoing connections failing with distinct network stacks and correct source IPs, rather than infer that from the loopback result.
- Observe inbound acceptance as well as outbound failures until the fixed diagnostic deadline.
- Relate the five new-address failures to recovery quarantine, retry scheduling and eventual successful negotiation.

The saved stage's A-side failure counter first increments within five seconds of
LAN restoration. Additional increments occur around 20, 50, 100 and 190 seconds;
other-address failures also contribute to this counter. It cannot independently
timestamp individual LAN attempts.

Source backoff is 10, 20, 40, 80, 160 seconds, capped at 300 seconds, with a
10-second redial tick. This is consistent with synchronized failure amplification,
but timing agreement alone does not prove TCP simultaneous open in the saved run.

### Checkpoint Validation

- Namespace harness unit tests: 41 passed; 22 opt-in tests excluded.
- Cached Nix test-source and vendored-source parity check passed.
- Runtime, dependency, configuration and frozen measurement artifacts remain unchanged.

```sh
"$TEST_BINARY" --ignored --exact \
  tun_namespace_tcp_simultaneous_dial_diagnostic --nocapture
```

Run as the user owning the cached test binary. The existing launcher supplies
isolated namespaces and a 30-second watchdog. No physical network changes occur.
