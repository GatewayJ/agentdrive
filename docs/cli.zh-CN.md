# 命令行使用

[English](cli.md) | [简体中文](cli.zh-CN.md)

## 启动守护进程

按照 [README](../README.zh-CN.md) 在仓库根目录构建。以下示例使用 POSIX shell 和 `jq` 构造 JSON、读取返回的 ID。请在每个终端中将构建后的程序加入 `PATH`：

```sh
export PATH="$PWD/target/debug:$PATH"
export AGENTDRIVE_DIRECTORY="$HOME/.local/share/agentdrive"
agentdrived init --directory "$AGENTDRIVE_DIRECTORY"
agentdrived serve --config "$AGENTDRIVE_DIRECTORY/agentdrive.json"
```

新目录只需运行一次 `init`。该命令会输出生成的配置路径，已有配置或 token 时会拒绝执行。`serve` 在前台运行，Ctrl+C 会请求关闭服务。S3 凭据可以通过 Dashboard 设置页面保存；选择环境凭据时，在当前终端运行 `serve` 前设置，详见[远端配置](configuration.zh-CN.md)。

| 命令 | 参数 |
| --- | --- |
| `agentdrived init` | 必填 `--directory <PATH>`；可选 `--listen <IP:PORT>`，默认 `127.0.0.1:7421`。 |
| `agentdrived serve` | 必填 `--config <PATH>`。 |
| `agentdrive tools` | 以 JSON 输出工具目录，包括 `inputSchema` 和 annotations。 |
| `agentdrive call <TOOL>` | 可选 `--json <JSON>`，默认 `{}`；可选 `--request-id <ID>`。 |
| `agentdrive mcp` | 启动 stdio MCP 代理，连接已有守护进程。 |
| `agentdrive mount` | 必填 `--workspace <ID>` 和目录；可选 `--request-id <ID>`。相对路径按 CLI 当前目录转换为绝对路径。 |

两个程序都提供 `--help` 和 `--version`，子命令提供 `--help`。

## 客户端连接与输出

连接参数放在**子命令前面**：

```sh
agentdrive --endpoint http://127.0.0.1:7421/mcp --token-file "$AGENTDRIVE_DIRECTORY/token" tools
```

| 参数 / 环境变量 | 行为 |
| --- | --- |
| `--endpoint` / `AGENTDRIVE_ENDPOINT` | 包含 `/mcp` 的完整 MCP URL，默认 `http://127.0.0.1:7421/mcp`。命令行参数覆盖环境变量。 |
| `--token-file` / `AGENTDRIVE_TOKEN_FILE` | 读取 token 文件，并去除首尾空白。命令行参数覆盖环境变量。 |
| `AGENTDRIVE_TOKEN` | 未选择 token 文件时使用。已选择的 token 文件无法读取时，会返回错误。 |
| `RUST_LOG` | 日志过滤设置，客户端默认 `warn`。日志写入 stderr。 |

守护进程需要已经启动。工具参数中的主机路径，例如导入来源和挂载路径，属于守护进程所在机器。`--token-file` 和标准输入属于客户端所在机器。

`tools` 输出包含 `tools` 数组的 MCP 工具目录对象。`call` 以 JSON 输出工具的 `structuredContent`。错误包含 `error.code`、`error.message` 和 `error.retryable`；操作结果记录可能同时包含已保存的结果和错误。对于操作，请分别检查 `phase`、`local_saved`、`remote_confirmed` 和 `mount_ready`。

工具响应没有错误时，客户端退出码为 0；工具返回错误时为 2；传输、身份验证、token 读取或 JSON 输入发生错误时为 1。命令行参数解析错误也使用退出码 2。`waiting_for_quiesce` 可能伴随退出码 0，请检查操作结果后再继续依赖该操作的任务。

