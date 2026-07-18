# OhMyCopy 自动化测试说明

## 我能自动测什么

| 类型 | 是否需要 VM | 覆盖内容 | 命令 |
|------|-------------|----------|------|
| 单元测试 | 否 | 协议、鉴权、历史、inbox、配置 | `cargo test --lib` |
| Hub 端到端 | 否 | 双节点配对、文本/图片/文件同步、错密码 | `cargo test --test hub_pair_e2e` |
| 大文件 | 否 | 可调 20–50MB+ 编码传输 | `OHMYCOPY_E2E_LARGE_MB=50 cargo test --test hub_pair_e2e` |
| 真机剪贴板（截图/微信/资源管理器） | **需要 2 台 Windows**（本机+VM） | OS 剪贴板格式 | 见下方 VM |

**GUI 点击 / 托盘 / 第三方截图工具** 需要桌面会话；可在本机用 computer-use 或人工点一次，协议层用 hub 测试覆盖。

## 一键本地

```powershell
cd D:\myrepo\code\rust\OhMyCopy
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1

# 加大文件用例（例如 20 MiB）
$env:OHMYCOPY_E2E_LARGE_MB = "20"
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
```

## 本地测试清单（`cargo test --release --tests --lib`）

| 套件 | 覆盖 |
|------|------|
| `lib` 单元 | auth、engine 去重/回声抑制、config、clients ignore、history、inbox zip、discover |
| `hub_pair_e2e` | 配对、文本/图/文件、文件夹 zip、B→A 反向、ignore 静音、unpair、超限拒绝、错密码 |
| `settings_and_clients` | 配置字段、clients 序列化、历史预览不泄路径、inbox 收据目录、PNG 往返 |
| `protocol_sync` / `e2e_sync` | Hello/Unpair/Clipboard postcard、发现包、双端鉴权+剪贴板帧 |

大文件 / 大文件夹 hub：

```powershell
# 默认 8 MiB 大文件 + ~3.5 MiB 文件夹 zip 已在 hub_pair_e2e 中固定跑
cargo test --release --test hub_pair_e2e hub_large -- --nocapture

# 加大（例如 20 MiB）
$env:OHMYCOPY_E2E_LARGE_MB = "20"
cargo test --release --test hub_pair_e2e hub_large_file_sync -- --nocapture
```

VM 实机大文件/文件夹（`vm_ssh_smoke.py` 默认 8 MiB + OS HDROP 文件夹）：

```powershell
$env:OHMYCOPY_E2E_LARGE_MB = "12"   # 可选
python scripts\vm_ssh_smoke.py
```

## 虚拟机真实剪贴板（SSH + schtasks）

推荐脚本（不依赖 WinRM，需要 VM 开 SSH 且用户已登录控制台会话）：

```powershell
$env:OHMYCOPY_VM_HOST = "192.168.75.201"
$env:OHMYCOPY_VM_USER = "NRC"
$env:OHMYCOPY_VM_PASSWORD = "你的密码"   # 勿提交到仓库
python scripts\vm_ssh_smoke.py
```

会验证：

1. 文本 本机→VM（VM `history.db`）  
2. 文本 VM→本机（交互会话 `schtasks /IT` + `clip_probe` bat）  
3. 图片 / 文件 本机→VM（inbox / history）  
4. 文件夹 OS-HDROP：SKIP（协议层由 `hub_folder_zip_sync` 覆盖）

### WinRM 备选（`run_auto_tests.ps1 -RemoteVm`）

```powershell
$env:OHMYCOPY_VM_HOST = "192.168.75.201"
$env:OHMYCOPY_VM_USER = "Administrator"
# 可选：$env:OHMYCOPY_VM_PASSWORD = "你的密码"
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -RemoteVm
```

注意：headless 必须跑在**已登录的交互会话**（`schtasks /IT`），Session 0 无法可靠读写桌面剪贴板。

## 日常开发建议

- 改协议 / 超时 / 大文件 → 先跑 `hub_pair_e2e`  
- 改 UI / 托盘 → 本机手动或 computer-use  
- 发版前 → `scripts\run_auto_tests.ps1` + 可选 `-RemoteVm`

## 说明

- Hub e2e **不经过** arboard 系统剪贴板，专门验证配对与加密传输（稳定、可 CI）。  
- 系统剪贴板格式（PixPin/微信）依赖 OS，需双机真实会话；协议层已在 0.1.32+ 增强。  
