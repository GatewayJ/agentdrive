# Agent 集成与 Dashboard

[English](integrations.md) | [简体中文](integrations.zh-CN.md)

启动 `agentfsd`，配置 `AGENTFS_TOKEN_FILE`，并创建工作区。原生目录需要 FUSE、macFUSE 或 WinFsp。挂载路径必须是绝对路径，并位于 daemon 配置的 `mount_roots` 中。集成程序与 daemon 使用同一台机器和同一个操作系统账户。

## 应用启动命令

将 ACP 客户端的 agent 命令配置为：

```sh
agentfs integrate --protocol acp --workspace "$WORKSPACE" \
  --session-id editor-project --mount-path "$AGENTFS_DIRECTORY/mounts/project" \
  -- your-acp-agent
```

将通过 stdio 启动 Codex app-server 的客户端命令配置为：

```sh
agentfs integrate --protocol codex-app-server --workspace "$WORKSPACE" \
  --session-id codex-project --mount-path "$AGENTFS_DIRECTORY/mounts/project" \
  -- codex app-server --listen stdio://
```

应用与集成程序交换逐行 JSON-RPC 消息。重连时保留相同的 `--session-id`；ACP 或 Codex 的对话 ID 仍由应用管理。每个集成程序关联一个 OpenCSG Workspace 会话，同一时间允许一个活动回合。同一操作系统账户通过会话文件锁防止重复启动。标准输出传递协议消息，诊断信息写入标准错误输出。

集成程序关联 OpenCSG Workspace 会话，等待挂载就绪后启动 agent。ACP 的 `session/new`、`session/load`、`session/resume` 和 Codex 的 `thread/start`、`thread/resume`、`thread/fork`、`turn/start` 会使用挂载目录作为 `cwd`。新会话使用工作区当前选择的分支，已有会话恢复关联分支；显式切换工作区分支会更新这些关联。

## 保存与恢复

ACP 的 `session/prompt` 开始记录回合，结果保存成功后才向应用转发响应。`end_turn`、`max_tokens`、`max_turn_requests`、`refusal` 记录完成，`cancelled` 记录取消，RPC 错误记录失败。`session/cancel` 转发给 agent，收到 prompt 的结束结果后保存。

Codex 的 `turn/start`、`review/start` 在转发前开始记录。`turn/started` 关联服务端 turn ID；`turn/completed` 在转发前保存 `completed`、`failed`、`interrupted` 结果。协议中断记录为取消。agent 异常退出或应用断开时，保存尚未结束的回合并标记 `interrupted`，暂停会话，再卸载目录。

自动保存继续使用五秒静默期和三十秒连续写入期限。生命周期 HTTP 请求在重试时保持 request ID，响应正文接收失败也会重试。连接内重复的处理中请求只转发一次，近期已完成响应通过有容量限制的缓存重放。重复结束通知保留原回合结果。复用已缓存的 RPC ID 并改变参数会返回错误。建立新连接后提交的请求按新的协议请求处理。

重连会恢复会话，将上一次未结束的回合保存为中断。daemon 重启后的恢复使用持久保存的回合 ID。保存失败时向客户端报告错误。占用挂载的文件句柄需要关闭后才能重新关联。

自定义应用可调用经过 Bearer 身份验证的 `POST /api/lifecycle`，参数为 `{request_id, event, key, ...}`。`key` 使用标准 `SessionKey`。`attach` 需要 `mount_path`，`begin` 需要 `turn_id`，`end` 需要 `turn_id` 和终止状态 `status`，`detach` 用于断开。重试保留 request ID。回合结束确认本地保存，远程发布由 daemon 原有后台任务执行。`turn_begin`、`turn_end` 保留为内部服务操作，MCP 工具目录仅提供应用需要的其他操作。

## 分支与已有数据

`agentfs branch create` 和 MCP `branch_create` 使用当前工作内容或指定版本创建分支，并保持工作区原有选择。`agentfs branch switch` 和 MCP `branch_switch` 保存原分支并更新持久化选择。每个分支同时允许一个活动回合。切换前活动回合需要完成；CLI 会立即尝试释放挂载。挂载被占用时，保留等待中的切换操作；关闭文件句柄和使用目录的程序后，使用原 request ID 重试。MCP 调用方通过 `mount_release` 继续等待中的切换。等待中的切换操作完成或取消后，才能开始新的回合。

新工作区默认选择可写的 `main`。已有数据库保留原来的分支保护设置。旧工作区没有保存过当前分支时，需要显式选择可写分支，再打开默认会话；可以先使用 `--source <REVISION>` 或 `--source-branch <BRANCH>` 创建分支。旧版本对象仍然可以读取，commit 说明为可选字段。新的本地分支记录创建来源，旧分支和远程接管的分支可能没有来源信息。

## Dashboard

运行 `agentfs dashboard --open` 打开 daemon 的根地址。浏览器从 URL fragment 读取令牌，随后立即移除 fragment，将令牌保存在 session storage 中。也可以直接打开地址并输入 daemon token。数据访问沿用工作区权限。页面每 15 秒刷新，提供工作区选择、分支创建关系、版本历史和 commit 信息。点击分支只改变页面展示的历史。

只读 JSON 接口为 `GET /api/workspaces` 和 `GET /api/workspaces/{workspace}`。后者返回 `status` 和 `revisions`。版本信息显示创建该版本的原始分支，父版本 ID 保留合并关系。

协议参考：[ACP 会话](https://agentclientprotocol.com/protocol/v1/session-setup)、[ACP 回合](https://agentclientprotocol.com/protocol/v1/prompt-turn)、[Codex app-server](https://developers.openai.com/codex/app-server/)。
