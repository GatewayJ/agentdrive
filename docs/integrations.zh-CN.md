# Agent 集成与 Dashboard

[English](integrations.md) | [简体中文](integrations.zh-CN.md)

启动 `agentfsd` 并配置 `AGENTFS_TOKEN_FILE`。原生目录需要 FUSE、macFUSE 或 WinFsp。挂载路径必须位于 daemon 配置的 `mount_roots` 中。Agent 的执行环境需要能够访问该目录。

## MCP 与挂载目录

在已有 Agent 中配置 stdio MCP 命令 `agentfs mcp`，或者连接 daemon 的 `/mcp` HTTP 地址。MCP 连接复用长期运行的 daemon。

```sh
agentfs mount --workspace "$WORKSPACE" "$AGENTFS_DIRECTORY/mounts/project"
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

同一路径和参数重复挂载会返回已有 binding，多个 Agent 可以共享目录。共享目录中的文件修改与分支切换影响全部使用者。一个分支最多有一个可写挂载；更换目录时，需要显式卸载原目录。卸载和 Agent 断开都保留本机分支选择。需要隔离工作内容时，通过 CLI 或 MCP 显式创建分支。

MCP 返回 `result.binding`，其中包含 `id`、`branch`、`path` 和 `generation`。Agent 使用返回路径执行文件操作。MCP 连接本身不会改变 Agent 的工作目录或文件访问权限。保存当前挂载可调用 `mount_save`，参数为 `mount` 和 `expected_binding_generation`；保存请求不会创建新分支。

## 应用启动命令

需要由协议集成程序设置 Agent 工作目录并在任务结束时保存，可以配置以下启动命令：

```sh
agentfs integrate --protocol acp --workspace "$WORKSPACE" \
  --mount-path "$AGENTFS_DIRECTORY/mounts/project" -- your-acp-agent
```

```sh
agentfs integrate --protocol codex-app-server --workspace "$WORKSPACE" \
  --mount-path "$AGENTFS_DIRECTORY/mounts/project" -- codex app-server --listen stdio://
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

`agentfs branch create` 和 MCP `branch_create` 显式创建分支，保持原有选择。`agentfs branch switch` 和 MCP `branch_switch` 保存原分支并更新当前 location 的 workspace 选择。CLI 会尝试释放已有挂载；目录被占用时，需要关闭文件句柄和使用目录的程序，再使用原 request ID 重试。MCP 调用方通过 `mount_release` 继续等待中的切换。

旧数据库保留已有分支和选择。没有选择且只有一个可写本机分支时自动复用；存在多个可写本机分支时，使用 `branch_switch` 明确选择。

运行 `agentfs dashboard --open` 打开 daemon 页面。浏览器从 URL fragment 读取令牌，随后移除 fragment，将令牌保存在 session storage 中。也可以直接打开地址并输入 daemon token。页面每 15 秒刷新，展示分支创建关系、版本历史和 commit 信息。点击分支只改变页面展示的历史。

只读 JSON 接口为 `GET /api/workspaces` 和 `GET /api/workspaces/{workspace}`。后者返回 `status` 和 `revisions`，其中 `selected_branch` 是当前 location 的选择。管理员通过 `/settings` 编辑运行配置，详见[配置说明](configuration.zh-CN.md)。

协议参考：[ACP 会话](https://agentclientprotocol.com/protocol/v1/session-setup)、[ACP 回合](https://agentclientprotocol.com/protocol/v1/prompt-turn)、[Codex app-server](https://developers.openai.com/codex/app-server/)。
