# SDK dependency

The native CLI links `libp2p-vpn` from Git. The Nix flake consumes Android
and browser packages from the same revision. The app owns its command
line, UI, Android VPN service, NixOS module, and release packaging.

| Consumer | Pin | Verification |
| --- | --- | --- |
| Rust CLI | `Cargo.toml` and `Cargo.lock` | CLI pairing and namespace tests |
| Browser | `flake.nix` and `flake.lock` | Chromium with separately built bindings |
| Android | Same flake pin | APK installs JNI and Java from the SDK AAR |

## Update the SDK

1. Change the SDK revision in `Cargo.toml` and `flake.nix`. Keep any
   direct libp2p and test dependencies on the same Git source and revision.
2. Refresh both lockfiles. Nix verifies the SDK revision and uses its
   locked source hash for Cargo's Git dependency.
3. Run the app and SDK checks before publishing. Test Android traffic
   through a real network change before claiming device parity.

```sh
nix flake lock
nix develop -c cargo check --all-targets
nix develop -c cargo test --locked --test pair_cli
nix flake check --no-build
bash scripts/build-browser.sh
```

For an app cutover rollback, redeploy the last app revision before the
cutover. To roll back in source control, revert the cutover and its
follow-up packaging commits together; the new scripts require the SDK.

For a later SDK update, restore both manifest pins and both lockfiles.
Neither rollback changes device identity files or network membership.
