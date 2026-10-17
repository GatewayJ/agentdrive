# 验证

[English](validation.md) | [简体中文](validation.zh-CN.md)

通过多核心 Make 执行标准检查：

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" verify
make -j "$(getconf _NPROCESSORS_ONLN)" build
python3 tests/cli_smoke.py
```

流程测试覆盖并发请求重试、原子追加、持有打开句柄时重命名、重启恢复、挂载保存幂等性、并发挂载复用、不同 location 的分支独立性、自动保存时间、远端失败和确认丢失、所有权交接、合并文件编辑、过期挂载变更、缓存保留与回收，以及离线预取。持久化测试覆盖摘要失败、读取租约、事务回滚、请求恢复和未知格式拒绝。MCP 测试验证并发客户端、principal 隔离、身份验证，以及 Host/Origin 策略。CLI 测试启动实际进程，验证 stdio MCP、导入和导出的内容与元数据、允许的路径，以及进程重启。

## 原生文件系统

安装并启用平台驱动后执行：

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" test CARGO_FLAGS='-p agentdrive-platform --test native_mount -- --ignored --nocapture'
```

macOS 在 `CARGO_FLAGS` 中添加 `--features macos-fuse-t`。Windows 在 PowerShell 中使用 `make -j $env:NUMBER_OF_PROCESSORS`。集成测试管理自己的临时状态和挂载目录，覆盖大于 3 MiB 的文件、截断、随机位置写入、原子追加、重命名、fsync、超过 128 个目录项、历史版本只读访问，以及卸载。

WinFsp wrapper 的空指针卷 flush 回归测试在 Windows 上单独执行：

```powershell
make -j $env:NUMBER_OF_PROCESSORS test CARGO_FLAGS='--manifest-path vendor/winfsp_wrs/Cargo.toml --lib'
```

## S3 / RustFS

使用可清理的独立 endpoint，以及允许创建 `agentdrive-integration` bucket 的凭据。测试对象使用唯一前缀。请勿将这些测试连接到生产存储。

```sh
export AGENTDRIVE_TEST_S3_ENDPOINT=http://127.0.0.1:9000
export AWS_ACCESS_KEY_ID=AGENTDRIVETESTLOCAL
export AWS_SECRET_ACCESS_KEY=agentdrive-local-integration-secret-2026
make -j "$(getconf _NPROCESSORS_ONLN)" test CARGO_FLAGS='-p agentdrive-s3 -- --ignored --nocapture'
python3 tests/replica_smoke.py
python3 tests/storage_smoke.py
```

后端测试覆盖竞争条件下的引用创建、过期条件更新、空文件、摘要拒绝，以及 20 MiB 对象的分段发布。同步测试会推送导入的版本，在另一个 location 拉取并导出该版本，并通过实际 S3 后端交接分支所有权。启用原生驱动时，还会让三个 location 挂载同一工作区，验证自动导入工作区、从已发布根分支创建自有分支、重复挂载复用分支与关联，以及各自文件内容的独立性。

`storage_smoke.py` 验证 daemon PID 保持不变时的本地/S3 切换、凭据保存、切换失败恢复、配置文件修改、历史导出和重启恢复。

## 执行记录

| 检查 | 已验证环境 |
| --- | --- |
| Workspace 测试和 Clippy | macOS、Linux、Windows |
| 原生挂载文件操作和版本访问 | Linux root 用户与普通用户；安装 WinFsp 2.1 的 Windows；macOS 26.5 与 FUSE-T 1.2.9 |
| CLI、stdio MCP、目录元数据、重启恢复 | macOS、Linux、Windows |
| S3 条件写入和分段内容 | 隔离的 RustFS 实例 |
| 两个守护进程之间的 S3 同步和所有权交接 | macOS 和 Linux |
| macFUSE 回调编译 | macOS |

