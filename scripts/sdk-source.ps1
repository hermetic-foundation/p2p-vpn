function Invoke-SdkGit {
    param([string[]]$GitArgs)
    & git @GitArgs
    if ($LASTEXITCODE -ne 0) { throw "git $($GitArgs[0]) failed with exit code $LASTEXITCODE" }
}

function Get-PinnedSdkSource {
    param([string]$Repo)
    $lock = Get-Content -LiteralPath (Join-Path $Repo 'flake.lock') -Raw | ConvertFrom-Json
    $rootNode = $lock.nodes.($lock.root)
    $sdkKey = $rootNode.inputs.sdk
    $pin = $lock.nodes.$sdkKey.locked
    if ($pin.url -ne 'https://github.com/hermetic-foundation/libp2p-vpn.git' -or $pin.rev -notmatch '^[0-9a-f]{40}$') {
        throw 'Invalid SDK pin in flake.lock.'
    }
    $source = Join-Path $Repo "target\sdk-source\$($pin.rev)"
    if (!(Test-Path -LiteralPath (Join-Path $source '.git'))) {
        New-Item -ItemType Directory -Path $source -Force | Out-Null
        Invoke-SdkGit -GitArgs @('init', '--quiet', $source) | Out-Host
    }
    $head = & git -C $source rev-parse --verify --quiet HEAD
    if ($LASTEXITCODE -ne 0) {
        Invoke-SdkGit -GitArgs @('-C', $source, 'fetch', '--no-tags', '--depth', '1', $pin.url, $pin.rev) | Out-Host
        Invoke-SdkGit -GitArgs @('-C', $source, 'checkout', '--quiet', '--detach', 'FETCH_HEAD') | Out-Host
    }
    $head = Invoke-SdkGit -GitArgs @('-C', $source, 'rev-parse', 'HEAD')
    if ($head -ne $pin.rev) { throw 'SDK source does not match flake.lock.' }
    Invoke-SdkGit -GitArgs @('-C', $source, 'diff-index', '--quiet', 'HEAD', '--')
    $untracked = Invoke-SdkGit -GitArgs @('-C', $source, 'ls-files', '--others')
    if ($untracked | Where-Object { $_ -notmatch '^(target/|crates/p2p-vpn-browser/web/pkg/)' }) {
        throw "Unexpected files in pinned SDK checkout: $source"
    }
    return $source
}
