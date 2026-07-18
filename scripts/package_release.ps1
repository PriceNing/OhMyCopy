# Package OhMyCopy portable release into dist/.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1 -SkipTests
#
# Output (example):
#   dist/OhMyCopy-0.1.35/
#     ohmycopy.exe
#     README.txt
#     config-example.json
#     clients-example.json
#   dist/OhMyCopy-0.1.35.zip

param(
    [switch]$SkipTests,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Write-Step($msg) {
    Write-Host ""
    Write-Host "=== $msg ===" -ForegroundColor Cyan
}

# Version from Cargo.toml
$cargoToml = Get-Content (Join-Path $Root "Cargo.toml") -Raw
if ($cargoToml -notmatch 'version\s*=\s*"([^"]+)"') {
    throw "cannot parse version from Cargo.toml"
}
$Version = $Matches[1]
$Name = "OhMyCopy-$Version"
$DistRoot = Join-Path $Root "dist"
$OutDir = Join-Path $DistRoot $Name
$ExeSrc = Join-Path $Root "target\release\ohmycopy.exe"

Write-Host "Packaging $Name" -ForegroundColor Green

if (-not $SkipTests) {
    Write-Step "tests (release)"
    cargo test --release --tests --lib -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw "tests failed" }
}

if (-not $SkipBuild) {
    Write-Step "cargo build --release"
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}

if (-not (Test-Path $ExeSrc)) {
    throw "missing $ExeSrc — run cargo build --release first"
}

Write-Step "stage dist\$Name"
if (Test-Path $OutDir) {
    Remove-Item $OutDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Copy-Item $ExeSrc (Join-Path $OutDir "ohmycopy.exe") -Force
Copy-Item (Join-Path $Root "docs\config-example.json") (Join-Path $OutDir "config-example.json") -Force
Copy-Item (Join-Path $Root "docs\clients-example.json") (Join-Path $OutDir "clients-example.json") -Force

$readme = @"
OhMyCopy $Version — 局域网剪贴板同步（便携版）

使用：
1. 将本目录拷到任意位置，双击 ohmycopy.exe
2. 两台电脑设置相同「共享密码」（勿使用空密码或 change-me）
3. 设备页连接对方 IP:3721，或等待发现后点连接
4. 配置自动写在 exe 同目录：config.json / clients.json / history.db / inbox/

防火墙：放行 TCP/UDP 3721
无界面：ohmycopy.exe --headless

更多说明见项目 README / docs/
"@
Set-Content -Path (Join-Path $OutDir "README.txt") -Value $readme -Encoding UTF8

# Flat copy for quick run (dist/ohmycopy.exe) — overwritten each package
Copy-Item (Join-Path $OutDir "ohmycopy.exe") (Join-Path $DistRoot "ohmycopy.exe") -Force

# Zip
Write-Step "zip"
$zipPath = Join-Path $DistRoot "$Name.zip"
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
Compress-Archive -Path $OutDir -DestinationPath $zipPath -Force

$exeInfo = Get-Item (Join-Path $OutDir "ohmycopy.exe")
Write-Host ""
Write-Host "Release ready:" -ForegroundColor Green
Write-Host "  folder : $OutDir"
Write-Host "  zip    : $zipPath"
Write-Host "  quick  : $(Join-Path $DistRoot 'ohmycopy.exe')"
Write-Host "  size   : $([math]::Round($exeInfo.Length / 1MB, 2)) MiB"
Write-Host ""
Write-Host "Note: dist/* is gitignored (except .gitkeep). Zip/exe are local artifacts only."
exit 0
