import base64
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import time
import uuid
from pathlib import Path

import paramiko

ROOT = Path(r"D:\myrepo\code\rust\OhMyCopy")
EXE = ROOT / "target" / "release" / "ohmycopy.exe"
PROBE = next((ROOT / "target" / "release").rglob("clip_probe.exe"))
host = "192.168.75.201"
user = "NRC"
password = os.environ["OHMYCOPY_VM_PASSWORD"]
work = ROOT / "target" / "e2e_dbg"
if work.exists():
    shutil.rmtree(work, ignore_errors=True)
work.mkdir(parents=True)
host_id = str(uuid.uuid4())
vm_id = str(uuid.uuid4())
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.connect((host, 1))
lip = s.getsockname()[0]
s.close()
print("lip", lip, "host_id", host_id, "vm_id", vm_id)


def cfg(name, did):
    return {
        "config_version": 2,
        "device_name": name,
        "device_id": did,
        "tcp_port": 3721,
        "udp_port": 3721,
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


(work / "config.json").write_text(json.dumps(cfg("HOST", host_id), indent=2), encoding="utf-8")
(work / "clients.json").write_text(
    json.dumps(
        {
            "version": 1,
            "clients": [
                {
                    "device_id": vm_id,
                    "name": "VM",
                    "addr": f"{host}:3721",
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
shutil.copy2(EXE, work / "ohmycopy.exe")
subprocess.run(["taskkill", "/F", "/IM", "ohmycopy.exe"], capture_output=True)

c = paramiko.SSHClient()
c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
c.connect(host, username=user, password=password, allow_agent=False, look_for_keys=False)


def ps(script: str) -> str:
    data = base64.b64encode(script.encode("utf-16le")).decode()
    _, o, e = c.exec_command(f"powershell -NoProfile -EncodedCommand {data}", timeout=60)
    out = o.read().decode("utf-8", "replace")
    err = e.read().decode("utf-8", "replace")
    if err.strip() and "CLIXML" not in err:
        print("PS ERR", err[:400])
    return out


ps(
    """
Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force
Start-Sleep 1
"""
)
sftp = c.open_sftp()
sftp.put(str(EXE), "C:/OhMyCopyE2E/ohmycopy.exe")
with sftp.file("C:/OhMyCopyE2E/config.json", "w") as f:
    f.write(json.dumps(cfg("VM", vm_id), indent=2))
with sftp.file("C:/OhMyCopyE2E/clients.json", "w") as f:
    f.write(
        json.dumps(
            {
                "version": 1,
                "clients": [
                    {
                        "device_id": host_id,
                        "name": "HOST",
                        "addr": f"{lip}:3721",
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

print(
    ps(
        r"""
netsh advfirewall firewall add rule name=OhMyCopyE2E dir=in action=allow protocol=TCP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name=OhMyCopyE2E-Prog dir=in action=allow program=C:\OhMyCopyE2E\ohmycopy.exe profile=any enable=yes | Out-Null
Start-Process C:\OhMyCopyE2E\ohmycopy.exe -ArgumentList '--headless' -WorkingDirectory C:\OhMyCopyE2E -WindowStyle Hidden
Start-Sleep 3
netstat -an | findstr 3721
"""
    )
)

lp = subprocess.Popen(
    [str(work / "ohmycopy.exe"), "--headless"],
    cwd=str(work),
    stdout=open(work / "out.log", "w"),
    stderr=open(work / "err.log", "w"),
)
print("local pid", lp.pid)
time.sleep(18)
print("=== local netstat 3721 ===")
print(subprocess.check_output("netstat -an | findstr 3721", shell=True, text=True, errors="replace"))
print("=== established to VM ===")
print(
    subprocess.check_output(
        f'netstat -an | findstr "{host}:3721"', shell=True, text=True, errors="replace"
    )
)
print("=== VM established ===")
print(
    ps(
        "netstat -an | findstr 3721"
    )
)

token = "DBG-" + uuid.uuid4().hex[:10]
subprocess.check_call([str(PROBE), "set-text", token])
print("set token", token)
time.sleep(6)

for label, path in [("local", work / "history.db")]:
    if path.exists():
        con = sqlite3.connect(str(path))
        rows = con.execute("select preview, content from history").fetchall()
        con.close()
        print(label, "rows", len(rows), rows[:8])
    else:
        print(label, "no history.db")

tmp = Path(r"C:\Users\Administrator\AppData\Local\Temp\vm_h.db")
sftp = c.open_sftp()
try:
    sftp.get("C:/OhMyCopyE2E/history.db", str(tmp))
    con = sqlite3.connect(str(tmp))
    rows = con.execute("select preview, content from history").fetchall()
    con.close()
    print("vm rows", len(rows), rows[:8])
except Exception as e:
    print("vm history get failed", e)
sftp.close()

# kind on local clip
print("clip kind after set:")
print(subprocess.check_output([str(PROBE), "get-kind"], text=True, errors="replace"))

subprocess.run(["taskkill", "/F", "/IM", "ohmycopy.exe"], capture_output=True)
ps("Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force")
c.close()
print("done")
