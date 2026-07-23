#!/usr/bin/env python3
"""Deploy OhMyCopy to VM over SSH and verify sync via history.db / inbox.

Uses SSH (port 22). Does NOT rely on remote Get-Clipboard (SSH session clipboard
is isolated from the desktop/headless process session on Windows).

Env:
  OHMYCOPY_VM_HOST, OHMYCOPY_VM_USER, OHMYCOPY_VM_PASSWORD
  OHMYCOPY_TEST_PASSWORD (default e2e-auto-test-pass)
"""

from __future__ import annotations

import json
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

import paramiko

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "ohmycopy.exe"
PROBE_CANDIDATES = [
    ROOT / "target" / "release" / "examples" / "clip_probe.exe",
    ROOT / "target" / "release" / "clip_probe.exe",
]
REMOTE_DIR = r"C:\OhMyCopyE2E"
# The smoke passes this directory via OHMYCOPY_DATA_DIR, avoiding dependence on
# the profile environment inherited by Task Scheduler.
DATA_DIR_NAME = ".ohmycopy"
PORT = 3721


def find_probe() -> Path:
    for p in PROBE_CANDIDATES:
        if p.exists():
            return p
    found = list((ROOT / "target" / "release").rglob("clip_probe.exe"))
    if found:
        return found[0]
    raise FileNotFoundError("clip_probe.exe")


def lan_ip(vm_host: str) -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((vm_host, 1))
        return s.getsockname()[0]
    finally:
        s.close()


def ssh_connect(host: str, user: str, password: str) -> paramiko.SSHClient:
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    c.connect(
        host,
        username=user,
        password=password,
        timeout=20,
        allow_agent=False,
        look_for_keys=False,
        banner_timeout=30,
    )
    return c


def ps(c: paramiko.SSHClient, script: str, check: bool = True) -> str:
    import base64

    data = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    cmd = f"powershell -NoProfile -EncodedCommand {data}"
    _, stdout, stderr = c.exec_command(cmd, timeout=180)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    if check and code != 0:
        raise RuntimeError(f"ps exit {code}\n{out}\n{err}\n{script[:500]}")
    return out


def sftp_put(c: paramiko.SSHClient, local: Path, remote: str) -> None:
    sftp = c.open_sftp()
    try:
        # mkdir parents
        remote = remote.replace("/", "\\")
        bits = remote.split("\\")
        cur = bits[0] + "\\"
        for part in bits[1:-1]:
            if not part:
                continue
            cur = cur.rstrip("\\") + "\\" + part
            try:
                sftp.stat(cur)
            except FileNotFoundError:
                try:
                    sftp.mkdir(cur)
                except OSError:
                    pass
        sftp.put(str(local), remote)
    finally:
        sftp.close()


def sftp_get(c: paramiko.SSHClient, remote: str, local: Path) -> bool:
    sftp = c.open_sftp()
    try:
        sftp.get(remote, str(local))
        return True
    except FileNotFoundError:
        return False
    except OSError:
        return False
    finally:
        sftp.close()


def history_has(path: Path, needle: str) -> bool:
    if not path.exists():
        return False
    try:
        con = sqlite3.connect(str(path))
        cur = con.execute(
            "SELECT preview, IFNULL(content,'') FROM history ORDER BY created_at DESC LIMIT 50"
        )
        for prev, content in cur.fetchall():
            blob = f"{prev}\n{content}"
            if needle in blob:
                con.close()
                return True
        con.close()
    except sqlite3.Error as e:
        print(f"  sqlite error: {e}")
    return False


