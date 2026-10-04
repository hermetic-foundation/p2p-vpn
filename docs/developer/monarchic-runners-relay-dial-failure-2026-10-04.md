# Monarchic runners relay dial failure, 2026-10-04

Online code pairing found the inviter through public discovery, but repeatedly
failed to establish a relay connection. The joiner never reached an approval
candidate or selected a transport. This report records observed failures; the
cause of the cancellation is not established.

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

Trace where the relay client's response channel is dropped or cancelled during
these circuit dials. Correlate joiner logs with the inviter's operation status
and relay reservation state in the same UTC window. Distinguish local request
cancellation from a relay refusal, timeout, stale reservation, or connection
cleanup; the current error text does not identify which happened.

Once the cause is established, add a regression test for that cancellation path
and validate a fresh code pairing between the two hosts through approval and
completion. Report the required revision for each host and any diagnostic
commands needed before another pairing attempt.
