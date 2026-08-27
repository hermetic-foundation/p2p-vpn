# Pairing Implementation

This document defines the online code protocol and durable enrollment path.

User commands live in [../user/pairing.md](../user/pairing.md).

Post-admission dissemination is defined in [Membership Convergence](membership.md).

## Protocol Surfaces

| Surface | Version | Framing | Limit |
| --- | --- | --- | --- |
| Code exchange | `/p2p-vpn/pairing-code/1` | 4-byte length plus JSON | 64 KiB |
| Offline exchange | `/p2p-vpn/pairing/1` | 2-byte length plus JSON | 32 KiB |
| Local daemon RPC | `rpc-v1` | ASCII length header plus JSON | 64 KiB request |
| Completion artifact | Native Nix module | Text | 256 KiB response |

The code exchange uses libp2p request-response control streams.

It works over direct TCP, direct QUIC, or circuit relay connections.

## Local RPC Methods

| Method | State Transition |
| --- | --- |
| `pair_open` | Create inviter operation and one-time code. |
| `pair_join` | Create joiner operation from a code. |
| `pair_status` | Read operation phase and diagnostics. |
| `pair_approve` | Issue and apply an authorized grant. |
| `pair_reject` | Terminate a pending candidate. |
| `pair_cancel` | Terminate a local operation. |
| `pair_artifacts` | Return the applied enrollment as native Nix. |
| `pair_acknowledge` | Compact the enrollment into a receipt. |

Mutation methods fail when the daemon has no durable pairing-state path.

`Debug` output redacts the code and secret response material.

## Code and Locator

The code contains 80 random bits.

Display format is 16 Crockford Base32 characters in four groups.

```text
ABCD-EFGH-JKLM-NPQR
```

The DHT key does not contain the code:

```text
locator = SHA-256(domain || network_name || code)
DHT key = /p2p-vpn/pairing-code/<locator>/providers/1
```

The network name prevents cross-overlay locator reuse.

An observer of the provider key cannot complete the PAKE without the code.

## Discovery State Machine

| Time or Event | Action |
| --- | --- |
| Operation starts | Gather and try matching mDNS candidates. |
| 3-second LAN grace passes | Begin provider publication and lookup. |
| 8-second LAN deadline passes | Report public discovery as active. |
| Provider result arrives | Dial the returned peer and known addresses. |
| Relay address is selected | Carry code messages through the circuit. |
| Transport fails | Release attempt and retry another path. |
| `Submit` response is lost | Persist and retry the exact authenticated request. |
| Operation completes | Stop advertising the locator. |

LAN candidates are sorted by prior attempt count and peer ID.

This keeps first attempts deterministic while retaining jittered retries.

### Provider Publication

The inviter republishes after both successful and failed `StartProviding` queries.

One local success does not prove that later DHT peers received the record.

| Setting | Value |
| --- | --- |
| Initial retry | 1 second plus deterministic jitter |
| Maximum retry | 30 seconds plus jitter |
| Maximum publications | 128 per operation |
| Public lookup interval | 10 seconds |

Repeated publication fixes startup races where bootstrap convergence follows open.

## Authenticated Exchange

### Message Sequence

```text
joiner                         inviter
  | Hello(identity, SPAKE A)      |
  |------------------------------>|
  | Challenge(identity, SPAKE B,  |
  |   encrypted signed offer)     |
  |<------------------------------|
  | Submit(signed request, HMAC)  |
  |------------------------------>|
  | Pending(opaque ticket)        |
  |<------------------------------|
  | Poll(ticket)                  |
  |------------------------------>|
  | Accepted(signed response)     |
  |<------------------------------|
```

The inviter may return a typed rejection at any step.

### Cryptographic Bindings

