# OhMyCopy 自动化测试说明

对应实现版本：**v0.1.34**。

## 我能自动测什么

| 类型 | 是否需要 VM | 覆盖内容 | 命令 |
|------|-------------|----------|------|
| 单元测试 | 否 | 协议、鉴权、历史、inbox、配置、clients | `cargo test --lib` |
| Hub 端到端 | 否 | 双节点配对、文本/图片/文件/文件夹、大文件、ignore、unpair、错密码 | `cargo test --test hub_pair_e2e` |
| 大文件 | 否 | 默认 8MiB；可调 | `$env:OHMYCOPY_E2E_LARGE_MB=20; cargo test --test hub_pair_e2e hub_large_file_sync` |
| 真机剪贴板 | **2 台 Windows**（本机+VM） | OS 剪贴板（文本双向、图/文件/文件夹 HDROP） | `python scripts/vm_ssh_smoke.py` |

**GUI / 托盘点击** 需桌面会话；协议与配对层用 hub 测试覆盖。

## 一键本地

```powershell
cd D:\myrepo\code\rust\OhMyCopy
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1

# 加大文件用例（例如 20 MiB）
$env:OHMYCOPY_E2E_LARGE_MB = "20"
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -Large
```

## 本地测试清单

| 套件 | 覆盖 |
|------|------|
| `lib` | auth、engine 去重/回声抑制、config、clients ignore、history、inbox zip、discover |
| `hub_pair_e2e` | 配对、文本/图/文件、文件夹 zip 解压、大文件/大文件夹、B→A、ignore、unpair、超限、错密码 |
| `settings_and_clients` | 配置字段、clients JSON、历史预览、inbox 收据、PNG 往返 |
| `protocol_sync` / `e2e_sync` | Hello/Unpair/Clipboard、发现包、双端鉴权+剪贴板帧 |

```powershell
cargo test --release --tests --lib
cargo test --release --test hub_pair_e2e hub_large -- --nocapture
```

## 虚拟机真实剪贴板（推荐：SSH + schtasks）

前提：VM 开启 SSH；用户已登录**控制台会话**；本机与 VM 同局域网。

```powershell
$env:OHMYCOPY_VM_HOST = "192.168.75.201"
$env:OHMYCOPY_VM_USER = "NRC"
$env:OHMYCOPY_VM_PASSWORD = "你的密码"   # 勿提交到仓库
$env:OHMYCOPY_E2E_LARGE_MB = "8"        # 可选，默认 8
python scripts\vm_ssh_smoke.py
```

脚本会：编译 release + `clip_probe` → 部署到 VM `C:\OhMyCopyE2E\` → `schtasks /IT` 在会话 1 启动 headless → 验证：

1. 文本 本机→VM（VM `history.db`）  
2. 文本 VM→本机（`clip_probe` + bat + schtasks /IT）  
3. 图片 本机→VM（inbox `*.png`）  
4. 小文件 本机→VM（256KiB）  
5. 大文件 本机→VM（默认 8MiB，唯一文件名校验）  
6. 文件夹 本机→VM（`clip_probe set-folder` → OS HDROP → zip 解压校验）  

> **不要用 Session 0 的 `Start-Process` 跑 headless 做剪贴板测试**——读不到交互桌面剪贴板。

辅助脚本：`examples/clip_probe.rs`（`set-text` / `set-file` / `set-folder` / `set-image-png`）。  
调试用 `scripts/vm_debug_*.py` 不必进发版流水线。

### WinRM 备选

```powershell
$env:OHMYCOPY_VM_HOST = "192.168.75.201"
$env:OHMYCOPY_VM_USER = "Administrator"
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -RemoteVm
```

WinRM 仅部署对端；真剪贴板仍建议用 `vm_ssh_smoke.py`。

## 日常开发建议

| 改动 | 先跑 |
|------|------|
| 协议 / 超时 / 大文件 | `hub_pair_e2e` |
| 配置 / clients / inbox | `settings_and_clients` + `--lib` |
| 系统剪贴板格式 | `vm_ssh_smoke.py` 或本机手动 |
| UI / 托盘 | 本机手动 |

## 说明

- Hub e2e **不经过** arboard，只验证配对与加密传输（可 CI）。  
- 系统剪贴板格式（PixPin/微信/资源管理器）依赖 OS 会话；协议与格式增强见 0.1.32+。  
