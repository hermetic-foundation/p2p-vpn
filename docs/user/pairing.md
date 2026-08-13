# Pairing

Pairing authorizes a new peer without manually exchanging peer IDs or keys.

The inviter must already be running.

## NixOS Workflow

This workflow stays in native Nix mode.

It does not create or import a user-owned JSON configuration.

### 1. Declare Both Instances

Use the same name on both hosts:

```nix
{
  services.p2p-vpn.instances.runners.enable = true;
}
```

Apply the configuration on both hosts:

```sh
sudo nixos-rebuild switch
```

This creates each host's persistent identity.

### 2. Create an Offer

Run on the existing member:

```sh
sudo p2p-vpn pair offer \
  --nixos-instance runners \
  --output /tmp/runners.pair \
  --force
```

`--nixos-instance` reads the module runtime for that instance.

The default offer expires after 10 minutes.

### 3. Transfer the Offer

Transfer `/tmp/runners.pair` over a trusted channel.

The file contains a one-line `p2pvpn:` URI.

### 4. Accept into Nix

Stop the unpaired joiner to avoid two processes using one identity:

```sh
sudo systemctl stop p2p-vpn-runners.service
```

Accept the offer:

```sh
sudo p2p-vpn pair accept /tmp/runners.pair \
  --nixos-output /etc/nixos/p2p-vpn-runners.nix \
  --nixos-instance runners \
  --nixos-only
```

The command reuses this identity when it exists:

```text
/var/lib/p2p-vpn/runners/private.key
```

If the key does not exist, the command creates it with mode `0600`.

### 5. Import and Switch

```nix
{
  imports = [ ./p2p-vpn-runners.nix ];
}
```

```sh
sudo nixos-rebuild switch
```

The generated file uses typed `services.p2p-vpn.instances` options.

It omits settings already supplied by module defaults.

### 6. Verify

```sh
sudo p2p-vpn daemon-health \
  --socket /run/p2p-vpn-runners/control.sock \
  --require-validated-peers \
  --require-supported-paths
```

Use the peer's overlay address for a traffic test.

## Stable Address Request

Without `--vpn-ip`, the joiner uses its peer-derived built-in address.

Request a fixed address when services need a known IP:

```sh
sudo p2p-vpn pair accept runners.pair \
  --nixos-output /etc/nixos/p2p-vpn-runners.nix \
  --nixos-instance runners \
  --nixos-only \
  --vpn-ip 10.44.0.2
```

The inviter signs the requested host route into the member record.

Request additional originated prefixes with repeated `--local-route` options.

## Offer Inspection

Inspect an offer before accepting it:

```sh
p2p-vpn pair inspect runners.pair
```

| Field | Meaning |
| --- | --- |
| `pairing offer` | Signature and expiry state |
| `network` | Overlay being joined |
| `inviter peer` | Signing peer identity |
| `discovery only` | Whether direct inviter hints were omitted |
| `inviter address hints` | Signed initial dial paths |
| `bootstrap peers` | Signed discovery seeds |
| `rendezvous token` | Hidden one-time secret |

Reveal the token only on a trusted terminal:

```sh
p2p-vpn pair inspect runners.pair --show-secret
```

## Offer Expiry

Set a shorter expiry for exposed transfer paths:

```sh
sudo p2p-vpn pair offer \
  --nixos-instance runners \
  --expires-in-seconds 120 \
  --output /tmp/runners.pair \
  --force
```

Accepted tokens cannot be replayed during the inviter daemon lifetime.

Create a new offer for another peer.

## Discovery-Only Offer

Omit direct inviter addresses from the URI:

```sh
sudo p2p-vpn pair offer \
  --nixos-instance runners \
  --discovery-only \
  --output /tmp/runners.pair \
  --force
```

The joiner must find the inviter through mDNS, Kademlia, bootstrap, or relay hints.

Use this mode only when those discovery paths are already reachable.

## Identity Rules

| Situation | Result |
| --- | --- |
| Secure existing key matches | Key is reused and reported as `kept` |
| Key is missing | New key is generated |
| Existing key has permissive mode | Accept fails |
| Existing key content conflicts | Accept fails without `--force` |
| Existing path is a symlink | Accept fails |

Treat `--force` as permission to replace existing output or secret state.

Supplying a different key rotates the peer ID and invalidates old authorization.

## Signed Membership

The inviter returns an inviter-signed member record.

The record binds:

- Network name.
- Joiner peer ID and public key.
- Overlay-member role.
- Requested address and route grants.
- Sequence and membership epoch.

The joiner stores the record in generated Nix.

After an inviter restart, the joiner presents the record again for verification.

## Optional Membership Key

If the inviter uses `membershipKeyFile`, pairing transfers that key securely.

The joiner stores it outside the Nix store and references it from generated Nix.

The signed member record is still issued for restart and route authorization.

## Generic JSON Workflow

Use this only for non-NixOS or JSON-managed hosts.

### Offer

```sh
p2p-vpn pair offer \
  --config /etc/p2p-vpn/lab.json \
  --output lab.pair
```

### Accept

```sh
p2p-vpn pair accept lab.pair \
  --output /etc/p2p-vpn/lab.json
```

The default output is `p2p-vpn.json`.

Use `--private-key` only when an existing JSON workflow supplies the identity.

Do not combine this JSON output with native Nix instance settings.

## Offline Response Import

Import a response produced through an out-of-band exchange:

```sh
p2p-vpn pair accept lab.pair \
  --response pairing-response.json \
  --output /etc/p2p-vpn/lab.json
```

Normal pairing performs the encrypted libp2p exchange automatically.

## Accept Options

| Option | Purpose |
| --- | --- |
| `--nixos-output` | Write a typed NixOS module file |
| `--nixos-instance` | Select its instance and default state path |
| `--nixos-only` | Do not write JSON output |
| `--nixos-state-dir` | Override the per-instance secret directory |
| `--vpn-ip` | Request a stable joiner overlay address |
| `--local-route` | Request an additional originated prefix |
| `--peer-name` | Label the inviter in generated config |
| `--interface` | Override generated interface name |
| `--mtu` | Override generated TUN MTU |
| `--timeout-seconds` | Extend live discovery and exchange time |
| `--force` | Replace output or conflicting secret state |

## Failure Diagnostics

Live failures report compact path counters.

| Field | Check |
| --- | --- |
| `inviter_hints` | Signed direct dial hints were present |
| `relayed_inviter_hints` | Circuit-relay hints were present |
| `bootstrap_peers` | Discovery seeds were present |
| `request_attempts` | Pairing request reached send stage |
| `outbound_failures` | Request-response transport failed |
| `dial_errors` | No candidate connection completed |
| `relayed_dial_start_failures` | Relay path was rejected locally |

The error does not print the URI or token.

## Security Rules

- Treat the URI as a short-lived secret.
- Verify the network and inviter peer before accepting.
- Transfer offers over an authenticated channel.
- Keep generated key files root-owned and mode `0600`.
- Use one offer per joining peer.
- Delete expired offer files.

Protocol details and counters are in
[Pairing Implementation](../developer/pairing.md).