| Material | Binding |
| --- | --- |
| SPAKE2 identities | Network, role, inviter peer, joiner peer. |
| Hello signature | Joiner peer, public key, locator, time, SPAKE message. |
| HKDF keys | Shared secret, network, locator, and both peers. |
| Encrypted offer | ChaCha20-Poly1305 with both hello/challenge payloads as AAD. |
| Challenge signature | Inviter peer, public key, expiry, nonce, ciphertext. |
| Request confirmation | HMAC-SHA256 over the signed request and session. |
| Receipt | SHA-256 of the authenticated request transcript. |

libp2p transport identity must match each signed peer ID.

The challenge also verifies the inviter identity expected by the hello.

## Approval Boundary

The PAKE authenticates code possession.

It does not grant overlay authority.

The inviter exposes one pending candidate through the local control socket.

| Candidate Field | Operator Decision |
| --- | --- |
| Peer ID | Confirm the intended joining host. |
| Public-key fingerprint | Compare through another channel when possible. |
| Requested VPN IP | Accept or override. |
| Requested routes | Grant explicitly or omit. |

Approval IDs contain 256 random bits.

Polling tickets are opaque and bound to the authenticated peer and operation.

## Grant Construction

The inviter signs records for both members.

This gives each side the same restart-safe trust set.

| Condition | Record Roles |
| --- | --- |
| Overlay host only | `overlay_member` |
| Custom address or route grant | `overlay_member`, `route_authority` |

The peer-derived built-in host address needs no extra route grant.

A custom VPN IP becomes an explicit host route grant.

Requested prefixes are not granted unless approval repeats them.

## Durable Enrollment Transaction

Runtime installation is a two-phase operation.

```text
authenticated response
  -> validate full next configuration
  -> persist Prepared enrollment
  -> apply TUN routes and forwarding authority
  -> mark Applied
  -> persist completion
```

This ordering prevents an acknowledged response from being lost before application.

### Restart Reconciliation

| Persisted State | Startup Action |
| --- | --- |
| `Prepared` | Revalidate, apply additive routes and authority, mark `Applied`. |
| `Applied` | Reconstruct forwarding and membership state idempotently. |
| Receipt only | Keep replay evidence; no enrollment payload to apply. |

Conflicting identity, network, offer, response, or transcript data fails closed.

Reconciliation runs before normal packet forwarding begins.

## State Storage

NixOS stores state at:

```text
/var/lib/p2p-vpn/<instance>/pairing-state.json
```

The `.json` suffix names the logical payload.

Bytes on disk are an encrypted binary envelope.

| Property | Implementation |
| --- | --- |
| Key derivation | HKDF-SHA256 from private identity, network, and local peer. |
| Encryption | XChaCha20-Poly1305 with random 24-byte nonce. |
| Maximum plaintext | 512 KiB. |
| File mode | `0600`. |
| Replacement | Same-directory temporary file, fsync, atomic rename. |
| Parent durability | Directory sync after replacement. |
| Unsafe path | Symlink and non-regular files are rejected. |

Received membership keys use a separate owner-only `membership.key` file.

Private key and membership-key contents never enter generated Nix.

Learned signed history uses a separate `membership-state.json` file.

That file is versioned JSON and is not part of the encrypted pairing envelope.

## Acknowledgment and Compaction

`pair artifacts` is available only for an `Applied` enrollment.

Both sides derive the same transcript receipt.

`pair acknowledge` requires that exact digest.

It then:

1. Retains an expiry-bounded replay token.
2. Removes the full offer and response enrollment payload.
3. Stores a compact operation receipt.
4. Removes the active terminal operation.

Acknowledgment is idempotent for the same operation and receipt.

A different receipt for the same operation is a conflict.

## Resource Bounds

| Resource | Bound |
| --- | --- |
| Code expiry | 1 second to 1 hour |
| LAN candidates | 128 peers |
| Addresses per LAN peer | 8 |
| Inbound PAKE sessions | 8 |
| Pending outbound code requests | 32 |
| Attempts per peer | 16 |
| Total peer attempts | 2,048 |
| Durable enrollments | 256 |
| Receipts | 256 |
| Replay tokens | 256 |
| Retained poll tickets | 32 |

