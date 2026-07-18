#!/usr/bin/env python3
"""Focused FWD then REV with dual history diagnostics."""
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
import shutil

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "ohmycopy.exe"
PROBE = next((ROOT / "target" / "release").rglob("clip_probe.exe"))
REMOTE = r"C:\OhMyCopyE2E"
PORT = 3721
HOST = os.environ["OHMYCOPY_VM_HOST"]
USER = os.environ["OHMYCOPY_VM_USER"]
PASSWORD = os.environ["OHMYCOPY_VM_PASSWORD"]


def lan_ip(vm: str) -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((vm, 1))
        return s.getsockname()[0]
    finally:
        s.close()


def ps(c, script: str, timeout: int = 120) -> str:
    data = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    _, stdout, _ = c.exec_command(
        f"powershell -NoProfile -EncodedCommand {data}", timeout=timeout
    )
    out = stdout.read().decode("utf-8", "replace")
    print(out[-1800:])
    return out


def dump_hist(path: Path, label: str) -> list[str]:
    print(f"--- {label} ---")
    if not path.exists():
        print(" missing")
        return []
    con = sqlite3.connect(str(path))
    rows = [
        f"{a}|{b}"
        for a, b in con.execute(
            "SELECT preview, IFNULL(content,'') FROM history ORDER BY created_at DESC LIMIT 20"
        )
    ]
    con.close()
    for r in rows:
        print(" ", r[:120])
    return rows


def has_token(rows: list[str], token: str) -> bool:
    return any(token in r for r in rows)


def main() -> int:
    lip = lan_ip(HOST)
    host_id = str(uuid.uuid4())
    vm_id = str(uuid.uuid4())
    token = "FWD-" + uuid.uuid4().hex[:10]
    token2 = "REV-" + uuid.uuid4().hex[:10]
    print("lip", lip, "token", token, "token2", token2)

    def cfg(name: str, did: str) -> dict:
        return {
            "config_version": 2,
            "device_name": name,
            "device_id": did,
            "tcp_port": PORT,
            "udp_port": PORT,
            "password": "e2e-auto-test-pass",
            "max_payload_bytes": 209715200,
            "history_limit": 100,
            "discover_interval_secs": 3,
            "theme": "dark_glass",
            "auto_start": False,
            "sync_enabled": True,
            "console": True,
            "start_minimized_to_tray": False,
        }

    # Use same path as smoke to match environment
    lw = ROOT / "target" / "e2e_local"
    lw.mkdir(parents=True, exist_ok=True)
    for n in ("history.db", "history.db-wal", "history.db-shm"):
        p = lw / n
        if p.exists():
            p.unlink()
    (lw / "config.json").write_text(
        json.dumps(cfg("E2E-HOST", host_id), indent=2), encoding="utf-8"
    )
    (lw / "clients.json").write_text(
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
    shutil.copy2(EXE, lw / "ohmycopy.exe")
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
Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force
Start-Sleep 1
New-Item -ItemType Directory -Force -Path '{REMOTE}' | Out-Null
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
                            "addr": f"{lip}:{PORT}",
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
Get-Process ohmycopy | Format-List Id,SessionId
""",
    )
    lp = subprocess.Popen(
        [str(lw / "ohmycopy.exe"), "--headless"],
        cwd=str(lw),
        stdout=open(lw / "stdout.log", "w"),
        stderr=open(lw / "stderr.log", "w"),
    )
    print("handshake wait 12s")
    time.sleep(12)
    print("=== netstat ===")
    ps(c, "netstat -an | findstr 3721")

    print("=== FWD ===")
    subprocess.check_call([str(PROBE), "set-text", token])
    tmp = Path(tempfile.gettempdir()) / "vm_focus.db"
    fwd_ok = False
    for i in range(15):
        time.sleep(1)
        try:
            sftp = c.open_sftp()
            sftp.get(REMOTE + r"\history.db", str(tmp))
            sftp.close()
            rows = dump_hist(tmp, f"vm@{i}") if i % 5 == 0 else []
            if not rows:
                con = sqlite3.connect(str(tmp))
                rows = [str(r) for r in con.execute("SELECT preview FROM history")]
                con.close()
            if any(token in r for r in rows):
                print("FWD PASS at", i)
                fwd_ok = True
                break
        except Exception as e:
            print("sftp", e)
    print("FWD", fwd_ok)
    dump_hist(lw / "history.db", "local after FWD")

    time.sleep(2)
    print("=== REV ===")
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
netstat -an | findstr 3721
""",
    )

    rev_ok = False
    for i in range(20):
        time.sleep(1)
        local_rows = dump_hist(lw / "history.db", f"local@{i}") if i % 4 == 0 else []
        if not local_rows and (lw / "history.db").exists():
            con = sqlite3.connect(str(lw / "history.db"))
            local_rows = [
                f"{a}|{b}"
                for a, b in con.execute(
                    "SELECT preview, IFNULL(content,'') FROM history"
                )
            ]
            con.close()
        try:
            sftp = c.open_sftp()
            sftp.get(REMOTE + r"\history.db", str(tmp))
            sftp.close()
            con = sqlite3.connect(str(tmp))
            vm_rows = [
                f"{a}|{b}"
                for a, b in con.execute(
                    "SELECT preview, IFNULL(content,'') FROM history"
                )
            ]
            con.close()
        except Exception as e:
            print("vm hist", e)
            vm_rows = []
        local_hit = has_token(local_rows, token2)
        vm_hit = has_token(vm_rows, token2)
        if i % 4 == 0:
            print(f" t={i} local_hit={local_hit} vm_hit={vm_hit}")
            if i % 8 == 0:
                for r in vm_rows[:5]:
                    print("  vm:", r[:100])
        if local_hit:
            print("REV PASS at", i)
            rev_ok = True
            break
    if not rev_ok:
        print("REV FAIL")
        dump_hist(lw / "history.db", "local FINAL")
        try:
            sftp = c.open_sftp()
            sftp.get(REMOTE + r"\history.db", str(tmp))
            sftp.close()
            dump_hist(tmp, "vm FINAL")
        except Exception as e:
            print(e)

    lp.terminate()
    subprocess.run(["taskkill", "/F", "/IM", "ohmycopy.exe"], capture_output=True)
    try:
        ps(c, "Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force")
    except Exception:
        pass
    c.close()
    print("SUMMARY FWD", fwd_ok, "REV", rev_ok)
    return 0 if (fwd_ok and rev_ok) else 1


if __name__ == "__main__":
    raise SystemExit(main())
