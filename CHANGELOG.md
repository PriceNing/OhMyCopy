# Changelog

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