`--json -` 从标准输入读取内容，上限为 8 MiB。HTTP 请求正文同样限制为 8 MiB，该限制包含 MCP 外层结构。`--request-id` 会将输入 JSON 包装为 `{ "request_id": "...", "request": { ... } }`。请将它用于 [MCP 参考](mcp.zh-CN.md)列出的命令工具。查询工具和 `operation_cancel` 直接接收参数。重试时保留相同的 request ID 和参数，新操作需要使用新的 request ID。

## 创建、导入、读取和导出

在另一个终端设置相同的 `PATH` 和 `AGENTDRIVE_DIRECTORY`。独立运行另一组示例时，请使用新的 `RUN_ID`、来源路径和导出路径。该示例可以在 macOS 默认构建上运行，无需原生挂载。

```sh
export AGENTDRIVE_TOKEN_FILE="$AGENTDRIVE_DIRECTORY/token"
RUN_ID=readme-demo-1
agentdrive tools > "$AGENTDRIVE_DIRECTORY/tools.json"
agentdrive call workspace_create --request-id "$RUN_ID-create" --json '{"name":"demo"}' > "$AGENTDRIVE_DIRECTORY/demo-create.json"
WORKSPACE="$(jq -r '.result.workspace.id' "$AGENTDRIVE_DIRECTORY/demo-create.json")"

jq -n --arg workspace "$WORKSPACE" '{workspace:$workspace}' |
  agentdrive call workspace_status --json - > "$AGENTDRIVE_DIRECTORY/demo-status.json"

mkdir -p "$AGENTDRIVE_DIRECTORY/exchange/source"
printf 'Hello from AgentDrive\n' > "$AGENTDRIVE_DIRECTORY/exchange/source/hello.txt"
jq -n --arg workspace "$WORKSPACE" --arg source "$AGENTDRIVE_DIRECTORY/exchange/source" '{workspace:$workspace,source:$source}' |
  agentdrive call workspace_import --request-id "$RUN_ID-import" --json - > "$AGENTDRIVE_DIRECTORY/demo-import.json"
REVISION="$(jq -r '.result.branch.formal_head' "$AGENTDRIVE_DIRECTORY/demo-import.json")"
BRANCH="$(jq -r '.result.branch.id' "$AGENTDRIVE_DIRECTORY/demo-import.json")"

jq -n --arg workspace "$WORKSPACE" --arg revision "$REVISION" '{workspace:$workspace,revision:$revision,path:"/hello.txt",offset:0,size:4096}' |
  agentdrive call revision_read --json -
jq -n --arg workspace "$WORKSPACE" --arg branch "$BRANCH" '{workspace:$workspace,branch:$branch}' |
  agentdrive call revision_list --json -

jq -n --arg workspace "$WORKSPACE" --arg revision "$REVISION" --arg destination "$AGENTDRIVE_DIRECTORY/exchange/export" '{workspace:$workspace,revision:$revision,destination:$destination}' |
  agentdrive call workspace_export --request-id "$RUN_ID-export" --json -
```

创建操作返回 `result.workspace`。导入会创建新分支并返回 `result.branch`，其中的 `formal_head` 是导入后的版本。新工作区默认选择可写的 `main` 分支。`revision_read` 返回 `{ "bytes": [0, 1, ...], "length": ... }`，文件内容表示为整数字节数组，单次请求的大小最多为 4 MiB。导出要求父目录已经存在，目标目录尚未存在。

## 挂载与分支

```sh
agentdrive mount --workspace "$WORKSPACE" "$AGENTDRIVE_DIRECTORY/mounts/project"
agentdrive branch list --workspace "$WORKSPACE"
agentdrive branch create --workspace "$WORKSPACE" --name feat/task --request-id "$RUN_ID-branch"
agentdrive branch switch --workspace "$WORKSPACE" --branch "$BRANCH" --request-id "$RUN_ID-switch"
agentdrive dashboard --open
```

