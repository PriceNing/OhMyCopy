# OhMyCopy

轻量级**局域网**跨设备剪贴板同步（Windows / Linux）

- 对等架构：无中心服务器，每台既监听也连接  
- TCP 加密同步 + UDP 广播发现  
- 共享密码鉴权（Argon2id + ChaCha20-Poly1305）  
- egui 原生 UI（扁平 + 毛玻璃风格）  
- 目标：可靠、体积小、低内存  

规格见 [docs/开发文档.md](docs/开发文档.md)（**v0.2**）。

## 功能状态（v0.1.0）

| 能力 | 状态 |
|------|------|
| 文本剪贴板同步 | ✅ |
| 密码鉴权 + 会话加密 | ✅ |
| 每对单 TCP + device_id tie-break | ✅ |
| event_id 去重 / 同步写入不外发 | ✅ |
| UDP 设备发现 | ✅ |
| 手动添加 IP:Port | ✅ |
| 历史记录（SQLite） | ✅ |
| 设置页 / 暂停同步 | ✅ |
| 防火墙失败提示 | ✅ |
| 图片 / 文件同步 | ⏳ 后续（架构已预留 kind） |

## 快速开始

### 构建

```bash
cargo build --release
# 产物: target/release/ohmycopy.exe  (Windows)
# 体积约 5MB（当前 release 配置 strip + LTO）
```

### 运行

1. 在两台（或多台）局域网电脑上启动同一版本 OhMyCopy  
2. **设置**中填写相同的**共享密码**并保存  
3. 等待 **设备**页出现「附近设备」，点 **连接**，或手动添加 `IP:3721`  
4. 一侧复制文本，另一侧应在约 1 秒内可粘贴  

#### 无显卡 / 无 OpenGL 的机器（服务器、精简系统）

若 GUI 报 `OpenGL 2.0+` / `no suitable adapter`，**0.1.6+ 会自动进入无界面模式**，同步仍可用：

```bash
# 显式无界面（可选）
ohmycopy.exe --headless
# 或
set OHMYCOPY_HEADLESS=1
ohmycopy.exe
```

在有界面的电脑上，把该机器 IP 填进「手动添加」并连接即可。  


配置文件放在 **exe 同目录**（便携布局，方便拷贝/备份）：

```text
OhMyCopy/
  ohmycopy.exe
  config.json      # 本机名称、端口、共享密码、console 等
  clients.json     # 客户端列表；auto_connect=true 启动时自动连接
  history.db       # 剪贴板历史
```

`config.json` 中 **`"console": false`**（默认）启动后不显示黑色控制台；需要日志时设为 `true`。

示例见 `docs/config-example.json`、`docs/clients-example.json`。  
旧版 `%APPDATA%\OhMyCopy\...` 或 `config.toml` 会在首次启动时尽量迁移到 exe 目录。

默认端口：**TCP/UDP 3721**。若连接失败，请在系统防火墙放行该端口。

### 测试

```bash
cargo test
# 或一键脚本（含 hub 端到端）
powershell -ExecutionPolicy Bypass -File scripts/run_auto_tests.ps1
```

包含：协议编解码、鉴权与 AEAD、引擎去重、历史库、双节点 localhost 同步（文本/图片/文件）。  
可选大文件：`OHMYCOPY_E2E_LARGE_MB=20`。对接虚拟机见 [docs/auto-test.md](docs/auto-test.md)。

### 发布包

构建后可复制：

```text
dist/ohmycopy.exe
```

## 技术栈

Rust · tokio · egui · Argon2id · ChaCha20-Poly1305 · BLAKE3 · postcard · SQLite · arboard

## 许可

MIT
