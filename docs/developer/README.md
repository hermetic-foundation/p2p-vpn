# Developer Documentation

Use these docs when changing, testing, or debugging `p2p-vpn`.

## Contents

| Document | Use It For |
| --- | --- |
| [Architecture](architecture.md) | Runtime and protocol layout. |
| [Feature Matrix](feature-matrix.md) | Current implementation status. |
| [Testing](testing.md) | Unit, Nix, namespace, and two-host tests. |
| [Network Debugging](network-debugging.md) | Artifact capture and failure triage. |
| [Public Bootstrap Smoke](public-bootstrap-smoke.md) | Recorded public reachability evidence. |

## Development Shell

```sh
nix develop
```

## Fast Check

```sh
nix run .#check-fast
```

## Full Local Check

```sh
nix run .#check-operational
```

Use `nix flake check` when you want every exported check.

Some checks need Linux namespace and TUN privileges.

See [Testing](testing.md) before treating skipped namespace checks as failures.
