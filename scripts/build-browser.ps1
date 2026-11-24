param([switch]$TestHooks, [switch]$NativeFixture)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
. (Join-Path $PSScriptRoot 'sdk-source.ps1')
$sdk = Get-PinnedSdkSource -Repo $repo

$previousTarget = $env:CARGO_TARGET_DIR
try {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    & (Join-Path $sdk 'scripts\build-browser.ps1') -TestHooks:$TestHooks -NativeFixture:$NativeFixture
} finally {
    $env:CARGO_TARGET_DIR = $previousTarget
}
$sdkPkg = Join-Path $sdk 'crates\p2p-vpn-browser\web\pkg'
$appPkg = Join-Path $repo 'crates\p2p-vpn-browser\web\pkg'
New-Item -ItemType Directory -Path $appPkg -Force | Out-Null
foreach ($file in @('p2p_vpn_browser.js', 'p2p_vpn_browser.d.ts', 'p2p_vpn_browser_bg.wasm', 'p2p_vpn_browser_bg.wasm.d.ts')) {
    Copy-Item -LiteralPath (Join-Path $sdkPkg $file) -Destination $appPkg
}
Copy-Item -LiteralPath (Join-Path $repo 'assets\p2p-vpn-logo.svg') -Destination (Join-Path $appPkg 'logo.svg')
if ($NativeFixture) {
    $output = Join-Path $repo 'target\debug'
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $sdk 'target\debug\browser-fixture.exe') -Destination $output
    Write-Host 'For Windows TUN qualification, place the official signed x64 wintun.dll in target\debug beside browser-fixture.exe.'
}
Write-Host "Staged browser bindings from SDK $(Split-Path -Leaf $sdk) in $appPkg"