`branch create` 接受可选的 `--source <REVISION>` 或 `--source-branch <BRANCH>`；省略时保存当前分支的工作内容作为来源。结果中的 `result.branch` 为新分支，工作区当前选择保持原值。`branch switch` 更新当前 location 的分支选择和挂载目录。CLI 和 MCP 均在一次调用中完成切换。遇到 `MOUNT_BUSY`，关闭文件和使用目录的进程，或重启 Agent，再使用相同 request ID 重试。其他错误保留具体原因。两个命令会生成 request ID；需要重试时可以提供固定的 `--request-id`。

`mount` 只需 workspace ID 和目录，重复调用复用相同挂载。同一 location 使用已有选择；新 location 首次挂载时从主分支最近发布的版本创建本机分支。配置 S3 后，可以直接挂载远程 workspace ID。一个分支最多有一个可写挂载，移动目录前需要卸载原挂载。Agent 断开后保留目录。[Agent 集成](integrations.zh-CN.md)说明 MCP 接入和任务保存。

`dashboard` 打印浏览器地址，无需读取凭据或连接 daemon。`dashboard --open` 需要凭据，通过 URL fragment 传递访问令牌；页面读取后立即移除 fragment，并将令牌保存在 session storage 中。数据请求使用 Bearer 身份验证。页面提供分支树和版本信息，每 15 秒刷新。 管理员可以通过左侧“设置”进入 `/settings`，修改运行配置并查看需要重启的启动配置，详见[配置说明](configuration.zh-CN.md)。

顶部显示当前工作区的工作分支数量、本机挂载点数量、被就绪挂载使用的不同分支数量和远程同步状态。挂载点统计排除已释放记录，准备中、暂停和待恢复记录保留并显示状态。分支图按 `source_branch` 与 `base_revision` 连接到创建来源版本，点击版本可查看信息；来源版本可能是 commit、自动保存或检查点。“默认”表示本机未指定 branch 时的选择，“已挂载”显示就绪目录数量。挂载目录表列出完整路径、对应分支、读写模式和状态，点击分支可查看其历史。

分支提示分为“有未保存修改”（`mutation_seq != saved_seq`）、“已自动保存 · 有未提交修改”（序号相同且 `dirty` 为真）和“已提交”（序号相同且 `dirty` 为假）。已保存表示内容已有本地持久化检查点；检查点已保存、自动保存版本尚未生成时，悬浮说明会解释此状态。远程确认通过同步卡片展示，停止写入的状态单独显示。

版本历史仅展示当前所选分支创建的版本。查看来源分支的历史时，点击来源分支。尚未创建自身版本的分支显示空历史。

双击版本条目，或聚焦条目后按 Enter／空格，打开 **版本详情** 弹框。上方展示版本信息，下方展示分页修改文件列表，默认选中第一项并在右侧展示详情与文本差异。分支图中的来源版本使用相同弹框。加载更多时保留当前选择。自动刷新会更新弹框中的当前正式提交 标记，并保留已选文件和已读取的差异。可以通过关闭按钮、Escape 或点击弹框外部关闭。自动与父版本比较；合并版本使用第一父版本，初始版本与空目录比较。“上一版本”按父子关系确定。内容完全相同且来源、目标唯一的移动显示为重命名；存在多个匹配或同时修改内容的移动显示为新增、删除。新增、删除的空文件明确显示为空文件（0 字节）；重命名或属性修改中的相同内容单独说明。二进制和超出预览上限的文件显示元数据。浏览版本保持现有挂载和工作分支。

## 从文件读取请求

JSON 文件保存工具的请求字段。例如，`create-request.json` 可以包含 `{ "name": "another-workspace" }`：

```sh
agentdrive call workspace_create --request-id create-from-file --json - < create-request.json
```

PowerShell 示例：

```powershell
$env:AGENTDRIVE_TOKEN_FILE = 'C:\Users\alice\agentdrive\token'
Get-Content -Raw .\create-request.json | .\target\debug\agentdrive.exe call workspace_create --request-id create-from-file --json -
```

推送和拉取示例见 [S3 配置](configuration.zh-CN.md)；guard、缓存范围、合并和操作结果恢复参数见 [MCP 参考](mcp.zh-CN.md)。
