# Monarchic runners relay dial failure, 2026-10-04

Online code pairing found the inviter through public discovery, but repeatedly
failed to establish a relay connection. The joiner never reached an approval
candidate or selected a transport. The full journal exposed local routing-pool
rejections; other handshake failures and closures remain unexplained.

## Environment and operation

- Installed source revision: `e7dc13c9c9ddb3da9adfd29be3bd2fe143e79c72`.
- Joiner: NixOS, instance and network `monarchic-runners`.
- Service: `p2p-vpn-monarchic-runners.service`.
- Join operation: `pZOt0XXk8u3cUbCdYEqhLQ`.
- Joiner peer: `12D3KooWRT5MDukbE9JJUMoqRsvgYjibii3k2hJhGjSmw9FgzfV3`.
- Inviter peer: `12D3KooWCFXhNVdJVf1cZf7TA6NYgkrD51YL7gDNiVLhf1ANd9wX`.

The system daemon and CLI both used the updated NixOS package. Daemon health
reported ready after activation. The host clock was NTP-synchronized and its
display timezone was `America/New_York`. Timestamps below are UTC.

The joining command used `pair join` with `--instance monarchic-runners
--no-wait`. The one-time pairing code is intentionally omitted.

## Exact journal excerpt

The [complete service log for 19:00-19:30 UTC](monarchic-runners-service-2026-10-04-1900-1930-utc.log)
contains every journal entry in that window, without message filtering or
redaction. It was exported with UTC timestamps and `short-iso` formatting.

This entry is copied from the joiner's journal, rendered with
`journalctl --utc -o short-iso`:

```text
2026-10-04T19:22:53+00:00 nixos-pc p2p-vpn[238871]: outgoing connection to 12D3KooWCFXhNVdJVf1cZf7TA6NYgkrD51YL7gDNiVLhf1ANd9wX failed: Failed to negotiate transport protocol(s): [(/ip4/144.76.30.57/tcp/4001/p2p/12D3KooWRwrs3M34JNnRwGqhhocQs5xSYQk8kmci59fmYjhg9q4v/p2p-circuit/p2p/12D3KooWCFXhNVdJVf1cZf7TA6NYgkrD51YL7gDNiVLhf1ANd9wX: : Response from behaviour was canceled: Response from behaviour was canceled: oneshot canceled)(/ip4/135.181.230.175/tcp/4001/p2p/12D3KooWKywS55h124mfJqit6T7ejKsi8ZnpJtH1YgX5iENUkktc/p2p-circuit/p2p/12D3KooWCFXhNVdJVf1cZf7TA6NYgkrD51YL7gDNiVLhf1ANd9wX: : Response from behaviour was canceled: Response from behaviour was canceled: oneshot canceled)]
```

The pairing warning repeated during the attempts:

```text
event=pairing_code_outbound_failure
runtime_network=monarchic-runners
transport=unknown
error=dial_failure
```

Other attempts used `83.173.236.99:4001`, with relay peer
`12D3KooWBbkCD5MpJhMc1mfPAVGEyVkQnyxPKGS7AHwDqQM2JUsk`, and produced the
same cancellation error. No `at capacity` messages matched the filtered
19:08-19:23 UTC log window.

The cancellation also appeared for unrelated public peers later. That observation
does not prove those failures share a cause with the pairing failure.

## Collect the pairing window

Use an explicit UTC window: a relative one-hour window eventually excludes this
attempt. This command requires no `jq`:

```sh
sudo journalctl -u p2p-vpn-monarchic-runners.service \
  --since '2026-10-04 19:00:00 UTC' \
  --until '2026-10-04 19:30:00 UTC' \
  --no-pager --utc -o short-iso |
  grep -E 'event=pairing|event=code_pairing|12D3KooWCFXhNVdJVf1cZf7TA6NYgkrD51YL7gDNiVLhf1ANd9wX'
```

## Investigation requested