[GitHub Actions](https://github.com/GatewayJ/agentdrive/actions) 会为每次提交运行仓库内的测试矩阵。macOS 26.5 使用 FUSE-T 1.2.9 已通过两项原生挂载测试，覆盖原目录转换、独占创建、持有打开句柄时重命名、超过 128 个目录项、历史版本只读访问及正常卸载。目录项数量断言会排除 macOS 生成的 AppleDouble 元数据文件。

[CLI 使用](cli.zh-CN.md)中的创建、导入、读取、导出和挂载示例已通过临时守护进程执行验证。[MCP 参数表](mcp.zh-CN.md)已与运行中的守护进程返回的 30 个工具 schema 核对。S3 配置已根据守护进程配置加载器、适配器，以及锁定版本 `object_store` 的凭据和 endpoint 选择逻辑检查。上述文档检查不代表已经在 AWS S3 部署环境完成验证。

生命周期、分支及 Dashboard 的补充检查：

```sh
python3 tests/lifecycle_smoke.py
python3 tests/config_smoke.py
```

`lifecycle_smoke.py` 需要原生挂载支持，覆盖 ACP 和 Codex app-server 协议中的完成、取消、失败、进程中断、重复请求、自动保存、daemon 重启、重连、两个 Agent 连接共用挂载，以及已有挂载上的 CLI 分支切换。常规 Rust 测试还覆盖响应正文丢失后的重试、多个连接共用同一个挂载、切换时的文件句柄检查、分支选择持久化和 HTTP 权限。浏览器检查覆盖分支树、版本信息、令牌登录和小屏幕布局。本机 Codex app-server 的 initialize 与 thread/start 已验证工作目录指向 AgentDrive 挂载；完整任务流程通过协议测试程序验证。

`config_smoke.py` 使用临时目录运行 daemon，验证文件热更新、错误配置保留、启动配置修改提示、单个缓存容量字段的 HTTP 保存、版本冲突和重启恢复。该脚本无需挂载驱动。配置测试覆盖已有文件的读取和快照一致性；engine 缓存测试验证修改容量后按 75% 目标清理，并保留当前分支内容。

目录转换测试覆盖新 workspace 的 `main` 初始内容、指定 workspace 导入、Agent 重启确认、原目录备份、无效输入，以及挂载失败和 daemon 重启后的重试。

## FUSE-T 与 RustFS 本机验证（2026-10-17）

macOS 26.5、FUSE-T 1.2.9 与独立 RustFS 容器环境中，Codex 创建普通目录并在转换后的原路径继续写入。验证内容包含 12 MiB 文件、空文件、中文名称、创建、修改、重命名和删除。Codex 通过 stdio MCP 创建、切换分支并请求远端保存。main 与 feature 的版本在独立 daemon 中从 RustFS 恢复，全部文件清单和 SHA-256 一致。往返切换保持两个分支的独立内容。正常重启 daemon 后调用 `mount_prepare`，分支选择及文件内容保留。指定已有 workspace 的导入在原路径创建新的 branch。

停止 RustFS 后，本地保存成功、远端保存返回失败；恢复服务并同步后，原保存操作得到远端确认，独立恢复的全部文件校验一致。FUSE-T 构建通过 44 个 workspace 测试与 Clippy、两项原生挂载测试、56 个 fuser 测试，以及 CLI/stdio smoke 测试。原生测试耗时 134.14 秒。本次未验证多进程文件锁及生产环境吞吐量。生命周期与元数据行为见 [FUSE-T 使用说明](integrations.zh-CN.md#macos-fuse-t)。

## 一次调用完成分支切换

安装原生挂载驱动并完成支持挂载的构建后，执行：

```sh
python3 tests/branch_switch_smoke.py
```

stdio MCP 测试验证每次尝试只调用一次 `branch_switch`、进程工作目录占用、明确的 `MOUNT_BUSY` 恢复提示、重启 MCP 客户端后重试、已完成请求的重复调用，以及分支内容隔离。整个测试不调用 `mount_release`。引擎测试注入卸载权限失败和目标挂载失败，验证错误原因保留、相同请求可以继续执行。原生测试分别覆盖打开文件与工作目录占用。这些检查已在本机 macOS 26.5、FUSE-T 1.2.9 环境通过。

## 元数据与发布性能验证

本地查询升级测试覆盖旧 Node 数据迁移、统计增量更新、事务回滚和重复打开。workflow 测试覆盖目录移动、大小写与 Unicode 名称、无关节点保持不变、配额拒绝、截断及删除后的容量变化。上传测试覆盖跨发布操作的四个并发许可、失败清理和重试后的许可恢复。

执行可重复的 engine 基准：

```sh
make -j8 test CARGO_FLAGS='-p agentdrive-engine --test workflows metadata_scaling_benchmark -- --ignored --nocapture'
```

2026-10-17 在本机 macOS 上使用 debug 构建，将 `099fe55` 与此次修改运行相同测试。每个场景在其他目录放置指定数量的文件，然后对目标目录执行 100 次四字节写入、50 次目录读取和 50 次普通文件重命名。准备数据时间不计入结果。

| 无关文件数 | 写入，修改前/后 ms | 目录读取，修改前/后 ms | 重命名，修改前/后 ms |
| --- | --- | --- | --- |
| 100 | 103 / 50 | 38 / 11 | 395 / 30 |
| 1,000 | 563 / 38 | 286 / 10 | 3,714 / 30 |
| 10,000 | 5,487 / 33 | 2,797 / 11 | 37,951 / 29 |

这是一次本地 engine 与 SQLite 的对比测量，使用隔离临时目录，没有经过 FUSE、MCP 或 S3。结果说明这些操作已去除随无关节点数量增长的全量处理，不代表所有业务负载的加速比例，也不作为 CI 时间阈值。
