# Cross-host clipboard smoke: local machine <-> VM headless peer.
# Credentials via env (do not commit secrets):
#   OHMYCOPY_VM_HOST, OHMYCOPY_VM_USER, OHMYCOPY_VM_PASSWORD
# Optional: OHMYCOPY_TEST_PASSWORD (default e2e-auto-test-pass)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

$VmHost = $env:OHMYCOPY_VM_HOST
$VmUser = $env:OHMYCOPY_VM_USER
$VmPass = $env:OHMYCOPY_VM_PASSWORD
$TestPass = if ($env:OHMYCOPY_TEST_PASSWORD) { $env:OHMYCOPY_TEST_PASSWORD } else { "e2e-auto-test-pass" }
$Port = 3721
$RemoteDir = "C:\OhMyCopyE2E"
$LocalWork = Join-Path $Root "target\e2e_local"
$Token = "OHMYCOPY-E2E-" + [guid]::NewGuid().ToString("N").Substring(0, 12)

if (-not $VmHost -or -not $VmUser -or -not $VmPass) {
    Write-Error "Set OHMYCOPY_VM_HOST, OHMYCOPY_VM_USER, OHMYCOPY_VM_PASSWORD"
}

function Step($m) { Write-Host "`n=== $m ===" -ForegroundColor Cyan }

function Get-LanIPv4 {
    $ips = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object {
            $_.IPAddress -notlike "127.*" -and
            $_.IPAddress -notlike "169.254.*" -and
            $_.PrefixOrigin -ne "WellKnown"
        }
    # Prefer same /24 as VM if possible
    $vmPrefix = ($VmHost -split '\.')[0..2] -join '.'
    $same = $ips | Where-Object { $_.IPAddress.StartsWith($vmPrefix + ".") } | Select-Object -First 1
    if ($same) { return $same.IPAddress }
    if ($ips) { return ($ips | Select-Object -First 1).IPAddress }
    return $null
}

Step "build release + clip_probe"
cargo build --release --examples
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$Exe = Join-Path $Root "target\release\ohmycopy.exe"
$Probe = Join-Path $Root "target\release\examples\clip_probe.exe"
if (-not (Test-Path $Probe)) {
    # cargo may put example next to release
    $Probe = Join-Path $Root "target\release\clip_probe.exe"
}
if (-not (Test-Path $Probe)) {
    Get-ChildItem (Join-Path $Root "target\release") -Recurse -Filter "clip_probe.exe" | Select-Object -First 1 | ForEach-Object { $Probe = $_.FullName }
}
if (-not (Test-Path $Exe)) { Write-Error "missing exe" }
if (-not (Test-Path $Probe)) { Write-Error "missing clip_probe.exe" }

New-Item -ItemType Directory -Force -Path $LocalWork | Out-Null
Get-Process ohmycopy -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

$LocalIp = Get-LanIPv4
if (-not $LocalIp) { Write-Error "cannot detect LAN IP" }
Write-Host "Local LAN IP: $LocalIp  VM: $VmHost"

$sec = ConvertTo-SecureString $VmPass -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential ($VmUser, $sec)

# TrustedHosts
try {
    $th = (Get-Item WSMan:\localhost\Client\TrustedHosts -ErrorAction SilentlyContinue).Value
    if (-not $th -or $th -notlike "*$VmHost*") {
        Set-Item WSMan:\localhost\Client\TrustedHosts -Value $VmHost -Force -ErrorAction SilentlyContinue
    }
} catch {}

Step "WinRM session to $VmHost"
try {
    $session = New-PSSession -ComputerName $VmHost -Credential $cred -ErrorAction Stop
} catch {
    Write-Host "WinRM failed: $_" -ForegroundColor Red
    Write-Host "On VM (admin): Enable-PSRemoting -Force"
    exit 2
}

$results = @()

