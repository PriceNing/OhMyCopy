# OhMyCopy full automated test suite.
#
# Always runs ALL local tests (lib + hub: text/image/file/folder/large/ignore/unpair/...).
# When VM env is set (or -VmSmoke), also runs real host↔VM clipboard smoke.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
#
#   $env:OHMYCOPY_E2E_LARGE_MB = "20"
#   powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
#
#   $env:OHMYCOPY_VM_HOST = "192.168.1.100"   # test machine IP
#   $env:OHMYCOPY_VM_USER = "your-user"
#   $env:OHMYCOPY_VM_PASSWORD = "..."         # do not commit
#   powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -VmSmoke
#
# Exit 0 = all selected tests passed.

param(
    [switch]$VmSmoke,
    [switch]$RemoteVm,   # alias of -VmSmoke
    [switch]$Large,
    [switch]$RequireVm
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Write-Step($msg) {
    Write-Host ""
    Write-Host "=== $msg ===" -ForegroundColor Cyan
}

function Invoke-Cargo {
    # Prefer array splat so "--" is preserved for test binary args.
    param([Parameter(Mandatory = $true)][string[]]$Args)
    Write-Host ("> cargo " + ($Args -join " ")) -ForegroundColor DarkGray
    & cargo.exe @Args
    if ($LASTEXITCODE -ne 0) {
        throw "cargo failed (exit $LASTEXITCODE): cargo $($Args -join ' ')"
    }
}

if ($Large -and -not $env:OHMYCOPY_E2E_LARGE_MB) {
    $env:OHMYCOPY_E2E_LARGE_MB = "20"
}
if (-not $env:OHMYCOPY_E2E_LARGE_MB) {
    $env:OHMYCOPY_E2E_LARGE_MB = "8"
}

$wantVm = $VmSmoke -or $RemoteVm
$hasVmEnv = $env:OHMYCOPY_VM_HOST -and $env:OHMYCOPY_VM_USER -and $env:OHMYCOPY_VM_PASSWORD
if (-not $wantVm -and $hasVmEnv) {
    $wantVm = $true
    Write-Host "VM env detected — will run host↔VM smoke after local tests." -ForegroundColor DarkCyan
}
if ($RequireVm -and -not $hasVmEnv) {
    Write-Error "RequireVm set but OHMYCOPY_VM_HOST / USER / PASSWORD incomplete."
}

Write-Host @"

OhMyCopy FULL test matrix
  Local (always):
    - unit: auth / engine / config / clients / history / inbox / discover
    - hub: text+image+file, large file ($($env:OHMYCOPY_E2E_LARGE_MB) MiB), folder, large folder
           reverse, ignore, unpair, auth fail, oversize
    - protocol / settings_and_clients / e2e_sync
  VM OS clipboard (env or -VmSmoke):
    - text L→VM / VM→L, image, file, large file, folder HDROP
"@ -ForegroundColor Gray

# ----- 1) Full local suite -----
Write-Step "cargo test --release --tests --lib  (ALL local cases)"
Invoke-Cargo -Args @("test", "--release", "--tests", "--lib", "--", "--nocapture")

# ----- 2) Explicit hub matrix (named filters for clear logs) -----
Write-Step "hub matrix (text / image / file)"
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_pair_text_image_and_file", "--", "--nocapture")

Write-Step "hub matrix (large file $($env:OHMYCOPY_E2E_LARGE_MB) MiB)"
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_large_file_sync", "--", "--nocapture")

Write-Step "hub matrix (folder + large folder)"
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_folder_zip_sync", "--", "--nocapture")
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_large_folder_zip_sync", "--", "--nocapture")

Write-Step "hub matrix (reverse / ignore / unpair / auth / oversize)"
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_reverse_b_to_a", "--", "--nocapture")
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_ignore_mutes_clipboard", "--", "--nocapture")
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_unpair_notifies_peer", "--", "--nocapture")
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_auth_fail_wrong_password", "--", "--nocapture")
Invoke-Cargo -Args @("test", "--release", "--test", "hub_pair_e2e", "hub_oversize_not_emitted", "--", "--nocapture")

Write-Host ""
Write-Host "Local FULL suite PASSED (text/image/file/folder/large + unit)." -ForegroundColor Green

# ----- 3) VM real clipboard -----
if (-not $wantVm) {
    Write-Host "VM smoke SKIPPED (set OHMYCOPY_VM_* or pass -VmSmoke)." -ForegroundColor Yellow
    exit 0
}
if (-not $hasVmEnv) {
    Write-Error "VM smoke requires OHMYCOPY_VM_HOST, OHMYCOPY_VM_USER, OHMYCOPY_VM_PASSWORD"
}

Write-Step "VM clipboard smoke (SSH + schtasks /IT)  host=$($env:OHMYCOPY_VM_HOST)"
$smoke = Join-Path $Root "scripts\vm_ssh_smoke.py"
if (-not (Test-Path $smoke)) {
    Write-Error "missing $smoke"
}

Invoke-Cargo -Args @("build", "--release", "--examples")

python $smoke
if ($LASTEXITCODE -ne 0) {
    Write-Host "VM smoke FAILED (exit $LASTEXITCODE)" -ForegroundColor Red
    exit $LASTEXITCODE
}

Write-Host ""
Write-Host "ALL tests PASSED (local full + VM smoke)." -ForegroundColor Green
exit 0
