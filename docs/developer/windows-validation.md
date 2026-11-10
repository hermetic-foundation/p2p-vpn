# Native Windows validation — 2026-11-09

This is an experimental Windows x64 CLI port. It is not a Windows service or GUI.
The executable and Wintun DLL were built and exercised locally; no repository
commit, remote deployment, or firewall rule was created.

## Environment and reproduction

- Windows 11 Home, version `10.0.26200`, elevated execution.
- Rust `1.97.0`, `x86_64-pc-windows-gnu`, Rust's bundled LLD linker.
- Debug profile without debug information for this validation
  (`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`).
- Official Wintun 0.14.1 AMD64 DLL. The published archive SHA-256 matched
  `07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51`;
  Windows reported its Authenticode signature as valid.

```powershell
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
.\scripts\build-windows.ps1 -Configuration debug -DriverTest
```

The script selects bundled LLD for a GNU toolchain, verifies and packages Wintun,
and runs the following focused tests. MSVC compilation was not tested on this
machine because the Visual C++ linker was unavailable.

| Check | Result |
| --- | --- |
| CLI help, private config creation, status, Windows dry-run | 1 passed |
| Encrypted state save/replace/reload, cached unchanged ciphertext, corruption recovery, wrong-key and inherited-ACL rejection | 1 passed |
| Bounded control protocol over restricted Windows named pipes, including malformed/truncated frames | 27 passed |
| Fresh checkpoint formation, publication, and restore, with packet authority initially gated | 1 passed |
| Signed checkpoint pairing approval through authenticated loopback TCP | 1 passed |
| Windows socket → Wintun → injected UDP reply → Windows socket, plus blocked-reader cancellation | 1 passed |
| Total focused Windows tests | **32 passed** |
| Windows `cargo check --tests --locked` | Passed |
| Linux/WSL `cargo check --tests --locked` | Passed; execution of the Linux suite was not performed |
| Formatting and whitespace checks | Passed |

The full Rust test suite and NixOS VM campaigns were not run. Unix permission,
Unix socket, and Linux namespace/resource tests retain their platform gates.

## Live CLI smoke test

A separate daemon used a protected local state directory, an isolated Wintun
adapter, and a loopback TCP listener. The CLI successfully queried live status,
opened a code-pairing operation, and persisted checkpoint authority. After clean
shutdown and restart, `pair status` returned the same operation identifier.
The restart preserved the existing checkpoint resynchronization gate. Shutdown
completed and the test adapter disappeared. No test daemon or adapter was left
running.

The live smoke test did not admit a second physical device. The loopback TCP
approval test and local Wintun UDP test establish separate integration boundaries;
they do not establish an end-to-end Windows-to-Linux/Android VPN connection.

## Implementation boundaries

Windows protects private files with DACLs and serializes state writers with a
private lock file. State directories exclude untrusted accounts before temporary
files inherit access; file DACLs are protected before secret contents are written.
Other output directories use atomic private-file creation through Windows
PowerShell. Permission checks use native Windows APIs rather than spawning a
shell in the runtime's persistence loop.

Checkpoint and pairing publication flush the containing local volume using an
elevated volume handle and `FlushFileBuffers`. Failures propagate; there is no
silent directory-sync downgrade for these stores. The unchanged-pairing-state
cache checks live file and directory identities, metadata, DACLs, and ciphertext.
Physical power-loss/crash durability testing remains outstanding. Legacy
membership-only persistence retains its weaker directory-sync scope.

Windows source-address selection differs from Linux preferred-source routing;
per-route Linux advertised-MSS settings are not reproduced. There is no service
installer, automatic OS split DNS setup, or independent cross-device qualification.
