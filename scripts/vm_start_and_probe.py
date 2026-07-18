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

script = r"""
$ErrorActionPreference = 'Continue'
Get-Process ohmycopy -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
netsh advfirewall firewall delete rule name='OhMyCopyE2E' | Out-Null
netsh advfirewall firewall delete rule name='OhMyCopyE2E-UDP' | Out-Null
netsh advfirewall firewall delete rule name='OhMyCopyE2E-Prog' | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E' dir=in action=allow protocol=TCP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E-UDP' dir=in action=allow protocol=UDP localport=3721 profile=any enable=yes | Out-Null
netsh advfirewall firewall add rule name='OhMyCopyE2E-Prog' dir=in action=allow program='C:\OhMyCopyE2E\ohmycopy.exe' profile=any enable=yes | Out-Null
$exe = 'C:\OhMyCopyE2E\ohmycopy.exe'
$proc = Start-Process -FilePath $exe -ArgumentList '--headless' -WorkingDirectory 'C:\OhMyCopyE2E' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 4
Write-Output ('pid=' + $proc.Id)
Write-Output '--- netstat ---'
netstat -an | findstr 3721
Write-Output '--- process ---'
Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Format-List Id,Path
"""

data = base64.b64encode(script.encode("utf-16le")).decode()
_, stdout, stderr = c.exec_command(
    f"powershell -NoProfile -EncodedCommand {data}", timeout=60
)
print(stdout.read().decode("utf-8", "replace"))
print(stderr.read().decode("utf-8", "replace")[:500])

for i in range(12):
    try:
        with socket.create_connection((host, 3721), timeout=2):
            print("TCP 3721 OK from host")
            break
    except OSError as e:
        print(f"TCP wait {i}: {e}")
        time.sleep(1)
else:
    print("TCP 3721 STILL FAIL from host")

c.close()
