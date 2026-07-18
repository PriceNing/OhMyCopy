#!/usr/bin/env python3
"""Reproduce: local->VM then VM->local (smoke order)."""
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
    _, stdout, _ = c.exec_command(
        f"powershell -NoProfile -EncodedCommand {data}", timeout=timeout
    )
    out = stdout.read().decode("utf-8", "replace")
    print(out[-2000:])
    return out


def history_has(path: Path, needle: str) -> bool:
    if not path.exists():
        return False
    try:
        con = sqlite3.connect(str(path))
        for prev, content in con.execute(
            "SELECT preview, IFNULL(content,'') FROM history ORDER BY created_at DESC LIMIT 50"
        ):
            if needle in f"{prev}\n{content}":
                con.close()
                return True
        con.close()
    except sqlite3.Error as e:
        print("sqlite", e)
    return False


def history_dump(path: Path, label: str):
    print(f"--- {label} ---")
    if not path.exists():
        print(" missing")
        return
    con = sqlite3.connect(str(path))
    for r in con.execute(
        "SELECT preview, substr(IFNULL(content,''),1,50) FROM history ORDER BY created_at DESC LIMIT 10"
    ):
        print(" ", r)
    con.close()


def main():
    local_ip = lan_ip(HOST)
    host_id = str(uuid.uuid4())
    vm_id = str(uuid.uuid4())
    token = "FWD-" + uuid.uuid4().hex[:10]
    token2 = "REV-" + uuid.uuid4().hex[:10]
    print("token", token, "token2", token2)

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

    local_work = ROOT / "target" / "e2e_rev3"
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
                        "ignored": False,
                        "last_seen": 0,
                        "source": "manual",
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
        HOST, username=USER, password=PASSWORD, timeout=20, allow_agent=False, look_for_keys=False
    )
    rp = PASSWORD.replace('"', '""')
    ps(
        c,
        rf"""
Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force
Start-Sleep 1
New-Item -ItemType Directory -Force -Path '{REMOTE}' | Out-Null
Remove-Item -Force (Join-Path '{REMOTE}' 'history.db*') -EA SilentlyContinue
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
                            "ignored": False,
                            "last_seen": 0,
                            "source": "manual",
                        }
                    ],
                },
                indent=2,
            )
        )
    sftp.close()

    ps(
        c,
        rf"""
schtasks /Delete /TN OhMyCopyE2E /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyE2E /TR "C:\OhMyCopyE2E\ohmycopy.exe --headless" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{USER}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyE2E | Out-Null
Start-Sleep 4
""",
    )
    local_proc = subprocess.Popen(
        [str(local_work / "ohmycopy.exe"), "--headless"],
        cwd=str(local_work),
        stdout=open(local_work / "stdout.log", "w"),
        stderr=open(local_work / "stderr.log", "w"),
    )
    print("handshake wait 12s")
    time.sleep(12)

    print("=== FWD local->VM ===")
    subprocess.check_call([str(PROBE), "set-text", token])
    ok = False
    for i in range(15):
        time.sleep(1)
        tmp = Path(tempfile.gettempdir()) / "vm_h3.db"
        try:
            sftp = c.open_sftp()
            sftp.get(REMOTE + r"\history.db", str(tmp))
            sftp.close()
            if history_has(tmp, token):
                ok = True
                print(f"FWD ok at {i}s")
                break
        except Exception as e:
            print("sftp", e)
    print("FWD", "PASS" if ok else "FAIL")
    history_dump(local_work / "history.db", "local after FWD")

    print("=== REV VM->local ===")
    # extra settle after FWD
    time.sleep(2)
    ps(
        c,
        rf"""
$bat = @'
@echo off
C:\OhMyCopyE2E\clip_probe.exe set-text {token2} > C:\OhMyCopyE2E\probe_out.txt 2>&1
C:\OhMyCopyE2E\clip_probe.exe get-text > C:\OhMyCopyE2E\probe_get.txt 2>&1
'@
Set-Content C:\OhMyCopyE2E\run_probe.bat $bat -Encoding ASCII
schtasks /Delete /TN OhMyCopyProbe /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyProbe /TR "C:\OhMyCopyE2E\run_probe.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{USER}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyProbe | Out-Null
Start-Sleep 4
Get-Content C:\OhMyCopyE2E\probe_out.txt,C:\OhMyCopyE2E\probe_get.txt
""",
    )
    ok2 = False
    for i in range(20):
        time.sleep(1)
        if history_has(local_work / "history.db", token2):
            ok2 = True
            print(f"REV ok at {i}s")
            break
    print("REV", "PASS" if ok2 else "FAIL")
    history_dump(local_work / "history.db", "local after REV")
    try:
        sftp = c.open_sftp()
        sftp.get(REMOTE + r"\history.db", str(tmp))
        sftp.close()
        history_dump(tmp, "vm after REV")
    except Exception as e:
        print(e)

    local_proc.terminate()
    subprocess.run(["taskkill", "/F", "/IM", "ohmycopy.exe"], capture_output=True)
    try:
        ps(c, "Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force")
    except Exception:
        pass
    c.close()


if __name__ == "__main__":
    main()
