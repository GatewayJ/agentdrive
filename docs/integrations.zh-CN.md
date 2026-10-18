# Agent 集成与 Dashboard

[English](integrations.md) | [简体中文](integrations.zh-CN.md)

启动 `agentdrived` 并配置 `AGENTDRIVE_TOKEN_FILE`。原生目录需要 FUSE、FUSE-T/macFUSE 或 WinFsp。挂载路径必须位于 daemon 配置的 `mount_roots` 中。Agent 的执行环境需要能够访问该目录。

## MCP 与挂载目录

在已有 Agent 中配置 stdio MCP 命令 `agentdrive mcp`，或者连接 daemon 的 `/mcp` HTTP 地址。MCP 连接复用长期运行的 daemon。

```sh
agentdrive mount --workspace "$WORKSPACE" "$AGENTDRIVE_DIRECTORY/mounts/project"
```

对应的 MCP 工具为 `mount_prepare`，最少参数为：

```json
{
  "request_id": "mount-project-1",
  "request": {
    "workspace": "<workspace-id>",
    "path": "/absolute/path/to/project"
  }
}
```

`location_id` 属于 daemon 的数据目录，重启后保持不变。同一 location 重复挂载同一 workspace 时，复用本机选择的分支。新 location 首次挂载时，自动从该 workspace 主分支最近发布的保存版本创建 `location-<location-id>` 分支并保存选择。已配置 S3 时，可以直接指定远程 workspace ID，程序会读取其元数据。创建 workspace 的 location 使用已经选中的 `main`。主分支缺失或尚未发布时，请求返回错误。

同一个 principal 使用相同路径和参数重复挂载会返回已有 binding，多个 Agent 可以共享目录。其他 principal 即使具有 `manage` 权限，也无法复用该 binding。共享目录中的文件修改与分支切换影响全部使用者。一个分支最多有一个可写挂载；更换目录时，需要显式卸载原目录。卸载和 Agent 断开都保留本机分支选择。需要隔离工作内容时，通过 CLI 或 MCP 显式创建分支。

MCP 返回 `result.binding`，其中包含 `id`、`branch`、`path` 和 `generation`。Agent 使用返回路径执行文件操作。MCP 连接本身不会改变 Agent 的工作目录或文件访问权限。保存当前挂载可调用 `mount_save`，参数为 `mount` 和 `expected_binding_generation`；保存请求不会创建新分支。

## 转换已有目录

调用 `workspace_adopt_directory`，默认创建以目录名称命名、调用者拥有的 workspace，原目录内容作为 `main` 的初始 revision，并挂载到原路径。提供 `request.workspace` 时，导入指定 workspace 的新 branch，调用者需要该 workspace 的 `manage` 权限。路径必须位于 `mount_roots` 的子目录中，同时处于 `directory_roots` 允许范围内，父目录必须存在，并且原生挂载功能可用。目标路径及其父目录、子目录中已有的挂载需要提前卸载。

调用前暂停全部文件写入。传入 `restart_agent: true`，确认转换后必须重启 Agent 及其终端。工具通过参数和返回结果要求重启，进程的终止与启动由调用方执行。旧进程的工作目录引用和文件句柄可能继续指向备份目录，转换后应停止通过原 Agent 访问文件。

```json
{
  "request_id": "adopt-project-1",
  "request": {
    "path": "/absolute/path/to/project",
    "restart_agent": true
  }
}
```

工具按照 `workspace_import` 的规则校验并导入目录，然后将原目录移动到旁边的 `.agentdrive-backup-<operation-id>/original`，再将导入的 branch 挂载到原路径。包含 symbolic link 等不支持内容时，导入失败，原目录保持原位。指定 workspace ID 时，原有内容和默认分支选择保持不变，新 branch 显式挂载到 `path`。

成功时 `phase` 为 `complete`、`mount_ready` 为 `true`，且没有 `error`。操作记录的 `workspace` 返回 workspace ID。结果包含 `branch`、`path`、`backup_path`、`binding` 和 `restart_required: true`。重启 Agent 时继续使用原路径作为工作目录，后续显式挂载使用返回的 branch ID。确认重启后的 Agent 能正常访问文件后，可以自行清理备份。

