# Pairing

Pairing authorizes a new peer without copying a peer ID, key, or offer file.

The default workflow uses a short code and two running daemons.

## Workflow Choice

| Workflow | Use |
| --- | --- |
| `pair open` / `pair join` | Default online workflow. No file transfer. |
| `pair offer` / `pair accept` | Offline fallback for an out-of-band file exchange. |
| Static peer settings | Fully declarative environments with known peer IDs. |

## Network-Wide Result

Pairing is an overlay admission, not a permanent pairwise relationship.

If `B` and `C` each pair only with authorized member `A`, they still learn each other.

The daemons exchange signed histories, derive routes, and discover direct or relay paths.

See [Network Membership](membership.md) for trust and convergence details.

## Prerequisites

Both hosts need:

- The same overlay network name.
- A running daemon with a control socket.
- A persistent local identity.
- LAN or public libp2p reachability.

For NixOS, the same instance name supplies the same network name by default.

## NixOS Code Pairing

This workflow stays in native Nix mode.

It does not create a user-owned JSON configuration.

### 1. Declare the Instance

Add the instance on both hosts:

```nix
{
  services.p2p-vpn.instances.runners.enable = true;
}
```

Apply each host configuration:

```sh
sudo nixos-rebuild switch
```

The module starts each daemon with its configured identity and pairing state.

### 2. Open Pairing

Run on the existing member:

```sh
sudo p2p-vpn pair open --instance runners
```

Record both values:

```text
operation: OPEN_OPERATION
pairing code: ABCD-EFGH-JKLM-NPQR
```

The default window is 10 minutes.

Use a shorter window when needed:

```sh
sudo p2p-vpn pair open \
  --instance runners \
  --expires-in-seconds 120
```

### 3. Join With the Code

Run on the joining host:

```sh
sudo p2p-vpn pair join ABCD-EFGH-JKLM-NPQR \
  --instance runners \
  --no-wait
```

Record the returned `JOIN_OPERATION`.

Omit `--no-wait` to leave the command waiting for approval.

### 4. Verify the Candidate

Get the joining host's public identity:

```sh
sudo p2p-vpn instance show runners
```

Inspect the pending candidate on the inviter:

```sh
sudo p2p-vpn pair status OPEN_OPERATION \
  --instance runners
```

Verify these fields before approval:

| Field | Compare With |
| --- | --- |
| `candidate peer` | Joiner's `instance show` peer ID. |
| `candidate key fingerprint` | A second channel when available. |
| `requested hostname` | Joiner's authenticated overlay DNS label. |
| `requested VPN IP` | The address the joiner requested. |
| `requested route` | Prefixes the joiner wants to originate. |

### 5. Approve

Use the displayed `APPROVAL_ID`:

```sh
sudo p2p-vpn pair approve \
  OPEN_OPERATION APPROVAL_ID \
  --instance runners
```

The requested VPN IP is accepted unless `--vpn-ip` overrides it.

The requested hostname is accepted unless `--hostname` overrides it.

Requested routes require explicit grants:

```sh
sudo p2p-vpn pair approve \
  OPEN_OPERATION APPROVAL_ID \
  --instance runners \
  --route 10.80.0.0/24
```

Reject an unexpected candidate:

```sh
sudo p2p-vpn pair reject \
  OPEN_OPERATION APPROVAL_ID \
  --instance runners
```

### 6. Confirm Completion

Run on each host with its own operation ID:

```sh
sudo p2p-vpn pair status OPERATION --instance runners
```

Required result:

```text
phase: completed
artifacts ready: true
```

The live enrollment is active immediately.

No service restart is required for the first traffic test.

### 7. Render Native Nix

Run on the inviter:

```sh
sudo p2p-vpn pair artifacts OPEN_OPERATION \
  --instance runners \
  --output /etc/nixos/p2p-vpn-runners-paired.nix \
  --force
```

Run on the joiner:

```sh
sudo p2p-vpn pair artifacts JOIN_OPERATION \
  --instance runners \
  --output /etc/nixos/p2p-vpn-runners-paired.nix \
  --force
```

Each command prints a pairing receipt.

The two receipt digests must match.

### 8. Import and Rebuild

Import the local fragment on each host:

```nix
{
  imports = [ ./p2p-vpn-runners-paired.nix ];
}
```

Apply it:

```sh
sudo nixos-rebuild switch
```

The fragment contains public membership and route authorization only.

It contains no private identity or membership-key material.

### 9. Acknowledge Installation

After the rebuilt service is healthy, compact the enrollment on each host:

```sh
sudo p2p-vpn pair acknowledge OPERATION \
  --instance runners \
  --receipt RECEIPT_SHA256
```

Acknowledgment removes the completed artifact payload from durable state.

The installed Nix records remain the declarative authority.

## Stable Addresses and Routes

Without `--vpn-ip`, the joiner uses its peer-derived built-in address.

Request a chosen address on the joiner:

```sh
sudo p2p-vpn pair join CODE \
  --instance runners \
  --vpn-ip 10.44.0.2 \
  --no-wait
```

Request additional originated prefixes with repeated `--route` options.

The inviter must repeat every approved route on `pair approve`.

## Discovery Order

The code is not a network address.

It derives a network-scoped rendezvous locator.

| Stage | Behavior |
| --- | --- |
| LAN | Try mDNS candidates first. |
| Public routing | Query configured Kademlia/bootstrap infrastructure. |
| Relay | Use configured or discovered circuit-relay paths. |
| Recovery | Retry candidates with bounded backoff and jitter. |

Public bootstrap and relay peers provide reachability only.

They never receive VPN membership or route authority.

## Status and Recovery

Inspect an operation:

```sh
sudo p2p-vpn pair status OPERATION --instance runners
```

Useful fields:

| Field | Meaning |
| --- | --- |
| `phase` | Discovery, authentication, approval, or completion state. |
| `discovery` | LAN, public routing, or relay stage. |
| `LAN candidates` | Matching local candidates observed. |
| `pairing attempts` | Bounded authentication attempts and retries. |
| `public discovery` | Provider advertisements and lookup results. |
| `route recovery` | Whether transport failure recovery is active. |
| `pairing transport` | Direct or relay path used for the exchange. |

Cancel an unfinished local operation:

```sh
sudo p2p-vpn pair cancel OPERATION --instance runners
```

Operations and applied enrollments survive daemon restarts.

The state is encrypted and bound to the local identity and network.

## Security Model

| Property | Enforcement |
| --- | --- |
| Code entropy | 80 random bits in a 16-character Crockford Base32 code. |
| Password exchange | SPAKE2; the code is not sent as plaintext. |
| Discovery privacy | DHT records contain a derived locator, not the code. |
| Identity binding | Transcript binds both libp2p identities. |
| Network binding | Locator, offer, records, and state bind the network name. |
| Authorization | Inviter approval is mandatory. |
| Route authority | Inviter signs explicit address and prefix grants. |
| Replay defense | One-time token, expiry, receipts, and replay tracking. |
| Resource safety | Message, candidate, attempt, rate, and state limits. |

Treat the displayed code as a temporary secret.

Do not place it in logs, tickets, or public chat.

## NixOS Secret Handling

The daemon reuses the identity selected by the NixOS module.

This includes identities supplied through agenix or another runtime secret path.

| Secret | Storage |
| --- | --- |
| Private identity | Existing module state or `privateKeyFile`. |
| Pairing state | `/var/lib/p2p-vpn/<instance>/pairing-state.json`. |
| Learned membership | `/var/lib/p2p-vpn/<instance>/membership-state.json`. |
| Received membership key | `/var/lib/p2p-vpn/<instance>/membership.key`. |
| Generated Nix | Public records and secret paths only. |

Secret files are owner-only and never rendered into the Nix store.

## Offline Offer Fallback

Use this only when the two running daemons cannot discover a common path.

### Create an Offer

```sh
sudo p2p-vpn pair offer \
  --nixos-instance runners \
  --output /tmp/runners.pair \
  --force
```

Transfer the file over a trusted channel.

### Accept Into Nix

Stop the unpaired joiner before reusing its identity:

```sh
sudo systemctl stop p2p-vpn-runners.service
```

Accept the offer:

```sh
sudo p2p-vpn pair accept /tmp/runners.pair \
  --nixos-output /etc/nixos/p2p-vpn-runners-paired.nix \
  --nixos-instance runners \
  --nixos-only
```

Import the generated fragment and rebuild.

The inviter does not need a generated fragment for this legacy flow.

### Inspect an Offer

```sh
p2p-vpn pair inspect /tmp/runners.pair
```

Reveal its bearer token only on a trusted terminal:

```sh
p2p-vpn pair inspect /tmp/runners.pair --show-secret
```

## JSON-Managed Hosts

Code pairing requires a durable daemon state path:

```sh
sudo p2p-vpn up \
  --config /etc/p2p-vpn/lab.json \
  --control-socket /run/p2p-vpn/control.sock \
  --pairing-state /var/lib/p2p-vpn/pairing-state.json \
  --membership-state /var/lib/p2p-vpn/membership-state.json
```

The current code artifact renderer emits native Nix.

Use the offline offer workflow when persistent JSON output is required.

## Common Failures

| Symptom | Action |
| --- | --- |
| Pair command cannot reach daemon | Check instance name and control socket. |
| Mutation says durable state is required | Configure `--pairing-state` or use the NixOS module. |
| No LAN candidates | Check mDNS and UDP `5353`. |
| Public lookup finds no provider | Check bootstrap and Kademlia status. |
| Relay transport never appears | Check relay reservation and candidate counters. |
| Candidate is unexpected | Reject it and open a new code. |
| Artifact command fails | Wait for `phase: completed`. |
| Acknowledgment fails | Use the exact receipt printed by `pair artifacts`. |
