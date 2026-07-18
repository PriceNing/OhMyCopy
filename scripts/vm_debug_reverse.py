#!/usr/bin/env python3
"""Diagnose VM->local clipboard set via schtasks."""
from __future__ import annotations

import base64
import os
import time
import uuid
from pathlib import Path

import paramiko

HOST = os.environ.get("OHMYCOPY_VM_HOST", "192.168.75.201")
USER = os.environ.get("OHMYCOPY_VM_USER", "NRC")
PASSWORD = os.environ.get("OHMYCOPY_VM_PASSWORD", "")
REMOTE = r"C:\OhMyCopyE2E"


def ps(c: paramiko.SSHClient, script: str, timeout: int = 120) -> str:
    data = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    _, stdout, stderr = c.exec_command(
        f"powershell -NoProfile -EncodedCommand {data}", timeout=timeout
    )
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    print(f"CODE={code}")
    print(out)
    if err and "CLIXML" not in err:
        print("ERR:", err[:1000])
    return out


def main() -> int:
    if not PASSWORD:
        print("need OHMYCOPY_VM_PASSWORD")
        return 2
    token = "REVTEST-" + uuid.uuid4().hex[:10]
    print("TOKEN", token)
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    c.connect(
        HOST,
        username=USER,
        password=PASSWORD,
        timeout=20,
        allow_agent=False,
        look_for_keys=False,
    )
    rp = PASSWORD.replace('"', '""')

    # 1) bat wrapper for clip_probe
    script = f"""
$ErrorActionPreference='Continue'
$bat = @'
@echo off
C:\\OhMyCopyE2E\\clip_probe.exe set-text {token} > C:\\OhMyCopyE2E\\probe_out.txt 2>&1
C:\\OhMyCopyE2E\\clip_probe.exe get-text > C:\\OhMyCopyE2E\\probe_get.txt 2>&1
echo done >> C:\\OhMyCopyE2E\\probe_out.txt
'@
Set-Content -Path '{REMOTE}\\run_probe.bat' -Value $bat -Encoding ASCII

# 2) powershell Set-Clipboard bat
$bat2 = @'
@echo off
powershell -NoProfile -Command "Set-Clipboard -Value '{token}-ps'; Start-Sleep -Seconds 1; Get-Clipboard | Out-File -Encoding utf8 C:\\OhMyCopyE2E\\ps_clip.txt"
'@
Set-Content -Path '{REMOTE}\\run_psclip.bat' -Value $bat2 -Encoding ASCII

schtasks /Delete /TN OhMyCopyProbe /F 2>$null | Out-Null
$r = schtasks /Create /TN OhMyCopyProbe /TR "C:\\OhMyCopyE2E\\run_probe.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{USER}" /RP "{rp}" /IT
Write-Output "create_probe: $r last=$LASTEXITCODE"
schtasks /Run /TN OhMyCopyProbe
Write-Output "run_probe last=$LASTEXITCODE"
Start-Sleep -Seconds 5
Write-Output '--- probe_out ---'
if (Test-Path '{REMOTE}\\probe_out.txt') {{ Get-Content '{REMOTE}\\probe_out.txt' -Raw }} else {{ 'MISSING probe_out' }}
Write-Output '--- probe_get ---'
if (Test-Path '{REMOTE}\\probe_get.txt') {{ Get-Content '{REMOTE}\\probe_get.txt' -Raw }} else {{ 'MISSING probe_get' }}

schtasks /Delete /TN OhMyCopyPsClip /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyPsClip /TR "C:\\OhMyCopyE2E\\run_psclip.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{USER}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyPsClip | Out-Null
Start-Sleep -Seconds 5
Write-Output '--- ps_clip ---'
if (Test-Path '{REMOTE}\\ps_clip.txt') {{ Get-Content '{REMOTE}\\ps_clip.txt' -Raw }} else {{ 'MISSING ps_clip' }}

Write-Output '--- task status ---'
schtasks /Query /TN OhMyCopyProbe /V /FO LIST | Select-String -Pattern 'Last Run|Status|Result|Task To Run'
Write-Output '---'
schtasks /Query /TN OhMyCopyPsClip /V /FO LIST | Select-String -Pattern 'Last Run|Status|Result|Task To Run'
Write-Output '--- who / session ---'
query user 2>$null
Get-Process ohmycopy -EA SilentlyContinue | Format-List Id,SessionId,Path
"""
    ps(c, script)
    c.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
