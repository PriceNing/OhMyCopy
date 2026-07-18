#!/usr/bin/env python3
"""Full reverse path debug: start peers, set VM clip, check both histories + connection."""
from __future__ import annotations

import base64
import json
import os
import socket
import sqlite3
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

import paramiko

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "ohmycopy.exe"
PROBE = next((ROOT / "target" / "release").rglob("clip_probe.exe"))
REMOTE = r"C:\OhMyCopyE2E"
PORT = 3721
HOST = os.environ["OHMYCOPY_VM_HOST"]
USER = os.environ["OHMYCOPY_VM_USER"]
PASSWORD = os.environ["OHMYCOPY_VM_PASSWORD"]
TEST_PASS = "e2e-auto-test-pass"


def lan_ip(vm: str) -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((vm, 1))
        return s.getsockname()[0]
    finally:
        s.close()


def ps(c, script, timeout=120):
    data = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    _, stdout, stderr = c.exec_command(
        f"powershell -NoProfile -EncodedCommand {data}", timeout=timeout
    )
    out = stdout.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    print(out[-2500:])
    print("ps_code", code)
    return out


def history_dump(path: Path):
    if not path.exists():
        print(f"  no history at {path}")
        return
    con = sqlite3.connect(str(path))
    rows = con.execute(
        "SELECT preview, substr(IFNULL(content,''),1,60) FROM history ORDER BY created_at DESC LIMIT 15"
    ).fetchall()
    con.close()
    for r in rows:
        print(" ", r)


