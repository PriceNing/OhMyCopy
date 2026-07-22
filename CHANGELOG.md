# Changelog

## 0.1.41 — 2026-07-22

### 修复

- 多选文件、多个文件夹及二者混合复制时，现会合并为单个加密同步载荷；接收端解包后一次写入文件列表剪贴板，可直接粘贴全部项目
- 多项目打包与解包继续应用现有 ZIP 路径、条目数、目录深度及解压大小限制

## 0.1.40 — 2026-07-20

### 修复

- Windows 开机自启：注册表 Run 不再写入 `\\?\` 扩展路径（会导致登录不启动、任务管理器「打开文件位置」无反应）
- 启动时若自启项路径失效/前缀错误会自动重写为当前 exe 的普通路径

## 0.1.39 — 2026-07-19

### 修复

- Linux：接入 arboard `text/uri-list` 读写文件列表（此前误报「平台不支持」）
- 历史「复制」：仅当 content 为**绝对路径**且仍存在时才按文件处理，避免把「文件名」纯文本误判为文件
- 放入系统剪贴板失败时回退为路径文本，不再只弹平台不支持

## 0.1.38 — 2026-07-19

### 修复

- Linux headless：OS 剪贴板不可用时（X11 unreachable）重试/重建 arboard，并回退 `wl-copy`/`xclip`/`xsel`
- 剪贴板写入失败时仍写入历史，并把文本/图片落到 `~/.ohmycopy/last_clip/`
- headless 启动探测剪贴板并打印可操作提示（DISPLAY / xclip）
- arboard 启用 `wayland-data-control`

## 0.1.37 — 2026-07-19

### 修复

- Linux：系统托盘创建前调用 `gtk::init`，避免 `GTK has not been initialized` 整进程 panic
- Linux：托盘创建失败时软降级（无托盘仍可开主窗口）；release 改用 `panic = unwind` 以捕获托盘 panic
- Linux：主循环中泵送少量 GTK 事件，改善托盘菜单响应
- 文档：说明 Debian 需 `libxdo3` 等依赖；发布包可捆绑 libxdo

## 0.1.36 — 2026-07-18

### 功能

- 内置多语言（`en_us` / `zh_cn`）：源文件在 `languages/*.lang`，编译期 `include_str!` 嵌入，单 exe 无需外置语言包
- 设置页语言下拉热切换，立即写入 `config.language`；启动顺序：配置 → 系统 locale → English
- 缺 key 回退 English；GUI / headless / 托盘 / 状态与 toast 共用同一 i18n 路径

### 修复 / 统一

- headless 工作目录文案与 GUI 一致（`~/.ohmycopy`）；共享 `run_with_config` 同步管线不变

## 0.1.35 — 2026-07-18

### 安全与稳健

- 握手帧硬顶 64KiB（未鉴权阶段不再可声明 512MiB 撑爆内存）
- `AuthResponse.device_id` / `device_name` 必须与 Hello 一致
- 文件夹 zip 解压限制条目数/深度/未压缩总量，失败清理 receipt
- 新安装随机密码；`change-me`/空密码禁止配对与同步
- 保存设置：密码热更新；端口明确需重启；设置页强提示

### 质量

- `cargo clippy --lib -- -D warnings` 通过；`FrameType::Unpair`

## 0.1.34 — 2026-07-18

### 功能

- 文本 / 图片（含 CF_DIB、PNG）/ 文件（CF_HDROP）/ 文件夹（zip 打包同步）
- 互配对与解除配对（`clients.json`）；忽略（静音）不删连接
- UDP 发现仅展示，鉴权成功后才写入 clients；自动重连
- 星型中继：收到远端事件在 `event_id` 去重后可转发给其他已连接节点
- 历史预览短文件名；inbox 时间戳收据目录；大载荷动态 IO 超时
- 托盘：左键显示窗口 / 右键菜单；`start_minimized_to_tray`、`console`
- headless 模式；协议 v2（Hello 含 `listen_port`，`Unpair` 消息）

### 测试与工程

- hub e2e：文本/图/文件/文件夹/大文件/反向/ignore/unpair/鉴权失败
- `clip_probe` 示例；`scripts/vm_ssh_smoke.py` 本机↔VM 实机剪贴板
- 配置主格式 **JSON**（`config_version: 2`），遗留 TOML 可迁移

## 0.1.0 — 2026-07-16

### 首个可用版本

- 局域网对等文本剪贴板同步（Windows 可运行；Linux 同源代码）
- TCP 数据面 + 共享密码握手 + AEAD 会话
- UDP 广播发现与手动添加对端
- 每对节点单连接（device_id 去重）
- event_id 已见集 + 同步写入抑制回环
- egui 深色玻璃风 UI：历史 / 设备 / 设置
- SQLite 本地历史
- 可配置载荷大小上限、暂停同步
