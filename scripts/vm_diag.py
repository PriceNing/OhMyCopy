import base64
import os
import socket
import sys

import paramiko

host = os.environ.get("OHMYCOPY_VM_HOST", "192.168.75.201")
user = os.environ.get("OHMYCOPY_VM_USER", "NRC")
password = os.environ["OHMYCOPY_VM_PASSWORD"]


def tcp(port):
    try:
        with socket.create_connection((host, port), timeout=3):
            return True
    except OSError as e:
        return f"no:{e}"


print("tcp 3721", tcp(3721))
print("tcp 22", tcp(22))

c = paramiko.SSHClient()
c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
c.connect(host, username=user, password=password, allow_agent=False, look_for_keys=False)

script = r"""
$ErrorActionPreference='Continue'
'--- process ---'
Get-Process ohmycopy -EA SilentlyContinue | Format-List Id,Path
'--- listen ---'
Get-NetTCPConnection -LocalPort 3721 -EA SilentlyContinue | Format-Table LocalAddress,State,OwningProcess -AutoSize
'--- netstat ---'
netstat -an | findstr 3721
'--- firewall ---'
netsh advfirewall firewall show rule name=all | findstr /i "3721 OhMyCopy"
'--- dir ---'
Get-ChildItem C:\OhMyCopyE2E -EA SilentlyContinue | Format-Table Name,Length
'--- whoami ---'
whoami
"""
data = base64.b64encode(script.encode("utf-16le")).decode()
_, stdout, stderr = c.exec_command(f"powershell -NoProfile -EncodedCommand {data}")
print(stdout.read().decode("utf-8", "replace"))
print(stderr.read().decode("utf-8", "replace"))
c.close()