The configured per-peer pairing rate limit applies before PAKE processing.

A process-wide token bucket adds a second admission boundary.

## Native Nix Artifact

Each side renders a local module fragment.

| Included | Excluded |
| --- | --- |
| Expected local peer ID | Private identity material |
| Assigned local VPN IP | Membership-key contents |
| Approved local routes | Runtime JSON |
| Signed member records | Static `peers` authorization |
| Managed membership-key path | Redundant module defaults |

Signed records remain revocable.

Rendering static `peers` entries would bypass record revocation and is prohibited.

## Observability

Operation status exposes per-operation diagnostics.

Daemon metrics expose aggregate behavior:

| Metric | Meaning |
| --- | --- |
| `code_pairing_lan_candidates` | Matching mDNS candidates retained. |
| `code_pairing_provider_advertisement_attempts` | DHT publication attempts. |
| `code_pairing_provider_advertisement_failures` | Failed DHT publications. |
| `code_pairing_public_lookups` | DHT provider lookups. |
| `code_pairing_public_providers_found` | Providers returned by lookups. |
| `code_pairing_hello_attempts` | PAKE hello attempts. |
| `code_pairing_hello_retries` | Retried hello attempts. |
| `code_pairing_poll_attempts` | Approval ticket polls. |
| `code_pairing_transport_failures` | Failed request-response paths. |
| `code_pairing_direct_messages` | Code messages on direct paths. |
| `code_pairing_relay_messages` | Code messages on relay paths. |
| `code_pairing_rate_limited` | Admission denied by rate limit. |
| `code_pairing_busy` | Capacity-bound rejection. |
| `code_pairing_completed` | Applied live enrollments. |

Logs expose typed outcomes and non-secret peer or operation context.

Logs must never include the pairing code, membership key, or private key.

## Offline Compatibility

The original signed offer workflow remains available.

| Property | Code Workflow | Offline Workflow |
| --- | --- | --- |
| Human transfer | Short code | `p2pvpn:` offer file |
| PAKE | SPAKE2 | No; offer token is a bearer secret |
| Inviter approval | Required | Offer issuance is authorization |
| Running joiner daemon | Required | Not required during import |
| Native Nix artifacts | Both sides after completion | Joiner accept output |
| JSON persistence | Not rendered | Supported by `pair accept` |

Offline offer signatures, expiry, transport identity checks, and replay checks remain unchanged.

## Verification Matrix

| Layer | Test |
| --- | --- |
| Cryptography | `cargo test pairing_code` |
| Wire codec | `cargo test runtime::pairing_code` |
| Durable state | `cargo test runtime::pairing_sessions` |
| Runtime mutation | `cargo test runtime::runner` |
| Local RPC | `cargo test runtime::control_socket` |
| CLI | `cargo test --test pair_cli` |
| Linux namespace | `tun_namespace_code_pairing_crosses_peerless_overlay` |
| NixOS LAN | `nixos-vm-code-pairing-lan` |
| NixOS relay | `nixos-vm-code-pairing-relay` |

Run the operational proofs:

```sh
nix build .#checks.x86_64-linux.nixos-vm-code-pairing-lan -L
nix build .#checks.x86_64-linux.nixos-vm-code-pairing-relay -L
nix run .#tun-e2e -- \
  tun_namespace_code_pairing_crosses_peerless_overlay \
  -- --ignored --exact --nocapture
```

## Proven Behavior

| Proof | Assertions |
| --- | --- |
| LAN VM | Peerless start, mDNS, approval, traffic, restart, Nix evaluation, acknowledgment. |
| Relay VM | Isolated edge networks, DHT locator, circuit transport, traffic, restart. |
| Namespace | Peerless daemons, code CLI, approval gate, bidirectional five-packet ping. |

The relay VM gives each edge only its local relay address.

The edge nodes have no underlay route to each other.
