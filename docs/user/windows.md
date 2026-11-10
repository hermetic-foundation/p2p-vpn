# Windows (experimental native CLI)

The Windows x64 port uses Wintun for packets, Windows networking cmdlets for
addresses and routes, and a restricted local named pipe for the existing control
protocol. It runs in a terminal. Windows service installation and automatic OS
split DNS configuration are not included.

## Build

Install a current Rust toolchain supporting edition 2024. The MSVC toolchain needs
Visual Studio Build Tools with the C++ workload; the GNU toolchain needs MinGW-w64.
Windows PowerShell 5.1 and the NetTCPIP/NetAdapter modules are required.

From the repository in an elevated PowerShell (tests need administrator rights):

```powershell
.\scripts\build-windows.ps1 -Configuration debug -Test
```

The script builds `target\debug\p2p-vpn.exe` and downloads Wintun 0.14.1 from
[the official Wintun site](https://www.wintun.net/). It verifies the published
archive SHA-256 and DLL signature, then places the x64 DLL and license beside the
executable. Use `-Configuration release` for an optimized build. Keep these files
together when copying the application. The executable loads Wintun only from its
own directory.

## Configure and run

Run tunnel commands in **PowerShell as Administrator**. Choose a directory you own:

```powershell
$vpn = "$PWD\target\debug\p2p-vpn.exe"
$state = "$env:LOCALAPPDATA\p2p-vpn\lab"
[IO.Directory]::CreateDirectory($state) | Out-Null
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetOwner((New-Object Security.Principal.SecurityIdentifier($sid)))
$acl.SetAccessRuleProtection($true, $false)
foreach ($id in @($sid, 'S-1-5-18', 'S-1-5-32-544')) {
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
        (New-Object Security.Principal.SecurityIdentifier($id)), 'FullControl',
        'ContainerInherit,ObjectInherit', 'None', 'Allow')))
}
Set-Acl -LiteralPath $state -AclObject $acl

& $vpn init-config --network lab --interface pv0 --output "$state\config.json"
& $vpn status --config "$state\config.json"
& $vpn up --config "$state\config.json" --dry-run
& $vpn up --config "$state\config.json" `
    --control-socket "$state\control.sock" `
    --pairing-state "$state\pairing.json" `
    --membership-state "$state\membership.json"
```

Authorize the remote peer through the existing [pairing workflow](pairing.md) or
explicit peer configuration before expecting VPN traffic. Use the same network
name on each device. Retain the generated identity when updating a configuration.
Allow `p2p-vpn.exe` through Windows Firewall on the networks where you intend to
accept peers. Listener ports come from your configuration.

In another elevated terminal, use the same executable and control path:

```powershell
& $vpn daemon-status --socket "$state\control.sock"
& $vpn pair open --socket "$state\control.sock" --rpc-timeout-seconds 30
```

Press Ctrl+C in the daemon terminal to stop. Control paths identify named pipes;
Windows does not create a `.sock` file. Use explicit config/control paths rather
than the NixOS `--instance` and systemd management commands.

## Boundaries

- Durable pairing and checkpoint state require an elevated process and a local
  drive-letter volume. Windows flushes the containing volume after publication,
  using `FlushFileBuffers`; access/flush failures propagate without acknowledging
  a durable checkpoint. Network shares and volumes without a drive letter are
  unsupported. Volume flushes can be slower than Linux directory fsync, so allow
  a longer RPC timeout on busy disks. Physical power-loss testing is outstanding.
- Private state directories require a protected DACL granting access only to the
  current account, SYSTEM, and administrators. Generated Windows files receive
  protected DACLs at creation. Reparse points are rejected for private state.
- Windows serializes state writes with `.p2p-vpn-state.lock`. It retains abandoned
  temporary files for inspection. Unchanged pairing state avoids repeated writes
  after checking file/directory identities, metadata, permissions, and ciphertext.
  Legacy membership-only writes retain the existing weaker directory-sync scope.
- Windows selects source addresses through its own routing stack. Linux per-route
  preferred-source and advertised-MSS settings have no direct Windows equivalent.
- This port needs independent Windows-to-Linux/Android host testing before a
  production or cross-platform interoperability claim.

An elevated local adapter test is available:

```powershell
.\scripts\build-windows.ps1 -Configuration debug -DriverTest
```

It creates an isolated test adapter and a route to `198.18.255.254/32`, sends UDP
from the Windows socket stack through Wintun, injects a reply, and cancels a blocked
packet reader. It does not change default routes or DNS settings.

## Connection-change tests

```powershell
.\scripts\build-windows.ps1 -Configuration debug -RecoveryTest
```

This separate test uses production runtimes and real authenticated TCP/QUIC over
isolated loopback proxies. It cuts active connections, drops datagrams, exercises
alternate endpoints and checks fresh bidirectional packet delivery. TUN is
represented by packet channels; physical adapters and host routing are unchanged.
It also cuts a direct TCP path, verifies traffic switches to a real circuit
relay, then verifies an automatic return to direct when the path is restored.
Three native Windows relay runs recovered through the relay in 16.16–16.21
seconds. The relay is local; public Internet/NAT traversal is not covered.
Results are written under `target\connection-recovery-PID`, or the directory in
`P2P_VPN_RECOVERY_OUTPUT`. See the [recovery validation](../developer/windows-recovery-validation.md)
for results and the failed attempts that led to fixes.

The Windows CLI currently relies on transport failure detection rather than an
OS network-change notification subscription. A physical Wi-Fi/Ethernet/hotspot
handoff, discovery of a previously unknown peer address, and public Internet
relay availability still need separate qualification. Relay fallback requires a
reachable relay and an accepted reservation for the destination peer.
