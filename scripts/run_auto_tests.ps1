# OhMyCopy automated tests (local + optional remote VM peer).
#
# Usage:
#   # 1) Local unit + hub e2e (no VM needed)
#   powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
#
#   # 2) Larger file e2e (e.g. 20 MiB)
#   $env:OHMYCOPY_E2E_LARGE_MB = "20"
#   powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
#
#   # 3) Deploy headless peer to a Windows VM (PowerShell Remoting)
#   $env:OHMYCOPY_VM_HOST = "192.168.75.201"
#   $env:OHMYCOPY_VM_USER = "Administrator"
#   # optional: $env:OHMYCOPY_VM_PASSWORD = "..."
#   powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -RemoteVm
#
# Exit code 0 = all selected tests passed.

param(
    [switch]$RemoteVm,
    [switch]$SkipBuild,
    [switch]$Large
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Write-Step($msg) {
    Write-Host ""
    Write-Host "=== $msg ===" -ForegroundColor Cyan
}

if ($Large -and -not $env:OHMYCOPY_E2E_LARGE_MB) {
    $env:OHMYCOPY_E2E_LARGE_MB = "20"
}

Write-Step "cargo test (lib + integration)"
if (-not $SkipBuild) {
    cargo test --release --tests -- --nocapture
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} else {
    cargo test --release --tests -- --nocapture --exact 2>$null
    cargo test --release --test hub_pair_e2e -- --nocapture
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

Write-Step "local hub e2e (text / image / file / large / folder)"
cargo test --release --test hub_pair_e2e hub_pair_text_image_and_file -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test --release --test hub_pair_e2e hub_large_file_sync -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test --release --test hub_pair_e2e hub_folder_zip_sync -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test --release --test hub_pair_e2e hub_large_folder_zip_sync -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test --release --test hub_pair_e2e hub_auth_fail_wrong_password -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if (-not $RemoteVm) {
    Write-Host ""
    Write-Host "Local auto-tests PASSED." -ForegroundColor Green
    Write-Host "Tip: pass -RemoteVm with OHMYCOPY_VM_HOST / OHMYCOPY_VM_USER for VM deploy smoke test."
    exit 0
}

# ----- Optional: deploy headless binary to VM and open firewall -----
$VmHost = $env:OHMYCOPY_VM_HOST
$VmUser = $env:OHMYCOPY_VM_USER
if (-not $VmHost -or -not $VmUser) {
    Write-Error "RemoteVm requires env OHMYCOPY_VM_HOST and OHMYCOPY_VM_USER"
}

Write-Step "build release for deploy"
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Exe = Join-Path $Root "target\release\ohmycopy.exe"
if (-not (Test-Path $Exe)) { Write-Error "missing $Exe" }

$RemoteDir = "C:\OhMyCopyE2E"
Write-Step "deploy to $VmUser@$VmHost : $RemoteDir"

$cred = $null
if ($env:OHMYCOPY_VM_PASSWORD) {
    $sec = ConvertTo-SecureString $env:OHMYCOPY_VM_PASSWORD -AsPlainText -Force
    $cred = New-Object System.Management.Automation.PSCredential ($VmUser, $sec)
}

$sessionParams = @{
    ComputerName = $VmHost
}
if ($cred) { $sessionParams.Credential = $cred }

try {
    $session = New-PSSession @sessionParams
} catch {
    Write-Host "WinRM session failed: $_" -ForegroundColor Yellow
    Write-Host @"
请在虚拟机上开启 PowerShell 远程（管理员）:
  Enable-PSRemoting -Force
  # 若跨网段再放行防火墙 / TrustedHosts
  Set-Item WSMan:\localhost\Client\TrustedHosts -Value '$VmHost' -Force
本机也可:
  Set-Item WSMan:\localhost\Client\TrustedHosts -Value '$VmHost' -Force
"@
    exit 2
}

try {
    Invoke-Command -Session $session -ScriptBlock {
        param($dir)
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        # Stop previous e2e instance
        Get-Process ohmycopy -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    } -ArgumentList $RemoteDir

    Copy-Item -Path $Exe -Destination (Join-Path $RemoteDir "ohmycopy.exe") -ToSession $session -Force

    $localIp = (
        Get-NetIPAddress -AddressFamily IPv4 |
        Where-Object { $_.IPAddress -notlike "127.*" -and $_.PrefixOrigin -ne "WellKnown" } |
        Select-Object -First 1 -ExpandProperty IPAddress
    )
    if (-not $localIp) { $localIp = "127.0.0.1" }

    # Minimal config for headless peer
    $vmConfig = @{
        config_version          = 2
        device_name             = "E2E-VM"
        device_id               = [guid]::NewGuid().ToString()
        tcp_port                = 3721
        udp_port                = 3721
        password                = "e2e-auto-test-pass"
        max_payload_bytes       = 209715200
        history_limit           = 50
        discover_interval_secs  = 3
        theme                   = "dark_glass"
        auto_start              = $false
        sync_enabled            = $true
        console                 = $true
        start_minimized_to_tray = $false
    } | ConvertTo-Json

    $vmClients = @{
        version = 1
        clients = @(
            @{
                device_id    = $null
                name         = "E2E-HOST"
                addr         = "${localIp}:3721"
                auto_connect = $true
                last_seen    = 0
                source       = "manual"
            }
        )
    } | ConvertTo-Json -Depth 5

    Invoke-Command -Session $session -ScriptBlock {
        param($dir, $cfg, $clients)
        Set-Content -Path (Join-Path $dir "config.json") -Value $cfg -Encoding UTF8
        Set-Content -Path (Join-Path $dir "clients.json") -Value $clients -Encoding UTF8
        # Firewall for TCP/UDP 3721
        netsh advfirewall firewall delete rule name="OhMyCopyE2E" 2>$null | Out-Null
        netsh advfirewall firewall add rule name="OhMyCopyE2E" dir=in action=allow protocol=TCP localport=3721 | Out-Null
        netsh advfirewall firewall add rule name="OhMyCopyE2E-UDP" dir=in action=allow protocol=UDP localport=3721 | Out-Null
        # Start headless
        $exe = Join-Path $dir "ohmycopy.exe"
        Start-Process -FilePath $exe -ArgumentList "--headless" -WorkingDirectory $dir -WindowStyle Hidden
        Start-Sleep -Seconds 2
        $p = Get-Process ohmycopy -ErrorAction SilentlyContinue
        if (-not $p) { throw "ohmycopy did not start on VM" }
        "VM peer started pid=$($p.Id)"
    } -ArgumentList $RemoteDir, $vmConfig, $vmClients

    Write-Host "Remote headless peer started on $VmHost (password=e2e-auto-test-pass, port=3721)." -ForegroundColor Green
    Write-Host "本机可用 GUI 或 headless 连 $VmHost`:3721 做真实剪贴板验证。"
    Write-Host "Local IP suggested for VM clients.json: $localIp"
} finally {
    if ($session) { Remove-PSSession $session }
}

Write-Host ""
Write-Host "Auto-tests finished." -ForegroundColor Green
exit 0
