# Lifecycle Wire Protocol

## Status

| Item | Contract |
| --- | --- |
| Parent design | [Demand-driven lifecycle](demand-driven-lifecycle.md) |
| Protocol | `/p2p-vpn/lifecycle/1` |
| Version | 1 |
| Deployment | Not enabled or advertised until runner integration is ready |

## Capability Admission

| Control Capability Field | Encoding | Default |
| --- | --- | --- |
| `supports_demand_lifecycle` | Boolean | False; omitted when false |
| `demand_lifecycle_epoch` | Optional 16-byte array | Absent; omitted when absent |

- The local constructor remains disabled; integration must explicitly opt in with a fresh nonzero epoch.
- Missing support or a missing/zero epoch means legacy behavior, never permission to close a connection.
- Existing `/p2p-vpn/control/1` enum variants remain unchanged.
- An old decoder can ignore the extension fields; a new decoder can read old capability messages.
- Epochs are runtime nonces, not identities or membership credentials.

## Frame And Bounds

| Field | Limit |
| --- | --- |
| Frame length | Two-byte big-endian unsigned length; excludes its own two bytes |
| JSON payload | 1 through 2,048 bytes |
| Network name | 1 through 128 UTF-8 bytes |
| Membership tag | Absent or 1 through 64 UTF-8 bytes |
| Epoch | Exactly 16 bytes; all-zero forbidden |
| Intent revision | 1 through `u64::MAX`; monotonic within an epoch |
| Responder revision | 0 through `u64::MAX`; zero is valid before its first local revision |
| Exchanges | One request/response per stream; close the write side after the frame |
| Timeout | Ten seconds; bounded by request-response behavior |
| Concurrency | Existing per-network configured control-stream budget |

Reject oversized length before allocating the payload buffer. Reject truncated,
empty, malformed, unsupported-version, unknown-field, and trailing-byte frames.
Validate outgoing objects before serialization or writing any frame bytes.

Scope limits apply to this optional extension only. A configuration that cannot
be represented must stay compatible/legacy; it must not become an invalid VPN
configuration or silently lose routes.

## Message Layout

| Object | Fields |
| --- | --- |
| Scope | `network_name`, optional `membership_tag` |
| Intent | `sender_epoch`, `receiver_epoch`, `revision` |
| Request | `version`, `scope`, `intent`, `command` |
| Response | `version`, echoed `scope`, echoed `intent`, `reply` |

| Command | Policy Input |
| --- | --- |
| `idle` | An `IdleOffer` bound to both runtime epochs and the sender revision |
| `wake` | A `Wake` bound to both runtime epochs and the sender revision |

| Reply | Meaning |
| --- | --- |
| `idle_accepted` with `responder_revision` | Acknowledges exactly this idle offer; not a new offer |
| `wake_accepted` | The exact wake was accepted; not payload delivery confirmation |
| `rejected` with `reason` | Bounded reason: busy, no incoming path, not idle, unsupported, stale, rate limited, or unauthorized |

No message carries an asserted peer ID. Use the authenticated transport peer,
look up its current authorization, and bind the instance/epochs separately.
Idle and wake revisions share one ordering space; an idle revision cannot be
replayed as a wake.

## Validation Order

1. Admit streams under existing connection/stream budgets; decode only a bounded frame.
2. Match the authenticated transport peer to the expected peer and recheck current authorization.
3. Validate message version, sizes, epochs, and nonzero intent revision.
4. Match the network name and current/allowed-previous membership tag.
5. Match incoming intent epochs: sender is the validated remote epoch, receiver is local.
6. For responses, match the original outbound request's complete scope, intent, and command/reply kind.
7. Apply the policy model only after all checks pass; revocation during a request prevents its later commit.

An idle responder must not retire a connection merely because `send_response`
accepted a channel. Wait for the actual response-sent event and recheck current
authorization/readiness and the model's still-current acknowledgement ticket.

## Security And Compatibility Tests

| Area | Required Cases |
| --- | --- |
| Serialization | Idle/wake/accepted/rejected round trips; all rejection codes |
| Framing | Empty, oversized prefix, truncation, trailing bytes, unknown fields, unsupported versions |
| Scope | Wrong network; wrong/missing tag; allowed previous tag; exact response scope echo |
| Identity | Wrong authenticated peer; revoked peer; no caller-supplied peer identity accepted |
| Epochs | Zero, wrong direction, restart/stale request, stale response |
| Correlation | Wrong revision, reply kind, or original request rejected without policy mutation |
| Legacy | Old capability missing fields; contradictory optional fields treated as unsupported |
| Transport | Real Noise/TCP and TLS/QUIC loopback exchanges; unsupported protocol fails without repeated retries |

These checks establish a wire/authentication boundary, not public-relay
reachability or the 10x battery result. Those remain separate completion gates.
