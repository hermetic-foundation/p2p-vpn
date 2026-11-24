param([switch]$TestHooks, [switch]$NativeFixture)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$lock = Get-Content -LiteralPath (Join-Path $repo 'flake.lock') -Raw | ConvertFrom-Json
$rootNode = $lock.nodes.($lock.root)
$sdkKey = $rootNode.inputs.sdk
$sdkNode = $lock.nodes.$sdkKey.locked
if ($sdkNode.url -ne 'https://github.com/hermetic-foundation/libp2p-vpn.git' -or $sdkNode.rev -notmatch '^[0-9a-f]{40}$') {
    throw 'Invalid SDK pin in flake.lock.'
}
$sdk = Join-Path $repo "target\browser-sdk-source\$($sdkNode.rev)"

function Invoke-Git {
    param([string[]]$GitArgs)
    & git @GitArgs
    if ($LASTEXITCODE -ne 0) { throw "git $($GitArgs[0]) failed with exit code $LASTEXITCODE" }
}

if (!(Test-Path -LiteralPath (Join-Path $sdk '.git'))) {
    New-Item -ItemType Directory -Path $sdk -Force | Out-Null
    Invoke-Git -GitArgs @('init', '--quiet', $sdk)
}
$head = & git -C $sdk rev-parse --verify --quiet HEAD
if ($LASTEXITCODE -ne 0 -or $head -ne $sdkNode.rev) {
    Invoke-Git -GitArgs @('-C', $sdk, 'fetch', '--no-tags', '--depth', '1', $sdkNode.url, $sdkNode.rev)
    Invoke-Git -GitArgs @('-C', $sdk, 'checkout', '--quiet', '--detach', 'FETCH_HEAD')
}
$head = & git -C $sdk rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $head -ne $sdkNode.rev) { throw 'SDK source does not match flake.lock.' }
& git -C $sdk diff-index --quiet HEAD --
if ($LASTEXITCODE -ne 0) { throw 'The SDK build checkout contains modified tracked files.' }

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
Write-Host "Staged browser bindings from SDK $($sdkNode.rev) in $appPkg"
