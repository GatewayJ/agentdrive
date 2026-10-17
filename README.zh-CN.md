# AgentDrive

**人与 Agent 的共享工作空间**

**共享资料，接力工作。**

[English](README.md) | [简体中文](README.zh-CN.md)

AgentDrive 为 Agent 提供可持久保存的工作区、显式管理的工作分支、不可变的版本记录，以及原生文件系统路径。长期运行的 Rust 守护进程负责管理本地状态和挂载；多个 MCP 客户端和 CLI 可以并发访问同一套经过身份验证的 API。

功能包括：

- 跨平台文件名称、按内容寻址的对象、分页的目录和 inode 索引，以及 SQLite 事务。
- Linux FUSE、macOS FUSE-T/macFUSE 和 Windows WinFsp 适配；快照、分支恢复和版本历史。
- S3/RustFS 同步、限制内存用量的流式传输、离线缓存保留、在线所有权交接，以及可以编辑文件的三方合并候选结果。
- ACP 和 Codex app-server 生命周期集成、分支命令，以及提供分支历史与运行配置管理的 Dashboard。
- 通过需要身份验证的 Streamable HTTP 提供 30 个具有明确类型的 MCP 工具，同时提供 stdio 代理和 JSON CLI。

## 构建

需要 Rust stable（声明的最低版本为 1.90）、用于编译内置 SQLite 的 C 编译器，以及 GNU Make。构建任务通过 Make 的 jobserver 管理并行度。

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" build
make -j "$(getconf _NPROCESSORS_ONLN)" verify
```

| 平台 | 原生文件系统要求 | 构建方式 |
| --- | --- | --- |
| Linux | FUSE 3 工具、`/dev/fuse` 和挂载权限 | 使用默认 features |
| macOS | [FUSE-T](https://github.com/macos-fuse-t/fuse-t) 和 `pkg-config` | `make -j "$(sysctl -n hw.ncpu)" build FEATURES=macos-fuse-t` |
| Windows | MSVC，以及包含 Developer 功能的 [WinFsp 2.1](https://github.com/winfsp/winfsp/releases/tag/v2.1) | PowerShell：`make -j $env:NUMBER_OF_PROCESSORS build` |

macOS 默认构建包含守护进程、CLI 和存储操作。原生挂载使用 `macos-fuse-t`，无需内核扩展。Make 会选择 FUSE-T 动态库；直接运行 Cargo 时需要设置 `PKG_CONFIG_PATH="$PWD/build/fuse-t"`。已有 macFUSE 环境可继续使用 `FEATURES=macos-mount`。Windows 构建会从注册的安装路径加载 WinFsp；Windows 状态目录应位于本地 NTFS 卷。

## 运行

```sh
AGENTDRIVE_DIRECTORY="$HOME/.local/share/agentdrive"
./target/debug/agentdrived init --directory "$AGENTDRIVE_DIRECTORY"
./target/debug/agentdrived serve --config "$AGENTDRIVE_DIRECTORY/agentdrive.json"
```

在另一个终端执行：

```sh
export AGENTDRIVE_TOKEN_FILE="$HOME/.local/share/agentdrive/token"
./target/debug/agentdrive tools
./target/debug/agentdrive call workspace_create --request-id create-demo --json '{"name":"demo"}'
./target/debug/agentdrive call workspace_list
```

`init` 会创建配置、随机 bearer token，以及状态、挂载和文件交换所需的目录。如果配置已经存在，初始化会拒绝执行，已有凭据会保留。守护进程默认监听 `127.0.0.1:7421`，MCP endpoint 为 `/mcp`。每个状态目录只能由一个守护进程使用。

通过 stdio 连接 MCP 客户端时，将可执行程序设置为 `agentdrive`，参数设置为 `["mcp"]`，并设置 `AGENTDRIVE_TOKEN_FILE`。可以通过 `AGENTDRIVE_ENDPOINT` 指定其他守护进程 endpoint。stdio 进程会将请求转发给正在运行的守护进程。

使用 `agentdrive tools` 查看 JSON 输入 schema。通过 MCP 调用命令工具时，请求格式为 `{ "request_id": "stable-id", "request": { ... } }`；CLI 的 `--request-id` 选项会构造该请求结构。查询工具和 `operation_cancel` 直接接收参数。`--json -` 从标准输入读取 JSON。工具响应没有错误时，CLI 返回退出码 0；工具返回错误时返回 2；传输或输入发生错误时返回 1；命令行参数解析错误也使用 2。可执行示例见 [CLI 使用](docs/cli.zh-CN.md)，完整工具参考见 [MCP 参数与能力](docs/mcp.zh-CN.md)。

## 分支、Agent 与 Dashboard

新工作区默认选择可写的 `main` 分支。同一 location 挂载同一 workspace 时复用本机所选分支；新 location 首次挂载时，从主分支最近发布的内容创建并选择自己的分支。通过 MCP 的 `branch_create`、`branch_switch` 或 CLI 显式创建和切换：

```sh
agentdrive mount --workspace "$WORKSPACE" "$AGENTDRIVE_DIRECTORY/mounts/project"
agentdrive branch list --workspace "$WORKSPACE"
agentdrive branch create --workspace "$WORKSPACE" --name feat/task
agentdrive branch switch --workspace "$WORKSPACE" --branch "$BRANCH"
agentdrive dashboard --open
```

创建分支会保存来源分支的当前内容，并保持工作区原有选择。切换会保存原分支，检查文件句柄，再更新本机分支选择和挂载目录。共享目录的多个 Agent 使用同一个分支。重启后仍然保留选择。Dashboard 展示分支关系、commit 说明、创建者、时间和父版本。管理员可以在“设置”页面修改缓存策略、S3 存储和 HTTP Origin 允许列表，运行中的 daemon 会使用更新后的配置，详见[配置说明](docs/configuration.zh-CN.md)。

已有 Agent 可以通过 `agentdrive mcp` 接入并使用挂载目录。`agentdrive integrate` 支持 ACP agent 和 Codex app-server，负责工作目录以及完成、取消和失败时的保存。自动保存由文件修改触发，Agent 断开后保留共享挂载。完整命令和事件行为见[Agent 集成](docs/integrations.zh-CN.md)。

需要并发保护的变更会检查分支 owner、epoch、generation 和 head。这些值应从 `workspace_status` 获取，请勿自行编造。需要停止当前挂载的操作会返回 `waiting_for_quiesce`。请停止使用该目录的进程，关闭相关文件句柄，并使进程退出该目录，然后携带 operation ID 和预期的 binding generation 调用 `mount_release`。详见[操作与恢复](docs/operations.zh-CN.md)。

## 存储与配置

`agentdrive.json` 使用绝对文件系统路径。`mount_roots` 指定允许的挂载目标；`directory_roots` 指定允许的导入和导出路径。导入期间源目录需要保持稳定，导入操作会拒绝 symbolic link 和不兼容的文件名称。导出操作会创建新目录，并保留文件内容、修改时间和支持的权限。

在 Dashboard“设置”中启用 S3，填写并保存连接和凭据，也可以在生成的配置中添加 `remote`：

```json
{
  "remote": {
    "bucket": "agentdrive",
    "prefix": "agentdrive/v1",
    "region": "us-east-1",
    "endpoint": "https://s3.example.com",
    "allow_http": false
  }
}
```

凭据可以通过 Dashboard 保存，也可以通过 `object_store` 支持的 AWS 环境变量或角色凭据提供程序配置。bucket 必须已经存在，并支持条件对象写入、带条件的分段上传完成操作，以及强一致读取。凭据权限应限制在选定的 bucket/prefix 内。本地开发 endpoint 可以明确启用 `allow_http`。

本地与 S3 存储可以在运行期间切换：启用 S3 时复制已保留的历史，返回本地模式时下载缺少的历史内容。准备或保存配置失败时，当前连接继续有效。`meta.db` 是本地 SQLite 数据库，不会作为文件上传。S3 保存文件内容、目录和 inode 索引、版本历史及分支引用；拉取时将已发布的记录导入每台机器自己的数据库。location 身份、分支选择、挂载、缓存状态和工作文件保存在本地。

全部配置字段、AWS S3/RustFS 示例、支持的凭据来源、环境变量优先级和多台机器配置见[配置与 S3](docs/configuration.zh-CN.md)。

S3 模式只需设置“本地缓存容量”，默认为 2 GiB，配置文件使用以字节为单位的 `cache.max_bytes`。后台每 30 秒检查一次，超过容量时自动清理到设置值的 75%；磁盘剩余空间不足时也会清理。当前分支、等待发布的数据、合并候选内容、活动读取者和固定缓存受到保护，实际占用可能超过设置容量。清理只处理已获得远端确认的历史对象。本地模式完整保留历史，页面隐藏缓存设置。容量统计针对不可变对象文件，SQLite 元数据和工作文件另外占用空间。`cache_status.offline_ready` 要求指定保留的数据范围已完整缓存在本地。

## 验证与功能范围

CI 会在 Linux、macOS 和 Windows 上运行 workspace 测试，并执行 Linux/Windows 原生挂载测试、macFUSE 回调编译检查、CLI/stdio 集成测试，以及连接实际 RustFS 服务的后端测试。原生挂载测试覆盖数 MiB 的文件写入、截断、追加、持有打开句柄时重命名、目录枚举、fsync、历史版本只读挂载和卸载。macOS 使用 FUSE-T 运行原生挂载验证。测试命令和当前验证记录见[验证文档](docs/validation.zh-CN.md)。

文件操作支持普通文件和目录、分支内原子追加、重命名、权限、时间戳、截断和 fsync。文件内容以整个文件为单位保存为不可变对象；编辑后的文件通过流式处理重新生成对象。分支和合并操作会在内存中构建元数据树。每个工作区可配置的 inode 数量上限为一百万；容量限制应根据可用内存和磁盘空间设置。

能力查询响应会说明支持的行为。当前接口不支持 symbolic link、hard link、扩展属性、内存映射写入、advisory locking、自动文本冲突解决和远端对象物理回收。删除已打开的文件，或替换已打开的目标文件，会返回 busy 错误。同一路径的并发变更会产生明确的合并冲突。挂载使用守护进程的操作系统身份，工作区访问授权控制 MCP 请求。

更多说明见[文档目录](docs/README.zh-CN.md)、[架构](docs/architecture.zh-CN.md)、[操作与恢复](docs/operations.zh-CN.md)和[验证](docs/validation.zh-CN.md)。AgentDrive 使用 Apache-2.0 许可证；仓库内附带的 WinFsp Rust wrapper 保留其 MIT 许可证。
