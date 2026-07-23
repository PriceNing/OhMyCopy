# OhMyCopy 自动化测试说明

对应实现版本：**v0.1.35+**。

## 默认原则：一次跑全量

**推荐入口**（本地全量，含文本/图/文件/文件夹/大文件）：

```powershell
cd D:\myrepo\code\rust\OhMyCopy
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
```

脚本**始终**执行：

| 层级 | 覆盖 |
|------|------|
| `cargo test --release --tests --lib` | 全部单元 + 集成 |
| hub 矩阵（显式过滤，日志清晰） | 文本+图+文件、**大文件**、**文件夹/大文件夹**、反向、ignore、unpair、鉴权失败、超限 |
| VM smoke（有环境变量或 `-VmSmoke`） | 真剪贴板：文本双向、图、文件、大文件、文件夹 HDROP |

大文件默认 `OHMYCOPY_E2E_LARGE_MB=8`；加大：

```powershell
$env:OHMYCOPY_E2E_LARGE_MB = "20"
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -Large
```

## 本机 + 虚拟机（发版前）

设置好三个环境变量后，**同一脚本会自动接上 VM smoke**（不必再记第二个命令）：

```powershell
$env:OHMYCOPY_VM_HOST = "192.168.1.100"   # 你的测试机 IP
$env:OHMYCOPY_VM_USER = "your-user"
$env:OHMYCOPY_VM_PASSWORD = "your-password"   # 仅环境变量；勿写入仓库
# 可选：$env:OHMYCOPY_E2E_LARGE_MB = "8"
powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1
# 或强制要求 VM：
# powershell -ExecutionPolicy Bypass -File scripts\run_auto_tests.ps1 -VmSmoke -RequireVm
```

VM 前提：SSH 可用；用户已登录**控制台会话**；脚本用 `schtasks /IT`（勿用 Session 0）。

### VM smoke 用例清单

1. 文本 本机→VM  
2. 文本 VM→本机  
3. 图片位图 本机→VM
4. 图片文件 本机→VM（`clip_probe set-file probe.png` → OS HDROP；VM 交互会话确认仍为文件列表）
5. 小文件 本机→VM
6. 大文件 本机→VM（`OHMYCOPY_E2E_LARGE_MB`，默认 8）
7. 文件夹 本机→VM（`clip_probe set-folder` → OS HDROP）

也可单独跑：`python scripts\vm_ssh_smoke.py`

## 仅 cargo（等价本地全量）

```powershell
cargo test --release --tests --lib
```

| 套件 | 覆盖 |
|------|------|
| lib | auth、engine、config、clients、history、inbox、discover |
| hub_pair_e2e | 配对、文本/图/文件、文件夹、大文件、反向、ignore、unpair、鉴权… |
| settings_and_clients | 配置、历史预览、inbox、PNG |
| protocol_sync / e2e_sync | 协议帧、发现包、鉴权+剪贴板 |

## 日常建议

| 场景 | 命令 |
|------|------|
| 日常改代码 | `scripts\run_auto_tests.ps1` |
| 发版 / 剪贴板回归 | 配好 `OHMYCOPY_VM_*` 后跑同一脚本 |
| 只要协议大文件 | `$env:OHMYCOPY_E2E_LARGE_MB=20; cargo test --release --test hub_pair_e2e hub_large_file_sync` |

## 说明

- Hub e2e **不经过** arboard，验证配对与加密传输。  
- VM smoke 验证 **真实 OS 剪贴板**（需交互会话）。  
- GUI/托盘点击仍需人工或 computer-use。  