### Full Journal Findings

Source: [unfiltered joiner journal](monarchic-runners-service-2026-10-04-1900-1930-utc.log).

| Relay | Observed Failure |
| --- | --- |
| `144.76.30.57` | TCP transport handshake failed with EOF or reset. |
| `83.173.236.99` | TCP transport handshake failed with EOF or reset. |
| `135.181.230.175` | Connections established, then closed before circuit completion. |

At 19:10:32 and 19:20:36 UTC, the joiner explicitly closed the third relay with
`public_routing_connection_rejected reason=capacity close_requested=true`.
At 19:20:36, the target circuit cancellation immediately follows that closure.

This proves a local routing-capacity rejection path, not the cause of every
closure. The earlier transport failures and other short-lived connections remain
separate observations.

### Pairing Relay Ownership

- Snapshot relay dependencies from the addresses available to each pairing dial.
- Bound dependencies to 64 distinct relays per tracked peer attempt.
- Protect active attempt and selected-inviter relays from routing-pool rejection.
- Retain their connections while the pairing operation owns them.
- Release ownership on attempt failure, cancellation, expiry or completion.
- Relearn dependencies after restart; do not persist transient route hints.
- Preserve membership and route authorization independently of transport ownership.

`connection_closed` now includes libp2p's `cause` for subsequent investigation.
`none` means the event supplied no error; it is not proof of a remote refusal.

### Acceptance Boundary

Controlled regression tests must cover a full routing pool and ownership cleanup.
A successful external WAN pairing through approval and completion is still required
before calling the reported WAN failure resolved.

### Local Verification

| Check | Result |
| --- | --- |
| Rust suite | 1,737 passed; 47 opt-in tests ignored by the default run |
| Formatting | `cargo fmt -- --check` and included relay-test file passed |
| Clippy | Correctness, suspicious and performance deny gates passed; existing warnings remain |
| Full routing pool | Establishment and Identify retain only session-owned relays |
| Live local relay | Code Hello/Challenge crosses a circuit with a full routing pool |
| Lifecycle | Failure, cancel, expiry, completion, selection and restart covered |
| Peerless code pairing | Linux namespace approval and traffic test passed |
| Relayed file pairing | Linux namespace live pairing and traffic test passed |
| Nix package | Revision `67853219` built; release checks passed with 1,737 tests and 47 opt-in tests ignored |
| Local NixOS deployment | Personal flake pin committed; system built and switched; both VPN daemons use the new package |
| Existing network | Four validated runners peers; inventory preserved; ThinkPad overlay DNS and five-packet ping passed |
| VM builds | Not rerun; tested namespace integration and local NixOS deployment instead |
| External WAN acceptance | Outstanding; local tests do not establish public-relay reliability |

The local VPN services restarted at 23:34:56 UTC on October 4. The NixOS switch
succeeded and the Nix daemon kept its original process. No peer, key or relay
configuration changes were needed.

### Fresh WAN Attempt

1. Update the joiner's package to revision `67853219` or a descendant and restart its daemon.
2. Update the inviter as well so both journals include connection-closure causes.
3. Open a new invitation and start the external join before its expiry.
4. Verify the candidate, approve it, and confirm completion on both sides.
5. Capture both full journals for that UTC window if dialing still fails.

Use the existing minimal configuration. Do not pin another public relay as a
workaround: automatic discovery and circuit selection remain runtime responsibilities.

### Original Follow-Up

Trace where the relay client's response channel is dropped or cancelled during
these circuit dials. Correlate joiner logs with the inviter's operation status
and relay reservation state in the same UTC window. Distinguish local request
cancellation from a relay refusal, timeout, stale reservation, or connection
cleanup; the current error text does not identify which happened.

Once the cause is established, add a regression test for that cancellation path
and validate a fresh code pairing between the two hosts through approval and
completion. Report the required revision for each host and any diagnostic
commands needed before another pairing attempt.
