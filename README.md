# OhMyCopy

轻量级**局域网**跨设备剪贴板同步（Windows / Linux）

- 对等架构：无中心服务器，每台既监听也连接  
- TCP 加密同步 + UDP 广播/多播发现  
- 共享密码鉴权（Argon2id + ChaCha20-Poly1305）  
- egui 原生 UI（扁平 + 毛玻璃风格）+ 系统托盘  
- 目标：可靠、体积小、低内存  

规格与 as-built 说明见 [docs/开发文档.md](docs/开发文档.md)（**设计 v0.2 + 实现对齐当前版本**）。

仓库：<https://github.com/PriceNing/OhMyCopy>

## 功能状态（v0.1.36）

| 能力 | 状态 |
|------|------|
| 文本剪贴板同步 | ✅ |
| 图片 / 截图（PNG + CF_DIB/DIBV5 等） | ✅ |
| 文件同步（CF_HDROP / 整包） | ✅ |
| 文件夹同步（打包 zip，MIME `application/x-ohmycopy-dir-zip`） | ✅ |
| 密码鉴权 + 会话加密 | ✅ |
| 每对单 TCP + device_id 去重会话 | ✅ |
| event_id 去重 + 同步写入不外发 + 星型中继 | ✅ |
| UDP 设备发现（不自动写 clients） | ✅ |
| 配对列表 `clients.json`（鉴权成功后互写、自动重连） | ✅ |
| 忽略（静音）/ 解除配对 | ✅ |
| 历史记录（SQLite，预览不泄完整路径） | ✅ |
| 设置页 / 暂停同步 / 最小化到托盘 | ✅ |
| 防火墙失败提示 / headless 模式 | ✅ |
| 大载荷（配置上限 + 动态 IO 超时，帧上限约 512MiB） | ✅ |

## 快速开始

### 构建

```bash
cargo build --release
# 产物: target/release/ohmycopy.exe  (Windows)
# 体积约数 MB（release：strip + LTO + opt-level=s）
```

### 运行

1. 在两台（或多台）局域网电脑上启动**同一主版本** OhMyCopy  
2. **设置**中把两端改成**相同的共享密码**并保存（新安装默认是随机密码，**禁止**使用空/`change-me` 配对）  
3. 在 **设备**页：  
   - 看到附近设备后点击连接（密码正确后双方写入 `clients.json`），或  
   - 手动添加 `IP:3721`  
4. 已配对设备会自动重连；一侧复制文本/图片/文件/文件夹，另一侧约 1 秒内可粘贴  
   - 修改 **端口** 后需**重启应用**；修改密码会热更新（新连接立即生效）  

#### 无显卡 / 无 OpenGL 的机器

若 GUI 不可用，可无界面运行（同步仍可用）：

```bash
ohmycopy.exe --headless
# 或
set OHMYCOPY_HEADLESS=1
ohmycopy.exe
```

Windows 上 headless 若需读写**桌面剪贴板**，请在已登录的交互会话启动（例如计划任务 `/IT`），避免 Session 0。

### 配置与数据目录

配置与收件箱统一在用户主目录下的 **`.ohmycopy`**（Windows / Linux 相同约定）：

```text
Windows:  C:\Users\<你>\.ohmycopy\
Linux:    ~/.ohmycopy/

  config.json      # 名称、端口、密码、上限、console、托盘启动等
  clients.json     # 已配对客户端；启动后自动连接（可 ignored 静音）
  history.db       # 剪贴板历史
  inbox/           # 收到的文件/图片/文件夹解压收据目录
```

设置页可点 **「打开配置文件夹」**。  
若曾使用 exe 同目录或 `%APPDATA%\OhMyCopy` 布局，首次启动会尽量迁移到 `~/.ohmycopy`。

| 字段 | 说明 |
|------|------|
| `console` | `false`（默认）不弹黑框；需要日志时设 `true` |
| `start_minimized_to_tray` | `true` 时启动仅托盘 |
| `auto_start` | `true` 时写入当前用户开机/登录启动项（设置里勾选并保存） |
| `max_payload_bytes` | 默认 10MiB，可在设置中提高（大文件需两端一致） |
| `sync_enabled` | 暂停/恢复同步 |

示例：`docs/config-example.json`、`docs/clients-example.json`。  
旧版 `%APPDATA%\OhMyCopy\...` 或 `config.toml` 会在首次启动时尽量迁移到 exe 目录。

默认端口：**TCP/UDP 3721**。连接失败时请放行该端口。

### 测试

```bash
cargo test --release --tests --lib
# 或
powershell -ExecutionPolicy Bypass -File scripts/run_auto_tests.ps1
```

覆盖：协议/鉴权/引擎、hub 双节点（文本/图/文件/文件夹/大文件/ignore/unpair）、配置与历史。  
大文件：`$env:OHMYCOPY_E2E_LARGE_MB=20`。  
本机↔虚拟机真剪贴板：`python scripts/vm_ssh_smoke.py`（见 [docs/auto-test.md](docs/auto-test.md)）。

### 发布流程（便携版）

当前**没有安装器/商店流水线**，发布物是「单 exe + 说明」便携包。

| 步骤 | 做什么 | 产物位置 |
|------|--------|----------|
| 1. 开发构建 | `cargo build --release` | **`target/release/ohmycopy.exe`**（编译输出，不是 dist） |
| 2. 测试 | `scripts\run_auto_tests.ps1`（可选配 VM） | 无发布物 |
| 3. 打包 | `scripts\package_release.ps1` | **`dist/OhMyCopy-<版本>/`** + **`dist/OhMyCopy-<版本>.zip`** + `dist/ohmycopy.exe` |
| 4. 分发 | 把 zip 拷给用户，或挂到 GitHub Releases | 用户解压后运行 `ohmycopy.exe` |

```powershell
# 一键：测一遍 + release 构建 + 写入 dist/
powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1

# 已测过、只打包
powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1 -SkipTests
```

**GitHub Release（示例）：**

```powershell
# 需已安装 GitHub CLI (gh) 并登录
gh release create v0.1.36 dist/OhMyCopy-0.1.36.zip --title "v0.1.36" --notes-file CHANGELOG.md
```

**为什么默认看不到 dist 里的 exe？**

1. `cargo build` **只会**写到 `target/release/`，**不会**自动拷到 `dist/`。  
2. 清理垃圾时 `dist/` 被清空，只保留占位文件 `dist/.gitkeep`。  
3. `.gitignore` 忽略 `dist/*`（除 `.gitkeep`），避免把运行配置、inbox、exe 提交进 Git。  
4. 需要发布目录时，请跑上面的 **`package_release.ps1`**。

图标：`assets/ohmycopy.ico` 编进 exe；窗口/托盘用 `assets/icon.png`、`tray.png`。

## 技术栈

Rust · tokio · egui/eframe · Argon2id · ChaCha20-Poly1305 · BLAKE3 · postcard · SQLite · arboard · zip · tray-icon · image

## 许可

[MIT](LICENSE)
