"""Start OhMyCopy on VM in interactive user session via schtasks, then probe TCP."""
import base64
import os
import socket
import time

import paramiko

host = os.environ.get("OHMYCOPY_VM_HOST", "192.168.75.201")
user = os.environ.get("OHMYCOPY_VM_USER", "NRC")
password = os.environ["OHMYCOPY_VM_PASSWORD"]

c = paramiko.SSHClient()
c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
c.connect(host, username=user, password=password, allow_agent=False, look_for_keys=False)


def ps(script: str) -> str:
    data = base64.b64encode(script.encode("utf-16le")).decode()
    _, o, e = c.exec_command(f"powershell -NoProfile -EncodedCommand {data}", timeout=90)
    out = o.read().decode("utf-8", "replace")
    err = e.read().decode("utf-8", "replace")
    print(out)
    if err.strip() and "CLIXML" not in err[:50]:
        print("ERR", err[:600])
    return out


# Password for schtasks /RP — escape carefully
rp = password.replace('"', '""')

script = rf"""
$ErrorActionPreference = 'Continue'
Get-Process ohmycopy -EA SilentlyContinue | Stop-Process -Force
Start-Sleep 1
netsh advfirewall firewall delete rule name='OhMyCopyE2E' | Out-Null
netsh advfirewall firewall delete rule name='OhMyCopyE2E-UDP' | Out-Null
netsh advfirewall firewall delete rule name='OhMyCopyE2E-Prog' | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E' dir=in action=allow protocol=TCP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E-UDP' dir=in action=allow protocol=UDP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E-Prog' dir=in action=allow program='C:\OhMyCopyE2E\ohmycopy.exe' profile=any enable=yes | Out-Null

Write-Output '--- sessions ---'
query user

# Run in interactive session if available
schtasks /Delete /TN OhMyCopyE2E /F 2>$null | Out-Null
# /IT requires user logged on interactively
$create = schtasks /Create /TN OhMyCopyE2E /TR "C:\OhMyCopyE2E\ohmycopy.exe --headless" /SC ONCE /ST 00:00 /RL HIGHEST /F /RU "{user}" /RP "{rp}" /IT
Write-Output $create
$run = schtasks /Run /TN OhMyCopyE2E
Write-Output $run
Start-Sleep -Seconds 4
Write-Output '--- after schtasks ---'
Get-Process ohmycopy -EA SilentlyContinue | Format-List Id,Path,SessionId
netstat -an | findstr 3721

# Fallback: plain Start-Process if still not listening
$listening = netstat -an | Select-String ':3721' | Select-String 'LISTENING'
if (-not $listening) {{
  Write-Output 'fallback Start-Process'
  Start-Process -FilePath 'C:\OhMyCopyE2E\ohmycopy.exe' -ArgumentList '--headless' -WorkingDirectory 'C:\OhMyCopyE2E' -WindowStyle Hidden
  Start-Sleep 3
  netstat -an | findstr 3721
  Get-Process ohmycopy -EA SilentlyContinue | Format-List Id,SessionId
}}
"""

ps(script)

for i in range(15):
    try:
        with socket.create_connection((host, 3721), timeout=2):
            print(f"TCP OK try {i}")
            break
    except OSError as e:
        print(f"TCP fail {i}: {e}")
        time.sleep(1)
else:
    print("TCP STILL FAIL")

c.close()
