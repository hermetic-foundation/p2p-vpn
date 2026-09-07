# Package and Tooling Review

## Results

Seven exported checks built successfully on 2026-09-07 at `c9ccbe82`.
The runtime includes the bootstrap admission fix from `deedd041`.
No source changes or relaxed assertions were needed for these runs.

| Check | Verified Scope |
| --- | --- |
| `package` | Release build, 1,151 passing tests, 19 opt-in tests ignored; installed Zsh completion exists |
| `public-relay-repro-structure` | Packaged script contains expected phase, retry, reservation, and evidence commands |
| `public-vpn-repro-structure` | Generates two-host scripts; checks syntax, targets, cleanup hooks, and artifact references |
| `public-vpn-repro-evidence-structure` | Converts synthetic health, path, state, and metric logs into the expected JSON |
| `public-vpn-capture-structure` | Help/syntax and synthetic capture evidence, including configuration summaries |
| `releaseArchive` | Builds the release tarball from the checked package |
| `releaseArchiveSanity` | Required files, safe archive paths, executable scripts, and extracted CLI help |

### Boundaries

- Package tests are the root package's release-profile tests, not the full workspace suite.
- Tooling checks do not dial public relays, exercise NAT traversal, or carry live VPN traffic.
- Synthetic reports verify parsing and field mapping, not the truth of supplied logs.
- Archive execution uses the existing Nix store; non-Nix portability is not established.
- Debug-bundle, full NixOS consumer activation, and other outstanding gates remain separate.

## Reproduction

```bash
nix build --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link --print-out-paths \
  .#checks.x86_64-linux.public-relay-repro-structure \
  .#checks.x86_64-linux.public-vpn-repro-structure \
  .#checks.x86_64-linux.public-vpn-repro-evidence-structure \
  .#checks.x86_64-linux.public-vpn-capture-structure

nix build --offline --option substitute false --max-jobs 1 --cores 2 \
  --no-link --print-out-paths \
  .#checks.x86_64-linux.releaseArchive \
  .#checks.x86_64-linux.releaseArchiveSanity
```

The first invocation built eight derivations, including the package dependency.
The second reused that package and built two derivations. Both exited zero.
Cargo used two jobs; release compilation took 5m18s, test compilation 3m35s.

- Build substitutions were disabled; no dependencies were downloaded.
- A separate `nix log --offline` lookup attempted configured cache metadata and received HTTP 403; it was not needed for verification.
- Final logs were read directly from `/nix/var/log/nix/drvs/`.
- No VM, emulator, physical device, or production service was started or changed.

## Evidence

Invocation logs:

- `/tmp/p2p-vpn-review-public-tooling.log`
- `/tmp/p2p-vpn-review-release-archive.log`

All output names below are relative to `/nix/store/`.
Check marker outputs are not retained test reports; preserve build logs separately.

| Artifact | Store Output |
| --- | --- |
| Package | `8lp2g8cbc9iqvcd6f648qjgwk3hljzn7-p2p-vpn-0.1.0` |
| Relay structure | `371g3fg2py2xxxcb2jr54dgczs4212za-p2p-vpn-public-relay-repro-structure` |
| Repro structure | `04ln9jlvcb74vffq6xc1p06ppnn9cqyr-p2p-vpn-public-vpn-repro-structure` |
| Repro evidence | `5vpna7gz2746v9k5q069hw2xh64kk27l-p2p-vpn-public-vpn-repro-evidence-structure` |
| Capture structure | `dnxq7c3ah1gsqbawbk67pf6a2jhyla2p-p2p-vpn-public-vpn-capture-structure` |
| Archive | `032x7ym61lqcnjmqil4zaq0vg4x6hnah-p2p-vpn-0.1.0-x86_64-linux.tar.gz` |
| Archive sanity | `mqrmx14q4byhdrawjphxrys6r7619bdi-p2p-vpn-release-archive-sanity` |

| Artifact | SHA-256 |
| --- | --- |
| Package CLI | `4b1db2fc32517e152d8c2b539fff7a913bb50ba2f3120111bb6eb8c56d2f3b84` |
| Archive | `71febf3cd03d32f68cf00fa82f781b730b26af5791c1b6d76ce68eb05a5004f5` |
| Compressed package build log | `94d9fd0d4f7ca53edf1c00548a753f0f537d0b016cee51c7fd81cbfc5f7dcd41` |

The package log is
`/nix/var/log/nix/drvs/7m/isdjiv1x72f2fsyikyb7bajmg4gafs-p2p-vpn-0.1.0.drv.bz2`.
Its test groups report 977, 158, 1, 5, and 10 passing tests, with no failures.