try {
    # --- Prepare VM ---
    Step "deploy headless to VM"
    Invoke-Command -Session $session -ScriptBlock {
        param($dir)
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        Get-Process ohmycopy -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 1
    } -ArgumentList $RemoteDir

    Copy-Item -Path $Exe -Destination "$RemoteDir\ohmycopy.exe" -ToSession $session -Force

    $vmDeviceId = [guid]::NewGuid().ToString()
    $hostDeviceId = [guid]::NewGuid().ToString()

    $vmConfig = @"
{
  "config_version": 2,
  "device_name": "E2E-VM",
  "device_id": "$vmDeviceId",
  "tcp_port": $Port,
  "udp_port": $Port,
  "password": "$TestPass",
  "max_payload_bytes": 209715200,
  "history_limit": 100,
  "discover_interval_secs": 3,
  "theme": "dark_glass",
  "auto_start": false,
  "sync_enabled": true,
  "console": true,
  "start_minimized_to_tray": false
}
"@

    $vmClients = @"
{
  "version": 1,
  "clients": [
    {
      "device_id": "$hostDeviceId",
      "name": "E2E-HOST",
      "addr": "${LocalIp}:${Port}",
      "auto_connect": true,
      "last_seen": 0,
      "source": "manual",
      "ignored": false
    }
  ]
}
"@

    Invoke-Command -Session $session -ScriptBlock {
        param($dir, $cfg, $clients)
        Set-Content -Path (Join-Path $dir "config.json") -Value $cfg -Encoding UTF8
        Set-Content -Path (Join-Path $dir "clients.json") -Value $clients -Encoding UTF8
        netsh advfirewall firewall delete rule name="OhMyCopyE2E" 2>$null | Out-Null
        netsh advfirewall firewall delete rule name="OhMyCopyE2E-UDP" 2>$null | Out-Null
        netsh advfirewall firewall add rule name="OhMyCopyE2E" dir=in action=allow protocol=TCP localport=3721 | Out-Null
        netsh advfirewall firewall add rule name="OhMyCopyE2E-UDP" dir=in action=allow protocol=UDP localport=3721 | Out-Null
        $exe = Join-Path $dir "ohmycopy.exe"
        $p = Start-Process -FilePath $exe -ArgumentList "--headless" -WorkingDirectory $dir -PassThru -WindowStyle Hidden
        Start-Sleep -Seconds 2
        if (-not (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) { throw "VM ohmycopy failed to start" }
        "VM started pid=$($p.Id)"
    } -ArgumentList $RemoteDir, $vmConfig, $vmClients

    # --- Prepare local headless ---
    Step "start local headless peer"
    $localConfig = @"
{
  "config_version": 2,
  "device_name": "E2E-HOST",
  "device_id": "$hostDeviceId",
  "tcp_port": $Port,
  "udp_port": $Port,
  "password": "$TestPass",
  "max_payload_bytes": 209715200,
  "history_limit": 100,
  "discover_interval_secs": 3,
  "theme": "dark_glass",
  "auto_start": false,
  "sync_enabled": true,
  "console": true,
  "start_minimized_to_tray": false
}
"@
    $localClients = @"
{
  "version": 1,
  "clients": [
    {
      "device_id": "$vmDeviceId",
      "name": "E2E-VM",
      "addr": "${VmHost}:${Port}",
      "auto_connect": true,
      "last_seen": 0,
      "source": "manual",
      "ignored": false
    }
  ]
}
"@
    Set-Content -Path (Join-Path $LocalWork "config.json") -Value $localConfig -Encoding UTF8
    Set-Content -Path (Join-Path $LocalWork "clients.json") -Value $localClients -Encoding UTF8
    Copy-Item $Exe (Join-Path $LocalWork "ohmycopy.exe") -Force

    $localProc = Start-Process -FilePath (Join-Path $LocalWork "ohmycopy.exe") -ArgumentList "--headless" -WorkingDirectory $LocalWork -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 5
    if (-not (Get-Process -Id $localProc.Id -ErrorAction SilentlyContinue)) {
        throw "local ohmycopy failed to start"
    }
    Write-Host "Local headless pid=$($localProc.Id); wait for pair..."
    Start-Sleep -Seconds 8

    # --- TEST 1: text local -> VM ---
    Step "TEST text: local clipboard -> VM"
    & $Probe set-text $Token
    if ($LASTEXITCODE -ne 0) { throw "clip_probe set-text failed" }
    Start-Sleep -Seconds 5
    $remoteText = Invoke-Command -Session $session -ScriptBlock {
        try { Get-Clipboard -Raw -ErrorAction Stop } catch { "" }
    }
    if ($remoteText -and $remoteText.Contains($Token)) {
        Write-Host "PASS text local->VM" -ForegroundColor Green
        $results += "PASS text local->VM"
    } else {
        Write-Host "FAIL text local->VM (remote got: '$remoteText')" -ForegroundColor Red
        $results += "FAIL text local->VM"
    }

    # --- TEST 2: text VM -> local ---
    Step "TEST text: VM clipboard -> local"
    $Token2 = "OHMYCOPY-E2E-REV-" + [guid]::NewGuid().ToString("N").Substring(0, 8)
    Invoke-Command -Session $session -ScriptBlock {
        param($t)
        Set-Clipboard -Value $t
    } -ArgumentList $Token2
    Start-Sleep -Seconds 5
    $localText = & $Probe get-text
    if ($localText -and $localText.Contains($Token2)) {
        Write-Host "PASS text VM->local" -ForegroundColor Green
        $results += "PASS text VM->local"
    } else {
        Write-Host "FAIL text VM->local (local got: '$localText')" -ForegroundColor Red
        $results += "FAIL text VM->local"
    }

    # --- TEST 3: image local -> VM (check inbox growth / file on VM) ---
    Step "TEST image: local PNG clipboard -> VM inbox"
    $png = Join-Path $LocalWork "probe.png"
    & $Probe make-png $png
    & $Probe set-image-png $png
    Start-Sleep -Seconds 6
    $vmInboxCount = Invoke-Command -Session $session -ScriptBlock {
        param($dir)
        $inbox = Join-Path $dir "inbox"
        if (-not (Test-Path $inbox)) { return 0 }
        @(Get-ChildItem $inbox -Recurse -File -Filter "*.png" -ErrorAction SilentlyContinue).Count
    } -ArgumentList $RemoteDir
    if ($vmInboxCount -ge 1) {
        Write-Host "PASS image local->VM (png files in VM inbox: $vmInboxCount)" -ForegroundColor Green
        $results += "PASS image local->VM"
    } else {
        # Fallback: kind on remote hard; inbox is best signal for headless
        Write-Host "FAIL image local->VM (no png in VM inbox)" -ForegroundColor Red
        $results += "FAIL image local->VM"
    }

    # --- TEST 4: file local -> VM ---
    Step "TEST file: local file clipboard -> VM inbox"
    $bin = Join-Path $LocalWork "probe-bin.dat"
    $payload = New-Object byte[] (256 * 1024)
    (New-Object Random).NextBytes($payload)
    [IO.File]::WriteAllBytes($bin, $payload)
    & $Probe set-file $bin
    Start-Sleep -Seconds 8
    $vmDat = Invoke-Command -Session $session -ScriptBlock {
        param($dir)
        $inbox = Join-Path $dir "inbox"
        if (-not (Test-Path $inbox)) { return $false }
        $hit = Get-ChildItem $inbox -Recurse -File -Filter "probe-bin.dat" -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($hit -and $hit.Length -eq 262144) { return $true }
        # any new large-ish file
        $any = Get-ChildItem $inbox -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Length -eq 262144 } |
            Select-Object -First 1
        return [bool]$any
    } -ArgumentList $RemoteDir
    if ($vmDat) {
        Write-Host "PASS file local->VM" -ForegroundColor Green
        $results += "PASS file local->VM"
    } else {
        Write-Host "FAIL file local->VM" -ForegroundColor Red
        $results += "FAIL file local->VM"
    }

} finally {
    Step "cleanup"
    Get-Process ohmycopy -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    if ($session) {
        Invoke-Command -Session $session -ScriptBlock {
            Get-Process ohmycopy -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        } -ErrorAction SilentlyContinue
        Remove-PSSession $session -ErrorAction SilentlyContinue
    }
}

Write-Host "`n======== SUMMARY ========" -ForegroundColor Cyan
$fail = 0
foreach ($r in $results) {
    if ($r -like "FAIL*") {
        Write-Host $r -ForegroundColor Red
        $fail++
    } else {
        Write-Host $r -ForegroundColor Green
    }
}
if ($fail -gt 0) {
    Write-Host "`n$fail test(s) FAILED" -ForegroundColor Red
    exit 1
}
Write-Host "`nAll VM smoke tests PASSED" -ForegroundColor Green
exit 0