def main():
    local_ip = lan_ip(HOST)
    host_id = str(uuid.uuid4())
    vm_id = str(uuid.uuid4())
    token = "REV2-" + uuid.uuid4().hex[:12]
    print("token", token, "local", local_ip)

    def cfg(name, did):
        return {
            "config_version": 2,
            "device_name": name,
            "device_id": did,
            "tcp_port": PORT,
            "udp_port": PORT,
            "password": TEST_PASS,
            "max_payload_bytes": 209715200,
            "history_limit": 100,
            "discover_interval_secs": 3,
            "theme": "dark_glass",
            "auto_start": False,
            "sync_enabled": True,
            "console": True,
            "start_minimized_to_tray": False,
        }

    local_work = ROOT / "target" / "e2e_rev2"
    local_work.mkdir(parents=True, exist_ok=True)
    for n in ("history.db", "history.db-wal", "history.db-shm"):
        p = local_work / n
        if p.exists():
            p.unlink()
    (local_work / "config.json").write_text(
        json.dumps(cfg("E2E-HOST", host_id), indent=2), encoding="utf-8"
    )
    (local_work / "clients.json").write_text(
        json.dumps(
            {
                "version": 1,
                "clients": [
                    {
                        "device_id": vm_id,
                        "name": "E2E-VM",
                        "addr": f"{HOST}:{PORT}",
                        "auto_connect": True,
                        "last_seen": 0,
                        "source": "manual",
                        "ignored": False,
                    }
                ],
            },
            indent=2,
        ),
        encoding="utf-8",
    )
    import shutil

    shutil.copy2(EXE, local_work / "ohmycopy.exe")
    subprocess.run(["taskkill", "/F", "/IM", "ohmycopy.exe"], capture_output=True)

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

    ps(
        c,
        rf"""
$ErrorActionPreference='SilentlyContinue'
New-Item -ItemType Directory -Force -Path '{REMOTE}' | Out-Null
Get-Process ohmycopy | Stop-Process -Force
Start-Sleep 1
Remove-Item -Force (Join-Path '{REMOTE}' 'history.db*') -EA SilentlyContinue
Remove-Item -Recurse -Force (Join-Path '{REMOTE}' 'inbox') -EA SilentlyContinue
""",
    )
    sftp = c.open_sftp()
    sftp.put(str(EXE), REMOTE + r"\ohmycopy.exe")
    sftp.put(str(PROBE), REMOTE + r"\clip_probe.exe")
    with sftp.file(REMOTE + r"\config.json", "w") as f:
        f.write(json.dumps(cfg("E2E-VM", vm_id), indent=2))
    with sftp.file(REMOTE + r"\clients.json", "w") as f:
        f.write(
            json.dumps(
                {
                    "version": 1,
                    "clients": [
                        {
                            "device_id": host_id,
                            "name": "E2E-HOST",
                            "addr": f"{local_ip}:{PORT}",
                            "auto_connect": True,
                            "last_seen": 0,
                            "source": "manual",
                            "ignored": False,
                        }
                    ],
                },
                indent=2,
            )
        )
    sftp.close()

    # Start BOTH with HIGHEST /IT
    print("=== start both ===")
    ps(
        c,
        rf"""
$ErrorActionPreference='Continue'
netsh advfirewall firewall add rule name='OhMyCopyE2E' dir=in action=allow protocol=TCP localport=3721 profile=any enable=yes | Out-Null
schtasks /Delete /TN OhMyCopyE2E /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyE2E /TR "C:\OhMyCopyE2E\ohmycopy.exe --headless" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{USER}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyE2E | Out-Null
Start-Sleep 4
Get-Process ohmycopy | Format-List Id,SessionId
""",
    )

    local_proc = subprocess.Popen(
        [str(local_work / "ohmycopy.exe"), "--headless"],
        cwd=str(local_work),
        stdout=open(local_work / "stdout.log", "w"),
        stderr=open(local_work / "stderr.log", "w"),
    )
    time.sleep(14)

    print("=== netstat VM ===")
    ps(c, "netstat -an | findstr 3721")

    # Reverse only: set on VM
    print("=== set reverse on VM ===")
    ps(
        c,
        rf"""
$bat = @'
@echo off
C:\OhMyCopyE2E\clip_probe.exe set-text {token} > C:\OhMyCopyE2E\probe_out.txt 2>&1
C:\OhMyCopyE2E\clip_probe.exe get-text > C:\OhMyCopyE2E\probe_get.txt 2>&1
'@
Set-Content C:\OhMyCopyE2E\run_probe.bat $bat -Encoding ASCII
schtasks /Delete /TN OhMyCopyProbe /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyProbe /TR "C:\OhMyCopyE2E\run_probe.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{USER}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyProbe | Out-Null
Start-Sleep 3
Get-Content C:\OhMyCopyE2E\probe_out.txt,C:\OhMyCopyE2E\probe_get.txt
""",
    )

    time.sleep(8)
    print("=== local history ===")
    history_dump(local_work / "history.db")
    print("=== pull VM history ===")
    tmp = Path(tempfile.gettempdir()) / "vm_h_rev2.db"
    try:
        sftp = c.open_sftp()
        sftp.get(REMOTE + r"\history.db", str(tmp))
        sftp.close()
        history_dump(tmp)
    except Exception as e:
        print("get vm history failed", e)

    # Also try Set-Clipboard PowerShell
    token3 = "REV3-" + uuid.uuid4().hex[:8]
    print("=== Set-Clipboard", token3)
    ps(
        c,
        rf"""
$bat = @'
@echo off
powershell -NoProfile -Command "Set-Clipboard -Value '{token3}'"
'@
Set-Content C:\OhMyCopyE2E\run_ps.bat $bat -Encoding ASCII
schtasks /Delete /TN OhMyCopyPs /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyPs /TR "C:\OhMyCopyE2E\run_ps.bat" /SC ONCE /ST 00:00 /RL LIMITED /F /RU "{USER}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyPs | Out-Null
Start-Sleep 3
""",
    )
    time.sleep(8)
    print("=== local history after PS clip ===")
    history_dump(local_work / "history.db")
    try:
        sftp = c.open_sftp()
        sftp.get(REMOTE + r"\history.db", str(tmp))
        sftp.close()
        print("=== VM history after PS clip ===")
        history_dump(tmp)
    except Exception as e:
        print(e)

    # local stdout
    print("=== local logs ===")
    for n in ("stdout.log", "stderr.log"):
        p = local_work / n
        if p.exists():
            t = p.read_text(errors="replace")
            print(n, len(t), t[-1500:])

    local_proc.terminate()
    subprocess.run(["taskkill", "/F", "/IM", "ohmycopy.exe"], capture_output=True)
    ps(c, "Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force", check=False if False else True)
    c.close()


if __name__ == "__main__":
    main()
