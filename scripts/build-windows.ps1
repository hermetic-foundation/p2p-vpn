param(
    [ValidateSet('debug', 'release')][string]$Configuration = 'release',
    [switch]$Test,
    [switch]$DriverTest,
    [switch]$RecoveryTest
)
$ErrorActionPreference = 'Stop'
function Invoke-Cargo {
    # PowerShell 5.1 otherwise turns normal Cargo progress on stderr into errors
    # when the calling terminal captures output. Check the native exit code.
    $ErrorActionPreference = 'Continue'
    & cargo @args 2>&1 | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { throw "cargo failed with exit code $LASTEXITCODE" }
}
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $repo
$previousPath = $env:PATH
$previousFlags = $env:RUSTFLAGS
try {
    $hostInfo = (& rustc -vV) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Install Rust for Windows first.' }
    if ($hostInfo -notmatch 'host: x86_64-pc-windows-(gnu|msvc)') {
        throw 'This build script currently packages Windows x64 only.'
    }
    if ($hostInfo -match 'host: x86_64-pc-windows-gnu') {
        # Rust ships LLD. GNU ld can stall linking this large crate on Windows.
        $sysroot = & rustc --print sysroot
        $lldDirectory = Join-Path $sysroot 'lib\rustlib\x86_64-pc-windows-gnu\bin\gcc-ld'
        if (!(Test-Path -LiteralPath (Join-Path $lldDirectory 'ld.lld.exe'))) { throw 'The Rust LLD linker is missing.' }
        $env:PATH = "$lldDirectory;$env:PATH"
        $env:RUSTFLAGS = "$previousFlags -C link-arg=-fuse-ld=lld".Trim()
    }
    $cargoArguments = @('build', '--locked', '--bin', 'p2p-vpn')
    if ($Configuration -eq 'release') { $cargoArguments += '--release' }
    Invoke-Cargo @cargoArguments

    $archive = Join-Path $repo 'target\wintun-0.14.1.zip'
    if (!(Test-Path -LiteralPath $archive)) {
        Invoke-WebRequest 'https://www.wintun.net/builds/wintun-0.14.1.zip' -OutFile $archive
    }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne '07C256185D6EE3652E09FA55C0B673E2624B565E02C4B9091C79CA7D2F24EF51') {
        throw 'Wintun archive failed the published SHA-256 check.'
    }
    $extracted = Join-Path $repo 'target\wintun-0.14.1'
    Expand-Archive -LiteralPath $archive -DestinationPath $extracted -Force
    $dll = Join-Path $extracted 'wintun\bin\amd64\wintun.dll'
    if ((Get-AuthenticodeSignature -LiteralPath $dll).Status -ne 'Valid') { throw 'Wintun signature is not valid.' }
    $binaryDirectory = Join-Path $repo "target\$Configuration"
    Copy-Item -LiteralPath $dll -Destination (Join-Path $binaryDirectory 'wintun.dll')
    Copy-Item -LiteralPath (Join-Path $extracted 'wintun\LICENSE.txt') -Destination (Join-Path $binaryDirectory 'wintun-LICENSE.txt')
    if ($Test -or $DriverTest) {
        $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
        if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run Windows persistence tests from an elevated PowerShell.' }
        Invoke-Cargo @('test', '--locked', '--test', 'windows_platform', '--', '--include-ignored')
        Invoke-Cargo test --locked --lib runtime::control_socket::tests
        Invoke-Cargo test --locked --lib fresh_formation_accepts_empty_or_self_only_startup_history_and_returns_gated
        Invoke-Cargo test --locked --lib checkpoint_pair_rpc_approval_crosses_authenticated_tcp_pairing_transport
    }
    if ($DriverTest) {
        $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
        if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run the driver test from an elevated PowerShell.' }
        Copy-Item -LiteralPath $dll -Destination (Join-Path $repo 'target\debug\deps\wintun.dll')
        Invoke-Cargo @('test', '--locked', '--lib', 'wintun_udp_roundtrip_and_blocked_read_cancellation', '--', '--ignored', '--nocapture')
    }
    if ($RecoveryTest) {
        Invoke-Cargo @('test', '--locked', '--test', 'connection_recovery', '--', '--nocapture', '--test-threads=1')
    }
    Write-Host "Built $(Join-Path $binaryDirectory 'p2p-vpn.exe')"
} finally {
    $env:PATH = $previousPath
    $env:RUSTFLAGS = $previousFlags
    Pop-Location
}
