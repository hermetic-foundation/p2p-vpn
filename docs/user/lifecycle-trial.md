# Demand Lifecycle Trial

## Status

This opt-in build lets unused peer paths become dormant while retaining an
incoming relay path. Application traffic reactivates paths automatically.

It is experimental. Cellular recovery verification is incomplete, and a 10x
battery improvement has not been demonstrated.

## NixOS

Keep the existing upstream module and network configuration. Select the trial
package in the shared module imported by participating hosts:

```nix
{ inputs, pkgs, ... }:
{
  services.p2p-vpn.package =
    inputs.p2p-vpn.packages.${pkgs.stdenv.hostPlatform.system}.demand-lifecycle-trial;
}
```

Update the flake input, then rebuild each participating host:

```sh
nix flake update p2p-vpn
sudo nixos-rebuild switch --flake .#HOSTNAME
```

| Item | Behavior |
| --- | --- |
| Scope | The selected package runs every enabled instance on that host |
| Configuration | No new JSON fields, identities, pairing, or network recreation |
| Cooperation | Both endpoints must run the trial to negotiate dormancy |
| Older peers | Continue legacy behavior; they can prevent the expected idle savings |
| Active traffic | Direct QUIC remains preferred, with existing fallbacks |

## Android

Build the matching APK from the same repository revision:

```sh
nix build .#android-debug-apk-demand-lifecycle-trial
adb install -r result/p2p-vpn-debug.apk
```

Use the same signing key as the installed app. Never uninstall to bypass a
signature mismatch: that would remove its saved networks and identities.

Open the app after updating and confirm the existing networks are enabled.
Updating only the phone does not enable bilateral dormancy with ordinary builds.

## Verify

On NixOS, inspect each instance after the service has restarted:

```sh
sudo p2p-vpn daemon-state \
  --socket /run/p2p-vpn-personal-devices/control.sock
```

| Field | Meaning |
| --- | --- |
| `local capability supports demand lifecycle: true` | The running daemon advertises the experimental protocol |
| `peer lifecycle state` | Reports remote support, maintenance pause, and committed quiet state |
| `quiet true` | Negotiated state, not proof of battery savings or end-to-end reachability |

Verify incoming and outgoing overlay access after leaving the phone unused.
Keep normal battery history for an all-day cellular evaluation; do not reset it.

## Rollback

Select the ordinary package and rebuild participating NixOS hosts:

```nix
services.p2p-vpn.package =
  inputs.p2p-vpn.packages.${pkgs.stdenv.hostPlatform.system}.default;
```

Install the previous Android APK with the same signer using `adb install -r`.
Do not remove network profiles, private keys, or membership state.

## Evidence

- [Protocol design](../developer/demand-driven-lifecycle.md)
- [Earlier battery comparison](../developer/android-cellular-lifecycle-pair-2026-11-13.json)
- [Latest reference capture](../developer/android-lifecycle-reference-capture-2026-11-14.json)