def tcp_open(host: str, port: int, timeout: float = 3.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def main() -> int:
    host = os.environ.get("OHMYCOPY_VM_HOST", "192.168.75.201")
    user = os.environ.get("OHMYCOPY_VM_USER", "NRC")
    password = os.environ.get("OHMYCOPY_VM_PASSWORD", "")
    test_pass = os.environ.get("OHMYCOPY_TEST_PASSWORD", "e2e-auto-test-pass")
    if not password:
        print("OHMYCOPY_VM_PASSWORD required", file=sys.stderr)
        return 2

    print("=== build ===")
    # Build both the application that will be deployed and the clipboard probe.
    # `--examples` alone leaves a stale target/release/ohmycopy.exe in place,
    # which makes a VM smoke appear to test a source revision it never ran.
    subprocess.check_call(
        ["cargo", "build", "--release", "--bins", "--examples"], cwd=ROOT
    )
    probe = find_probe()
    if not EXE.exists():
        print("missing ohmycopy.exe", file=sys.stderr)
        return 2

    local_ip = lan_ip(host)
    print(f"Local IP={local_ip} VM={host}")

    # Local firewall
    subprocess.run(
        [
            "netsh",
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=OhMyCopyE2E-Local",
            "dir=in",
            "action=allow",
            "protocol=TCP",
            "localport=3721",
        ],
        capture_output=True,
    )
    subprocess.run(
        [
            "netsh",
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=OhMyCopyE2E-Local-UDP",
            "dir=in",
            "action=allow",
            "protocol=UDP",
            "localport=3721",
        ],
        capture_output=True,
    )

    host_id = str(uuid.uuid4())
    vm_id = str(uuid.uuid4())
    token = "OHMYCOPY-E2E-" + uuid.uuid4().hex[:12]
    token2 = "OHMYCOPY-E2E-REV-" + uuid.uuid4().hex[:8]

    def cfg(name: str, did: str) -> dict:
        return {
            "config_version": 2,
            "device_name": name,
            "device_id": did,
            "tcp_port": PORT,
            "udp_port": PORT,
            "password": test_pass,
            "max_payload_bytes": 209715200,
            "history_limit": 100,
            "discover_interval_secs": 3,
            "theme": "dark_glass",
            "auto_start": False,
            "sync_enabled": True,
            "console": True,
            "start_minimized_to_tray": False,
        }

    vm_config = cfg("E2E-VM", vm_id)
    local_config = cfg("E2E-HOST", host_id)
    vm_clients = {
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
    }
    local_clients = {
        "version": 1,
        "clients": [
            {
                "device_id": vm_id,
                "name": "E2E-VM",
                "addr": f"{host}:{PORT}",
                "auto_connect": True,
                "last_seen": 0,
                "source": "manual",
                "ignored": False,
            }
        ],
    }

    import shutil

    local_work = ROOT / "target" / "e2e_local"
    local_data = local_work / DATA_DIR_NAME
    # Match all historical release executable names too.  Otherwise a manually
    # started `ohmycopy-windows-x64.exe` can keep port 3721, and the smoke
    # talks to a stale build rather than the staged binary.
    subprocess.run(
        [
            "powershell",
            "-NoProfile",
            "-Command",
            "Get-Process ohmycopy* -EA SilentlyContinue | Stop-Process -Force",
        ],
        capture_output=True,
    )
    # taskkill returns before Windows necessarily releases SQLite handles.  Wait
    # for the previous local smoke instance to exit before resetting its data.
    for _ in range(20):
        still_running = subprocess.run(
            ["powershell", "-NoProfile", "-Command", "@(Get-Process ohmycopy* -EA SilentlyContinue).Count"],
            capture_output=True,
            text=True,
        )
        if still_running.stdout.strip() == "0":
            break
        time.sleep(0.25)
    # The old executable handle can linger briefly after the process disappears.
    # Retrying makes repeated smoke runs reliable on Windows.
    for attempt in range(20):
        try:
            shutil.copy2(EXE, local_work / "ohmycopy.exe")
            break
        except PermissionError:
            if attempt == 19:
                raise
            time.sleep(0.25)
    local_data.mkdir(parents=True, exist_ok=True)
    host_audit = local_work / "host-audit.log"
    if host_audit.exists():
        host_audit.unlink()
    # clean old history for clean asserts
    for name in ("history.db", "history.db-wal", "history.db-shm"):
        p = local_data / name
        if p.exists():
            p.unlink()
    (local_data / "config.json").write_text(json.dumps(local_config, indent=2), encoding="utf-8")
    (local_data / "clients.json").write_text(
        json.dumps(local_clients, indent=2), encoding="utf-8"
    )

    print("=== SSH connect ===")
    c = ssh_connect(host, user, password)
    results: list[str] = []
    local_proc = None
    try:
        print("=== prepare VM ===")
        ps(
            c,
            rf"""
$ErrorActionPreference='SilentlyContinue'
New-Item -ItemType Directory -Force -Path '{REMOTE_DIR}' | Out-Null
Get-Process ohmycopy* | Stop-Process -Force
Start-Sleep -Seconds 1
Remove-Item -Recurse -Force (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') | Out-Null
""",
        )
        sftp_put(c, EXE, REMOTE_DIR + r"\ohmycopy.exe")
        sftp = c.open_sftp()
        try:
            with sftp.file(REMOTE_DIR + rf"\{DATA_DIR_NAME}\config.json", "w") as f:
                f.write(json.dumps(vm_config, indent=2))
            with sftp.file(REMOTE_DIR + rf"\{DATA_DIR_NAME}\clients.json", "w") as f:
                f.write(json.dumps(vm_clients, indent=2))
        finally:
            sftp.close()

        print("=== start VM headless (schtasks /IT console session) ===")
        rp = password.replace('"', '""')
        out = ps(
            c,
            rf"""
$ErrorActionPreference='Continue'
netsh advfirewall firewall delete rule name='OhMyCopyE2E' | Out-Null
netsh advfirewall firewall delete rule name='OhMyCopyE2E-UDP' | Out-Null
netsh advfirewall firewall delete rule name='OhMyCopyE2E-Prog' | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E' dir=in action=allow protocol=TCP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E-UDP' dir=in action=allow protocol=UDP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E-Prog' dir=in action=allow program='C:\OhMyCopyE2E\ohmycopy.exe' profile=any enable=yes | Out-Null
Get-Process ohmycopy* -EA SilentlyContinue | Stop-Process -Force
Start-Sleep 1
schtasks /Delete /TN OhMyCopyE2E /F 2>$null | Out-Null
$bat = @'
@echo off
set "OHMYCOPY_DATA_DIR=C:\OhMyCopyE2E\.ohmycopy"
set "OHMYCOPY_SYNC_AUDIT_PATH=C:\OhMyCopyE2E\vm-audit.log"
set "OHMYCOPY_SYNC_AUDIT=1"
set > C:\OhMyCopyE2E\ohmycopy-env.log
set "RUST_LOG=debug"
C:\OhMyCopyE2E\ohmycopy.exe --headless >> C:\OhMyCopyE2E\ohmycopy.log 2>&1
'@
Set-Content -Path '{REMOTE_DIR}\run_ohmycopy.bat' -Value $bat -Encoding ASCII
Remove-Item -Force '{REMOTE_DIR}\ohmycopy.log' -EA SilentlyContinue
Remove-Item -Force '{REMOTE_DIR}\vm-audit.log' -EA SilentlyContinue
Remove-Item -Force (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}\sync-audit.log') -EA SilentlyContinue
schtasks /Create /TN OhMyCopyE2E /TR "C:\OhMyCopyE2E\run_ohmycopy.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{user}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyE2E | Out-Null
Start-Sleep -Seconds 5
$p = Get-Process ohmycopy -EA SilentlyContinue | Select-Object -First 1
if (-not $p) {{ throw 'VM ohmycopy not running after schtasks' }}
Write-Output ('VM pid=' + $p.Id + ' session=' + $p.SessionId)
netstat -an | findstr 3721
""",
        )
        print(out.strip())

        for i in range(15):
            if tcp_open(host, PORT, 3):
                print(f"TCP to VM:3721 OK (try {i})")
                break
            print(f"wait TCP VM:3721 ({i})")
            time.sleep(1)
        else:
            print("ERROR: cannot TCP connect to VM:3721")
            results.append("FAIL tcp to VM:3721")
            return 1

        print("=== start local headless ===")
        local_proc = subprocess.Popen(
            [str(local_work / "ohmycopy.exe"), "--headless"],
            cwd=str(local_work),
            env={
                **os.environ,
                "OHMYCOPY_DATA_DIR": str(local_data),
                "OHMYCOPY_SYNC_AUDIT_PATH": str(host_audit),
                "OHMYCOPY_SYNC_AUDIT": "1",
                "RUST_LOG": "debug",
            },
            stdout=open(local_work / "stdout.log", "w"),
            stderr=open(local_work / "stderr.log", "w"),
        )
        for i in range(20):
            time.sleep(1)
            if tcp_open(local_ip, PORT, 2):
                print(f"local listen OK (try {i})")
                break
        print("waiting 12s for auto_connect handshake…")
        time.sleep(12)
        # The endpoint may be connected via either inbound or outbound TCP;
        # confirm an authenticated application session from the audit trail,
        # not merely that port 3721 accepts a socket.
        session_ready = False
        for i in range(20):
            tmp = Path(tempfile.gettempdir()) / "vm_sync_audit_ready.log"
            if sftp_get(c, REMOTE_DIR + r"\vm-audit.log", tmp):
                try:
                    session_ready = "net_session_ready" in tmp.read_text(errors="replace")
                except OSError:
                    pass
            if session_ready:
                print(f"authenticated session ready at {i}s")
                break
            time.sleep(1)
        if not session_ready:
            print("WARN authenticated session not observed in audit log")

        # ---- text local -> VM (verify VM history.db) ----
        print("=== TEST text local->VM ===")
        subprocess.check_call([str(probe), "set-text", token])
        ok = False
        for _ in range(15):
            time.sleep(1)
            tmp = Path(tempfile.gettempdir()) / "vm_history.db"
            if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp):
                if history_has(tmp, token):
                    ok = True
                    break
        if ok:
            print("PASS text local->VM (history.db)")
            results.append("PASS text local->VM")
        else:
            print("FAIL text local->VM (token not in VM history.db)")
            results.append("FAIL text local->VM")
            # dump local log tail
            for n in ("stdout.log", "stderr.log"):
                p = local_work / n
                if p.exists():
                    print(f"--- local {n} ---")
                    print(p.read_text(errors="replace")[-1500:])

        # Settle after FWD so remote sync-write + suppress fingerprint clear cleanly.
        time.sleep(5)

        # ---- text VM -> local: set clipboard in console session via bat + schtasks /IT
        # (schtasks /TR "exe arg1 arg2" often drops args; use a .bat wrapper)
        print("=== TEST text VM->local (probe via schtasks /IT bat) ===")
        sftp_put(c, probe, REMOTE_DIR + r"\clip_probe.exe")
        rp = password.replace('"', '""')
        ok = False
        for attempt in range(2):
            if attempt:
                print(f"  reverse retry #{attempt}")
                time.sleep(2)
            probe_log = ps(
                c,
                rf"""
$ErrorActionPreference='Continue'
$bat = @'
@echo off
C:\OhMyCopyE2E\clip_probe.exe set-text {token2} > C:\OhMyCopyE2E\probe_out.txt 2>&1
C:\OhMyCopyE2E\clip_probe.exe get-text > C:\OhMyCopyE2E\probe_get.txt 2>&1
'@
Set-Content -Path '{REMOTE_DIR}\run_probe.bat' -Value $bat -Encoding ASCII
Remove-Item -Force '{REMOTE_DIR}\probe_out.txt','{REMOTE_DIR}\probe_get.txt' -EA SilentlyContinue
schtasks /Delete /TN OhMyCopyProbe /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyProbe /TR "C:\OhMyCopyE2E\run_probe.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{user}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyProbe | Out-Null
Start-Sleep -Seconds 4
Write-Output '---probe_out---'
if (Test-Path '{REMOTE_DIR}\probe_out.txt') {{ Get-Content '{REMOTE_DIR}\probe_out.txt' -Raw }} else {{ 'MISSING' }}
Write-Output '---probe_get---'
if (Test-Path '{REMOTE_DIR}\probe_get.txt') {{ Get-Content '{REMOTE_DIR}\probe_get.txt' -Raw }} else {{ 'MISSING' }}
$p = Get-Process ohmycopy -EA SilentlyContinue | Select-Object -First 1
if ($p) {{ Write-Output ('ohmycopy pid=' + $p.Id + ' session=' + $p.SessionId) }} else {{ Write-Output 'ohmycopy NOT RUNNING' }}
netstat -an | findstr ':3721'
""",
                check=False,
            )
            print(probe_log.strip()[-1200:])
            for _ in range(18):
                time.sleep(1)
                if history_has(local_data / "history.db", token2):
                    ok = True
                    break
                try:
                    t = subprocess.check_output(
                        [str(probe), "get-text"], text=True, errors="replace", timeout=5
                    )
                    if token2 in t:
                        ok = True
                        break
                except Exception:
                    pass
            if ok:
                break
        if ok:
            print("PASS text VM->local")
            results.append("PASS text VM->local")
        else:
            print("FAIL text VM->local")
            results.append("FAIL text VM->local")
            # Did VM watcher even see the copy? (local history insert on VM)
            tmp = Path(tempfile.gettempdir()) / "vm_history_rev.db"
            if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp):
                print(
                    "  VM history has reverse token:",
                    history_has(tmp, token2),
                )
            for n in ("stdout.log", "stderr.log"):
                p = local_work / n
                if p.exists():
                    print(f"--- local {n} tail ---")
                    print(p.read_text(errors="replace")[-2000:])

        # ---- image ----
        print("=== TEST image local->VM ===")
        png = local_work / "probe.png"
        subprocess.check_call([str(probe), "make-png", str(png)])
        subprocess.check_call([str(probe), "set-image-png", str(png)])
        ok = False
        for _ in range(15):
            time.sleep(1)
            # count png under remote inbox
            cnt = ps(
                c,
                rf"""
$inbox = Join-Path (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') 'inbox'
if (-not (Test-Path $inbox)) {{ 0; return }}
@(Get-ChildItem $inbox -Recurse -File -Filter '*.png' -EA SilentlyContinue).Count
""",
                check=False,
            ).strip()
            try:
                n = int([x for x in cnt.splitlines() if x.strip().isdigit()][-1])
            except Exception:
                n = 0
            if n >= 1:
                ok = True
                break
            tmp = Path(tempfile.gettempdir()) / "vm_history2.db"
            if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp):
                if history_has(tmp, "[图片]") or history_has(tmp, "image"):
                    ok = True
                    break
        if ok:
            print("PASS image local->VM")
            results.append("PASS image local->VM")
        else:
            print("FAIL image local->VM")
            results.append("FAIL image local->VM")

        # Let the remote bitmap clipboard write and watcher suppression settle
        # before replacing it with the same PNG as an HDROP file.
        time.sleep(2)

        # ---- image *file* via CF_HDROP ----
        # A .png copied from Explorer must arrive as a file-list clipboard entry,
        # rather than being converted into bitmap clipboard data.  Run the probe
        # in the VM's interactive session so Get-Clipboard sees the same desktop
        # clipboard that users paste from.
        print("=== TEST image file local->VM (OS HDROP) ===")
        image_file_name = f"image-file-{uuid.uuid4().hex[:8]}.png"
        image_file = local_work / image_file_name
        image_file.write_bytes(png.read_bytes())
        subprocess.check_call([str(probe), "set-file", str(image_file)])
        ok = False
        for i in range(20):
            time.sleep(1)
            # The final user-visible contract is CF_HDROP on the VM desktop.
            # Probe it directly instead of inferring clipboard state from the
            # inbox location (which may be redirected by a Windows user profile).
            file_kind = ps(
                c,
                rf"""
$bat = @'
@echo off
C:\OhMyCopyE2E\clip_probe.exe get-kind > C:\OhMyCopyE2E\probe_image_file_kind.txt 2>&1
'@
Set-Content -Path '{REMOTE_DIR}\run_probe_image_file.bat' -Value $bat -Encoding ASCII
Remove-Item -Force '{REMOTE_DIR}\probe_image_file_kind.txt' -EA SilentlyContinue
schtasks /Delete /TN OhMyCopyProbeImageFile /F 2>$null | Out-Null
schtasks /Create /TN OhMyCopyProbeImageFile /TR "C:\OhMyCopyE2E\run_probe_image_file.bat" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{user}" /RP "{rp}" /IT | Out-Null
schtasks /Run /TN OhMyCopyProbeImageFile | Out-Null
Start-Sleep -Seconds 2
if (Test-Path '{REMOTE_DIR}\probe_image_file_kind.txt') {{ Get-Content '{REMOTE_DIR}\probe_image_file_kind.txt' -Raw }} else {{ 'MISSING' }}
""",
                check=False,
            )
            if "files 1" in file_kind.lower() and image_file_name.lower() in file_kind.lower():
                print(f"  VM clipboard reports a file list at {i}s")
                ok = True
                break
        if not ok:
            print("  VM clipboard probe:", file_kind.strip())

        if ok:
            print("PASS image file local->VM (OS HDROP)")
            results.append("PASS image file local->VM (OS HDROP)")
        else:
            print("FAIL image file local->VM (OS HDROP)")
            results.append("FAIL image file local->VM (OS HDROP)")

        # ---- small file ----
        print("=== TEST file local->VM ===")
        bin_path = local_work / "probe-bin.dat"
        bin_path.write_bytes(bytes((i * 17) % 256 for i in range(256 * 1024)))
        subprocess.check_call([str(probe), "set-file", str(bin_path)])
        ok = False
        for _ in range(20):
            time.sleep(1)
            hit = ps(
                c,
                rf"""
$inbox = Join-Path (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') 'inbox'
if (-not (Test-Path $inbox)) {{ '0'; return }}
$n = @(Get-ChildItem $inbox -Recurse -File -EA SilentlyContinue | Where-Object {{ $_.Length -eq 262144 }}).Count
$n
""",
                check=False,
            ).strip()
            try:
                n = int([x for x in hit.splitlines() if x.strip().isdigit()][-1])
            except Exception:
                n = 0
            if n >= 1:
                ok = True
                break
            tmp = Path(tempfile.gettempdir()) / "vm_history3.db"
            if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp) and history_has(tmp, "probe-bin"):
                ok = True
                break
        if ok:
            print("PASS file local->VM")
            results.append("PASS file local->VM")
        else:
            print("FAIL file local->VM")
            results.append("FAIL file local->VM")

        # ---- large file (default 8 MiB; OHMYCOPY_E2E_LARGE_MB) ----
        large_mb = int(os.environ.get("OHMYCOPY_E2E_LARGE_MB", "8") or "8")
        large_mb = max(1, min(large_mb, 100))
        large_size = large_mb * 1024 * 1024
        print(f"=== TEST large file local->VM ({large_mb} MiB) ===")
        large_tag = uuid.uuid4().hex[:8]
        large_name = f"large-{large_mb}m-{large_tag}.bin"
        large_path = local_work / large_name
        # Stream write to avoid huge Python list overhead
        with open(large_path, "wb") as f:
            chunk = bytes((i * 17) % 256 for i in range(1024 * 1024))
            for _ in range(large_mb):
                f.write(chunk)
            # unique head/tail markers
            f.seek(0)
            f.write(b"\xA5")
            f.seek(large_size - 1)
            f.write(b"\x5A")
        # clear prior same-size leftovers is unnecessary — match unique name
        subprocess.check_call([str(probe), "set-file", str(large_path)])
        ok = False
        wait_s = max(60, large_mb * 8)
        for i in range(wait_s):
            time.sleep(1)
            hit = ps(
                c,
                rf"""
$inbox = Join-Path (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') 'inbox'
if (-not (Test-Path $inbox)) {{ '0'; return }}
$n = @(Get-ChildItem $inbox -Recurse -File -EA SilentlyContinue | Where-Object {{
  $_.Name -eq '{large_name}' -and $_.Length -eq {large_size}
}}).Count
$n
""",
                check=False,
            ).strip()
            try:
                n = int([x for x in hit.splitlines() if x.strip().isdigit()][-1])
            except Exception:
                n = 0
            if n >= 1:
                ok = True
                print(f"  found large file on VM at {i}s")
                break
            if i % 10 == 9:
                tmp = Path(tempfile.gettempdir()) / "vm_history_lg.db"
                if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp) and history_has(
                    tmp, large_tag
                ):
                    ok = True
                    print(f"  found large file in VM history at {i}s")
                    break
        if ok:
            print(f"PASS large file local->VM ({large_mb} MiB)")
            results.append(f"PASS large file local->VM ({large_mb} MiB)")
        else:
            print(f"FAIL large file local->VM ({large_mb} MiB)")
            results.append(f"FAIL large file local->VM ({large_mb} MiB)")

        # ---- folder via CF_HDROP (clip_probe set-folder) ----
        print("=== TEST folder local->VM (OS HDROP) ===")
        folder = local_work / "sample_folder"
        if folder.exists():
            import shutil as _shutil

            _shutil.rmtree(folder, ignore_errors=True)
        folder.mkdir(parents=True, exist_ok=True)
        folder_marker = "folder-hello-" + uuid.uuid4().hex[:8]
        (folder / "hello.txt").write_text(folder_marker, encoding="utf-8")
        (folder / "nested").mkdir(exist_ok=True)
        (folder / "nested" / "x.txt").write_text("nested-x", encoding="utf-8")
        # ~1 MiB inside folder to exercise zip path a bit
        (folder / "nested" / "chunk.bin").write_bytes(
            bytes((i * 3) % 256 for i in range(1024 * 1024))
        )
        subprocess.check_call([str(probe), "set-folder", str(folder)])
        ok = False
        for i in range(45):
            time.sleep(1)
            # Extracted folder should contain hello.txt with marker
            hit = ps(
                c,
                rf"""
$roots = @((Join-Path (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') 'inbox'), 'C:\Users\{user}\.ohmycopy\inbox')
$hits = $roots | Where-Object {{ Test-Path $_ }} | ForEach-Object {{
  Get-ChildItem $_ -Recurse -File -Filter 'hello.txt' -EA SilentlyContinue
}} | Where-Object {{ (Get-Content $_.FullName -Raw -EA SilentlyContinue) -match '{folder_marker}' }}
@($hits).Count
""",
                check=False,
            ).strip()
            try:
                n = int([x for x in hit.splitlines() if x.strip().isdigit()][-1])
            except Exception:
                n = 0
            if n >= 1:
                ok = True
                print(f"  folder extract ok at {i}s")
                break
            tmp = Path(tempfile.gettempdir()) / "vm_history_folder.db"
            if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp):
                if history_has(tmp, "sample_folder") or history_has(tmp, "文件夹"):
                    # history may list folder before extract finishes; keep waiting for files
                    if i >= 5 and history_has(tmp, "sample_folder"):
                        # confirm chunk.bin size exists
                        hit2 = ps(
                            c,
                            rf"""
$roots = @((Join-Path (Join-Path '{REMOTE_DIR}' '{DATA_DIR_NAME}') 'inbox'), 'C:\Users\{user}\.ohmycopy\inbox')
$n = @($roots | Where-Object {{ Test-Path $_ }} | ForEach-Object {{
  Get-ChildItem $_ -Recurse -File -EA SilentlyContinue
}} | Where-Object {{ $_.Name -eq 'chunk.bin' -and $_.Length -eq 1048576 }}).Count
$n
""",
                            check=False,
                        ).strip()
                        try:
                            n2 = int([x for x in hit2.splitlines() if x.strip().isdigit()][-1])
                        except Exception:
                            n2 = 0
                        if n2 >= 1:
                            ok = True
                            break
        if ok:
            print("PASS folder local->VM (OS HDROP)")
            results.append("PASS folder local->VM (OS HDROP)")
        else:
            print("FAIL folder local->VM (OS HDROP)")
            results.append("FAIL folder local->VM (OS HDROP)")
            tmp = Path(tempfile.gettempdir()) / "vm_history_folder_fail.db"
            if sftp_get(c, REMOTE_DIR + rf"\{DATA_DIR_NAME}\history.db", tmp):
                print("  VM history has sample_folder:", history_has(tmp, "sample_folder"))

    finally:
        print("=== cleanup ===")
        if local_proc and local_proc.poll() is None:
            local_proc.terminate()
            try:
                local_proc.wait(timeout=3)
            except Exception:
                local_proc.kill()
        subprocess.run(
            ["powershell", "-NoProfile", "-Command", "Get-Process ohmycopy* -EA SilentlyContinue | Stop-Process -Force"],
            capture_output=True,
        )
        try:
            ps(c, "Get-Process ohmycopy* -EA SilentlyContinue | Stop-Process -Force", check=False)
        except Exception:
            pass
        c.close()

    print("\n======== SUMMARY ========")
    fail = 0
    for r in results:
        print(r)
        if r.startswith("FAIL"):
            fail += 1
    if fail:
        print(f"\n{fail} test(s) FAILED")
        return 1
    print("\nAll runnable VM smoke tests PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
