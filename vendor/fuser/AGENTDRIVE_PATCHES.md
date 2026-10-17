# AgentDrive 的 fuser 修改

来源：crates.io fuser 0.18.0。许可证见 LICENSE.md。

- INIT 回复长度依据协商后的协议版本，支持 FUSE-T 的 macOS v2 数据格式。
- `fuse-t` feature 在 macOS 使用完整消息读取，处理 stream socket 分段和连续消息。
- FUSE-T 使用不含 macFUSE 额外字段的 RENAME 请求格式。
- 同一个连接的回复串行发送，并处理部分写入。

其他平台保留设备文件读写方式。