WinFsp 会创建目录挂载点。Windows 上，AgentDrive 在启动驱动前移除经过检查的空目录；启动失败时恢复该目录。已有文件和 reparse point 会被拒绝，导入的原始文件继续保存在备份目录中。挂载成功后使用原路径。参见 [WinFsp 目录挂载实现](https://github.com/winfsp/winfsp/blob/v2.1/src/dll/mount.c#L439)。

移动目录后如果挂载失败，原文件保存在 `backup_path`，原路径可能为空。暂停目录访问，使用相同 request ID 和相同参数重试；daemon 重启后也支持继续处理，并复用已导入的 branch。响应丢失时通过 `operation_get` 或 `operation_by_request` 查询结果。导入阶段失败时，修正源目录后使用新的 request ID。已经移动的路径应继续原请求。

## 应用启动命令

需要由协议集成程序设置 Agent 工作目录并在任务结束时保存，可以配置以下启动命令：

```sh
agentdrive integrate --protocol acp --workspace "$WORKSPACE" \
  --mount-path "$AGENTDRIVE_DIRECTORY/mounts/project" -- your-acp-agent
```

```sh
agentdrive integrate --protocol codex-app-server --workspace "$WORKSPACE" \
  --mount-path "$AGENTDRIVE_DIRECTORY/mounts/project" -- codex app-server --listen stdio://
```

应用与集成程序交换逐行 JSON-RPC 消息。集成程序复用挂载，等待目录就绪后启动 Agent。ACP 的 `session/new`、`session/load`、`session/resume`，以及 Codex 的 `thread/start`、`thread/resume`、`thread/fork`、`turn/start` 使用该目录作为 `cwd`。协议中的对话 ID 由应用管理。

## 保存与恢复

文件修改暂停约五秒，或连续修改约三十秒，会触发自动保存。daemon 每秒检查一次。保存包含共享分支当时的文件内容。

集成程序在开始任务时查询当前挂载 binding，在完成、取消、失败、Agent 退出或连接中断时调用保存。正常结束响应在本地保存成功后转发。保存失败会向调用方报告错误。请求重试保留 request ID；重复的协议请求复用已完成响应。断开连接后保留共享挂载。

分支切换和恢复可能更新 binding ID 或 generation，迟到的保存请求返回 `STALE_BINDING`。集成程序在下一次任务开始时重新查询当前 binding。daemon 重启后重新连接会恢复已保存内容并检查原生挂载，尚未达到保存时刻的修改可能丢失。

自定义应用可以调用经过 Bearer 身份验证的 `POST /api/lifecycle`：

| event | 参数 | 行为 |
| --- | --- | --- |
| `attach` | `request_id`, `workspace`, `mount_path` | 准备或复用挂载，返回 `binding` 和 `mount_path`。 |
| `check` | `request_id`, `workspace`, `mount_path` | 查询该目录当前可写的 binding。使用 attach 返回的规范化路径。 |
| `save` | `request_id`, `mount`, `expected_binding_generation` | 保存指定挂载版本的内容，返回操作记录。 |

远程发布由 daemon 的后台同步任务执行。只有 `remote_confirmed` 才表示该次修改已经获得远程确认。

## 分支与 Dashboard

`agentdrive branch create` 和 MCP `branch_create` 显式创建分支，保持原有选择。`agentdrive branch switch` 和 MCP `branch_switch` 保存原分支并更新当前 location 的 workspace 选择。CLI 和 MCP 均在一次调用中完成切换。遇到 `MOUNT_BUSY`，关闭文件和使用目录的进程，或重启 Agent，再使用相同 request ID 重试 `branch_switch`。其他错误保留具体原因。

旧数据库保留已有分支和选择。没有选择且只有一个可写本机分支时自动复用；存在多个可写本机分支时，使用 `branch_switch` 明确选择。

运行 `agentdrive dashboard --open` 打开 daemon 页面。浏览器从 URL fragment 读取令牌，随后移除 fragment，将令牌保存在 session storage 中。也可以直接打开地址并输入 daemon token。页面每 15 秒刷新，展示分支创建关系、版本历史和 commit 信息。点击分支只改变页面展示的历史。

只读 JSON 接口为 `GET /api/workspaces` 和 `GET /api/workspaces/{workspace}`。后者返回 `status`、`revisions`、`summary` 和 `origins`。`summary` 包含 `branch_count`、`mount_count`、`ready_mount_count`、`mounted_branch_count`；`status.mounts` 仅保留未释放记录，并按路径排序。`origins` 为每个 branch 提供来源 branch、准确的来源 revision、版本是否可读和工作区根分支标识。`selected_branch` 是当前 location 对此工作区的默认选择。管理员通过 `/settings` 编辑运行配置，详见[配置说明](configuration.zh-CN.md)。

协议参考：[ACP 会话](https://agentclientprotocol.com/protocol/v1/session-setup)、[ACP 回合](https://agentclientprotocol.com/protocol/v1/prompt-turn)、[Codex app-server](https://developers.openai.com/codex/app-server/)。

顶部显示当前工作区的工作分支数量、本机挂载点数量、被就绪挂载使用的不同分支数量和远程同步状态。挂载点统计排除已释放记录，准备中、暂停和待恢复记录保留并显示状态。分支图按 `source_branch` 与 `base_revision` 连接到创建来源版本，点击版本可查看信息；来源版本可能是 commit、自动保存或检查点。“默认”表示本机未指定 branch 时的选择，“已挂载”显示就绪目录数量。挂载目录表列出完整路径、对应分支、读写模式和状态，点击分支可查看其历史。

分支提示分为“有未保存修改”（`mutation_seq != saved_seq`）、“已自动保存 · 有未提交修改”（序号相同且 `dirty` 为真）和“已提交”（序号相同且 `dirty` 为假）。已保存表示内容已有本地持久化检查点；检查点已保存、自动保存版本尚未生成时，悬浮说明会解释此状态。远程确认通过同步卡片展示，停止写入的状态单独显示。

版本历史仅展示当前所选分支创建的版本。查看来源分支的历史时，点击来源分支。尚未创建自身版本的分支显示空历史。

版本历史每行右侧提供 **更多（⋯）** 菜单。**版本信息** 在弹框中显示版本属性；**修改内容** 在弹框中显示分页修改文件列表，默认选中第一项并在右侧展示详情与文本差异。加载更多时保留当前选择。可以通过关闭按钮、Escape 或点击弹框外部关闭。自动与父版本比较；合并版本使用第一父版本，初始版本与空目录比较。“上一版本”按父子关系确定。内容完全相同且来源、目标唯一的移动显示为重命名；存在多个匹配或同时修改内容的移动显示为新增、删除。新增、删除的空文件明确显示为空文件（0 字节）；重命名或属性修改中的相同内容单独说明。二进制和超出预览上限的文件显示元数据。浏览版本保持现有挂载和工作分支。

版本查询接口为 `GET /api/workspaces/{workspace}/revisions/{revision}/changes` 和 `GET /api/workspaces/{workspace}/revisions/{revision}/diff`，均要求 bearer 认证与工作区读取权限。`changes` 接受 `before`、`cursor`、`limit`；`diff` 接受 `before`、`path`、`previous_path`。省略 `before` 表示与空目录比较。参数及返回字段见 [MCP 文档](mcp.zh-CN.md)。

## macOS FUSE-T

安装 FUSE-T 1.2.9 和 pkg-config，使用 `make -j "$(sysctl -n hw.ncpu)" build FEATURES=macos-fuse-t` 编译。AgentDrive 使用本机 NFS 后端并关闭属性缓存，无需内核扩展。FUSE-T 的[许可条款](https://github.com/macos-fuse-t/fuse-t)要求商业使用或随商业软件分发时取得商业许可。

macOS 可能生成 AppleDouble `._` 元数据文件，AgentDrive 会将这些文件作为普通文件保存。`branch_switch` 在一次调用中尝试正常卸载和重新挂载。运行中的 Agent、终端或 MCP stdio 代理可能占用目录。遇到 `MOUNT_BUSY`，退出相关进程或重启 Agent，通过目录之外的进程使用相同 request ID 重试 `branch_switch`。客户端允许时，将 MCP 代理的工作目录设置在挂载目录之外。

文件写入和 `fsync` 遵循 binding 的 durability 设置。默认 `local` 设置下，使用 `mount_save` 的 `durability: "remote"` 并检查 `remote_confirmed`，确认内容已保存到 RustFS。仅凭本地文件写入成功无法确认远端保存。

2026-10-18 的 FUSE-T 1.2.9 NFS 探针使用带有 xattr 回调的镜像文件系统，比较 `noattrcache`、默认缓存及 `nonamedattr`。默认缓存将该负载的 GETATTR 次数从 2028 减至 454，但三种设置下，外部 chmod 的可见延迟均约为 4.5–4.8 秒；通过挂载路径执行 chmod 后立即可见。xattr 回调调用次数均为零，自定义属性通过 `._` 文件保存，探针中的 ResourceFork 写入失败。因此 AgentDrive 保留当前缓存设置和 AppleDouble 文件。仅增加 xattr 方法尚未证明能在该后端消除配套文件。[FUSE-T 开发文档](https://github.com/macos-fuse-t/fuse-t/wiki)说明属性缓存由客户端管理、FUSE TTL 不生效，NFS 不支持通知。这些结果仅适用于本次 NFS 后端测试。
