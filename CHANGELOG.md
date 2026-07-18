# Changelog

## 0.1.0 — 2026-07-16

### 首个可用版本

- 局域网对等文本剪贴板同步（Windows 可运行；Linux 同源代码）
- TCP 数据面 + 共享密码握手 + AEAD 会话
- UDP 广播发现与手动添加对端
- 每对节点单连接（device_id tie-break）
- event_id 已见集 + 同步写入抑制回环
- egui 深色玻璃风 UI：历史 / 设备 / 设置
- SQLite 本地历史
- 可配置载荷大小上限、暂停同步
- `cargo test` 全通过；release 体积约 5MB
