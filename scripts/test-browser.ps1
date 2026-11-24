param(
    [switch]$SkipBuild, [string]$WebRtcIp = '', [int]$Port = 8787,
    [switch]$Lan, [string]$ClientIp = '', [switch]$LocalSmoke,
    [ValidateRange(30,3300)][int]$TimeoutSeconds = 1800
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'The browser/TUN qualification fixture requires an elevated PowerShell.'
}
if ($LocalSmoke -and !$Lan) { throw '-LocalSmoke requires -Lan.' }
if ($ClientIp -and !$Lan) { throw '-ClientIp requires -Lan.' }
if ($Lan) {
    if (!$WebRtcIp -or !$ClientIp) { throw '-Lan requires -WebRtcIp NATIVE_LAN_IP and -ClientIp BROWSER_DEVICE_LAN_IP.' }
    foreach ($address in @($WebRtcIp, $ClientIp)) {
        $ip = [Net.IPAddress]::Parse($address)
        $bytes = $ip.GetAddressBytes()
        if ($ip.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork -or !($bytes[0] -eq 10 -or ($bytes[0] -eq 172 -and $bytes[1] -ge 16 -and $bytes[1] -le 31) -or ($bytes[0] -eq 192 -and $bytes[1] -eq 168))) { throw 'The LAN fixture requires private IPv4 underlay addresses.' }
        if ($address -in @('10.203.0.1', '10.203.0.2')) { throw 'Use underlay LAN addresses, not test overlay addresses.' }
    }
    $nativeAddresses = @(Get-NetIPAddress -AddressFamily IPv4 | Select-Object -ExpandProperty IPAddress)
    if ($WebRtcIp -notin $nativeAddresses) { throw '-WebRtcIp must belong to this native host.' }
    if ($LocalSmoke) {
        if ($ClientIp -ne $WebRtcIp) { throw '-LocalSmoke requires -ClientIp equal to -WebRtcIp.' }
    } elseif ($ClientIp -in $nativeAddresses) { throw 'A LAN qualification needs a second device; this ClientIp belongs to the native host.' }
}
function Invoke-Checked {
    param([string]$Executable, [string[]]$Arguments)
    $ErrorActionPreference = 'Continue'
    & $Executable @Arguments 2>&1 | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { throw "$Executable failed with exit code $LASTEXITCODE" }
}
Push-Location $repo
$fixture = $null
$firewallRule = $null
$udpFirewallRule = $null
$createdFirewallRules = @()
$evidence = $null
$completed = $false
$environmentNames = @('P2P_VPN_BROWSER_URL','P2P_VPN_BROWSER_REPORT','P2P_VPN_BROWSER_SCREENSHOT','P2P_VPN_BROWSER_KEEP_FIXTURE')
$previousEnvironment = @{}
foreach ($name in $environmentNames) { $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    if (!$SkipBuild) { & (Join-Path $PSScriptRoot 'build-browser.ps1') -TestHooks -NativeFixture }
    foreach ($ip in @('10.203.0.1','10.203.0.2')) {
        if (Get-NetIPAddress -IPAddress $ip -ErrorAction SilentlyContinue) { throw "Test overlay address $ip is already in use." }
    }
    $driver = Join-Path $repo 'target\debug\wintun.dll'
    if (!(Test-Path -LiteralPath $driver)) {
        $archive = Join-Path $repo 'target\wintun-0.14.1.zip'
        if (!(Test-Path -LiteralPath $archive)) { Invoke-WebRequest 'https://www.wintun.net/builds/wintun-0.14.1.zip' -OutFile $archive }
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne '07C256185D6EE3652E09FA55C0B673E2624B565E02C4B9091C79CA7D2F24EF51') { throw 'Wintun archive checksum mismatch.' }
        $extract = Join-Path $repo 'target\wintun-0.14.1'
        Expand-Archive -LiteralPath $archive -DestinationPath $extract -Force
        Copy-Item -LiteralPath (Join-Path $extract 'wintun\bin\amd64\wintun.dll') -Destination $driver
    }
    if ((Get-AuthenticodeSignature -LiteralPath $driver).Status -ne 'Valid') { throw 'Wintun DLL signature is invalid.' }
    $prefix = if ($Lan) { if ($LocalSmoke) { 'browser-lan-smoke-' } else { 'browser-lan-' } } else { 'browser-validation-' }
    $evidence = Join-Path $repo ('target\' + $prefix + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss'))
    New-Item -ItemType Directory -Path $evidence | Out-Null
    $stdout = Join-Path $evidence 'native-stdout.log'
    $stderr = Join-Path $evidence 'native-stderr.log'
    $appWeb = Join-Path $repo 'crates\p2p-vpn-browser\web'
    $fixtureArguments = @('--port', "$Port", '--web-root', ('"' + $appWeb + '"'))
    if ($WebRtcIp) { $fixtureArguments += @('--webrtc-ip', $WebRtcIp) }
    $clientDirectory = Join-Path $evidence 'client'
    if ($Lan) { $fixtureArguments += @('--export-client', ('"' + $clientDirectory + '"')) }
    $fixture = Start-Process -FilePath (Join-Path $repo 'target\debug\browser-fixture.exe') -ArgumentList $fixtureArguments -WorkingDirectory $repo -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # Retain the handle now: Windows PowerShell can otherwise lose ExitCode
    # when the child exits before WaitForExit is called.
    $null = $fixture.Handle
    $readyDeadline = [DateTime]::UtcNow.AddSeconds(90)
    $session = $null
    while (!$session) {
        if ($fixture.HasExited) { throw 'Native fixture exited before becoming ready.' }
        try { $session = Invoke-RestMethod "http://127.0.0.1:$Port/session.json" -TimeoutSec 1 } catch {}
        if ([DateTime]::UtcNow -gt $readyDeadline) { throw 'Native fixture startup timed out.' }
        if (!$session) { Start-Sleep -Milliseconds 200 }
    }
    if ($session.native_interface -ne "p2p-wasm-$($fixture.Id)") { throw 'Loopback session does not belong to this fixture.' }
    # The test sends real IP packets into Windows, so its TCP service needs a
    # narrowly scoped inbound rule. Never change a firewall profile or default.
    $firewallRule = 'p2p-vpn-browser-test-' + [Guid]::NewGuid().ToString('N')
    New-NetFirewallRule -Name $firewallRule -DisplayName $firewallRule -Direction Inbound -Action Allow -Profile Any -Protocol TCP -LocalAddress '10.203.0.1' -RemoteAddress '10.203.0.2' -LocalPort $session.overlay_port -InterfaceAlias $session.native_interface -Program (Join-Path $repo 'target\debug\browser-fixture.exe') | Out-Null
    $createdFirewallRules += $firewallRule
    $env:P2P_VPN_BROWSER_REPORT = Join-Path $evidence 'browser.json'
    $env:P2P_VPN_BROWSER_SCREENSHOT = Join-Path $evidence 'browser.png'
    if ($Lan) {
        $udpFirewallRule = 'p2p-vpn-browser-test-' + [Guid]::NewGuid().ToString('N')
        New-NetFirewallRule -Name $udpFirewallRule -DisplayName $udpFirewallRule -Direction Inbound -Action Allow -Profile Any -Protocol UDP -LocalAddress $WebRtcIp -RemoteAddress $ClientIp -LocalPort $session.webrtc_port -Program (Join-Path $repo 'target\debug\browser-fixture.exe') | Out-Null
        $createdFirewallRules += $udpFirewallRule
        $archive = Join-Path $evidence 'client.zip'
        Compress-Archive -Path (Join-Path $clientDirectory '*') -DestinationPath $archive
        $metadata = [ordered]@{
            scope = $(if ($LocalSmoke) { 'same-host-split-harness-smoke' } else { 'two-device-lan' })
            session_id = $session.session_id; native_host = $session.native_host
            native_ipv4 = $nativeAddresses; client_ip = $ClientIp
            webrtc_ip = $session.webrtc_ip; webrtc_port = $session.webrtc_port
            browser_peer_id = $session.config.network.local_peer
            dial_address = $session.dial_address; expires_at = $session.expires_at
            client_archive_sha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
            native_executable_sha256 = (Get-FileHash -LiteralPath (Join-Path $repo 'target\debug\browser-fixture.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
            wasm_sha256 = (Get-FileHash -LiteralPath (Join-Path $clientDirectory 'pkg\p2p_vpn_browser_bg.wasm') -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        $metadataPath = Join-Path $evidence 'native-session.json'
        $metadata | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $metadataPath -Encoding UTF8
        $incoming = Join-Path $evidence 'client-report.json'
        Write-Host "Client bundle: $archive"
        Write-Host "Bundle SHA256: $($metadata.client_archive_sha256)"
        Write-Host "On the second device: expand client.zip, run npm ci --no-audit --no-fund, then node client.mjs --test."
        Write-Host "Return its client-report.json to: $incoming"
        if ($LocalSmoke) {
            Push-Location $clientDirectory
            try {
                Invoke-Checked -Executable npm.cmd -Arguments @('ci', '--no-audit', '--no-fund')
                Invoke-Checked -Executable node -Arguments @('client.mjs', '--test', '--local-smoke')
            } finally { Pop-Location }
            Copy-Item -LiteralPath (Join-Path $clientDirectory 'client-report.json') -Destination $incoming
            Copy-Item -LiteralPath (Join-Path $clientDirectory 'client.png') -Destination $env:P2P_VPN_BROWSER_SCREENSHOT
        }
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (!(Test-Path -LiteralPath $incoming)) {
            if ($fixture.HasExited) { throw 'Native fixture exited while awaiting the client report.' }
            if ([DateTime]::UtcNow -gt $deadline) { throw 'Timed out awaiting the second-device report; no LAN qualification was recorded.' }
            Start-Sleep -Milliseconds 500
        }
        # A file copy can be visible before its JSON is complete.
        $clientReport = $null
        while (!$clientReport) {
            try { $clientReport = Get-Content -LiteralPath $incoming -Raw | ConvertFrom-Json } catch {}
            if ([DateTime]::UtcNow -gt $deadline) { throw 'Client report is incomplete or invalid JSON.' }
            if (!$clientReport) { Start-Sleep -Milliseconds 200 }
        }
        Copy-Item -LiteralPath $incoming -Destination $env:P2P_VPN_BROWSER_REPORT
        $null = Invoke-WebRequest "http://127.0.0.1:$Port/shutdown" -Method Post -UseBasicParsing -Headers @{'X-Fixture-Token'=$session.shutdown_token} -TimeoutSec 3
    } else {
        $env:P2P_VPN_BROWSER_URL = "http://127.0.0.1:$Port"
        $env:P2P_VPN_BROWSER_KEEP_FIXTURE = '0'
        Push-Location (Join-Path $repo 'crates\p2p-vpn-browser')
        try {
            Invoke-Checked -Executable npm.cmd -Arguments @('ci', '--no-audit', '--no-fund')
            Invoke-Checked -Executable node -Arguments @('tests/e2e.mjs')
        } finally { Pop-Location }
    }
    if (!$fixture.WaitForExit(15000)) { throw "Fixture PID $($fixture.Id) did not shut down within 15 seconds." }
    $fixture.Refresh()
    if ($fixture.ExitCode -ne 0) { throw "Native fixture exited with code $($fixture.ExitCode)." }
    if (Get-NetAdapter -Name "p2p-wasm-$($fixture.Id)" -ErrorAction SilentlyContinue) { throw 'The test adapter was not removed.' }
    Remove-NetFirewallRule -Name $firewallRule
    if (Get-NetFirewallRule -Name $firewallRule -ErrorAction SilentlyContinue) { throw 'The test firewall rule was not removed.' }
    $firewallRule = $null
    if ($udpFirewallRule) {
        Remove-NetFirewallRule -Name $udpFirewallRule
        if (Get-NetFirewallRule -Name $udpFirewallRule -ErrorAction SilentlyContinue) { throw 'The UDP test firewall rule was not removed.' }
        $udpFirewallRule = $null
    }
    $nativeLog = (Get-Content -LiteralPath $stdout -Raw) + (Get-Content -LiteralPath $stderr -Raw)
    $counter = [regex]::Matches($nativeLog, '(?m)^\s*outbound_direct_webrtc_stream_fallback_packets (\d+)\s*$')
    if (!$counter.Count -or [long]$counter[$counter.Count-1].Groups[1].Value -le 0) { throw 'Native WebRTC payload counter did not prove delivery.' }
    $sourceDrops = [regex]::Matches($nativeLog, '(?m)^\s*inbound_drop_unauthorized_source_packets (\d+)\s*$')
    if (!$sourceDrops.Count -or [long]$sourceDrops[$sourceDrops.Count-1].Groups[1].Value -le 0) { throw 'Native counter did not prove rejection of the spoofed source.' }
    $report = Get-Content -LiteralPath $env:P2P_VPN_BROWSER_REPORT -Raw | ConvertFrom-Json
    $report | Add-Member -NotePropertyName native_webrtc_outbound_packets -NotePropertyValue ([long]$counter[$counter.Count-1].Groups[1].Value)
    $report | Add-Member -NotePropertyName native_adapter_removed -NotePropertyValue $true
    $report | Add-Member -NotePropertyName native_unauthorized_source_drops -NotePropertyValue ([long]$sourceDrops[$sourceDrops.Count-1].Groups[1].Value)
    $report | Add-Member -NotePropertyName native_exit_code -NotePropertyValue $fixture.ExitCode
    $report | Add-Member -NotePropertyName test_firewall_rule_removed -NotePropertyValue $true
    if ($Lan) { $report | Add-Member -NotePropertyName test_udp_firewall_rule_removed -NotePropertyValue $true }
    $report | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $env:P2P_VPN_BROWSER_REPORT -Encoding UTF8
    if ($Lan) {
        Invoke-Checked -Executable node -Arguments @('crates/p2p-vpn-browser/tests/verify-lan-report.mjs', $metadataPath, $env:P2P_VPN_BROWSER_REPORT, $stdout, $stderr)
    } elseif (!$report.passed) { throw 'Browser report did not pass.' }
    $verdict = if ($Lan) { $metadata.scope } else { 'same-host' }
    $completed = $true
    Write-Host "Browser/TUN $verdict qualification passed. Evidence: $evidence"
} finally {
    if ($fixture -and !$fixture.HasExited) {
        try {
            $session = Invoke-RestMethod "http://127.0.0.1:$Port/session.json" -TimeoutSec 3
            $null = Invoke-WebRequest "http://127.0.0.1:$Port/shutdown" -Method Post -UseBasicParsing -Headers @{'X-Fixture-Token'=$session.shutdown_token} -TimeoutSec 3
            $null = $fixture.WaitForExit(15000)
        } catch {
            if (!$fixture.WaitForExit(15000)) { Write-Warning "Could not complete fixture cleanup: $($_.Exception.Message)" }
        }
    }
    if ($firewallRule) { Remove-NetFirewallRule -Name $firewallRule -ErrorAction Continue }
    if ($udpFirewallRule) { Remove-NetFirewallRule -Name $udpFirewallRule -ErrorAction Continue }
    if ($evidence -and $fixture) {
        $fixture.Refresh()
        $remainingRules = @($createdFirewallRules | Where-Object { Get-NetFirewallRule -Name $_ -ErrorAction SilentlyContinue })
        $cleanup = [ordered]@{
            host_validation_completed = $completed
            native_process_exited = $fixture.HasExited
            native_exit_code = $(if ($fixture.HasExited) { $fixture.ExitCode } else { $null })
            native_adapter_removed = !(Get-NetAdapter -Name "p2p-wasm-$($fixture.Id)" -ErrorAction SilentlyContinue)
            remaining_test_firewall_rules = $remainingRules
        }
        $cleanup | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $evidence 'cleanup.json') -Encoding UTF8
        if (!$cleanup.native_process_exited -or !$cleanup.native_adapter_removed -or $remainingRules.Count) { Write-Warning "Fixture cleanup is incomplete; see $evidence\cleanup.json" }
    }
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], 'Process') }
    Pop-Location
}
