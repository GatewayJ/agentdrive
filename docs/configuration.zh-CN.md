# 配置与 S3

[English](configuration.md) | [简体中文](configuration.zh-CN.md)

## 守护进程配置

运行 `agentfsd init --directory /absolute/path/to/agentfs`，创建 `agentfs.json`、私有 token 文件，以及挂载和文件交换目录。修改生成的配置后，运行 `agentfsd serve --config /absolute/path/to/agentfs/agentfs.json`。daemon 在启动时读取配置，并每秒检查运行配置的修改。

生成的文件包含 `data_dir`、`listen`、`mount_roots`、`directory_roots` 和 `credentials`。其他可选设置使用下表中的默认值，按需添加即可。

| 字段 | 含义与默认值 |
| --- | --- |
| `data_dir` | 必填，本地状态的绝对路径；初始化选择 `<directory>/state`。每个状态目录只能由一个守护进程使用。 |
| `listen` | 必填，本机回环地址和端口；初始化选择 `127.0.0.1:7421`。 |
| `mount_roots` | 必填，允许挂载的绝对根目录数组；初始化创建 `<directory>/mounts`。 |
| `directory_roots` | 允许导入和导出的绝对根目录；省略时为 `[]`。初始化创建并允许使用 `<directory>/exchange`。 |
| `credentials` | 必填，格式为 `{ "principal": "owner", "token_file": "/absolute/path/token" }` 的数组，至少需要一项凭据。 |
| `administrators` | 可以读取和修改 daemon 设置的身份列表，默认为 `["owner"]`。与工作区授权分别管理，修改后需要重启。 |
| `allowed_origins` | HTTP Origin 允许列表，默认 `[]`。CLI 和 stdio 连接不会发送 Origin。 |
| `cache` | 可选对象，只包含 `max_bytes`：本地不可变对象的缓存容量，以字节为单位，默认为 `2147483648`（2 GiB），必须为正整数。启用 S3 时用于缓存清理。 |
| `remote` | 下文的 S3 配置。省略或设置为 `null` 时仅使用本地存储。 |

例如，添加 `"cache": { "max_bytes": 536870912 }` 可以将缓存容量设为 512 MiB。程序自动将清理目标设为容量的 75%，此例对应 384 MiB。

状态目录、允许的根目录和 token 路径必须使用绝对路径。启动前应创建允许的根目录。JSON 字符串不会展开 `~`、`$HOME` 或环境变量。Windows 路径使用转义后的反斜线，例如 `"C:\\Users\\alice\\agentfs\\state"`，也可以使用正斜线。未知的顶层配置字段，以及未知的 `cache` 或 `remote` 字段会被拒绝。配置文件上限为 1 MiB。

token 文件必须是普通文件，最多 4096 字节，去除首尾空白后至少包含 32 个非空白字节。Unix 环境下，文件不能授予所属组和其他用户访问权限；初始化会创建权限为 `0600` 的文件。各 token 必须唯一。token 选择配置中的 `principal`，工作区所有权和授权决定该身份的访问范围。请使用守护进程用户的文件系统权限保护配置和状态。

## 热更新与 Dashboard 设置

在 Dashboard 左侧选择“设置”，或访问 `/settings`，使用管理员 token 认证。页面支持修改 S3 连接与访问凭据、以 GiB 为单位的本地缓存容量，以及 HTTP Origin 允许列表。选择 S3 模式时显示缓存容量；本地模式保留已保存的容量值，供再次启用 S3 时使用。S3 连接设置保存后，在程序运行期间完成切换。保存会写入 daemon 启动时使用的同一个 `agentfs.json`，并发布完整配置快照。配置文件保持原有顶层结构，保存时仍会省略采用默认值的可选字段。

| 配置 | 生效时间 |
| --- | --- |
| `remote` | 准备历史内容与远程连接后切换，无需重启 |
| `cache` | 下一次缓存清理；后台任务每 30 秒执行一次 |
| `allowed_origins` | 下一次 HTTP 请求，包括已有 MCP 会话的请求 |
| `data_dir`、`listen`、`mount_roots`、`directory_roots` | daemon 重启后 |
| `credentials`、token 文件内容、`administrators`、进程环境变量 | daemon 重启后 |

Origin 使用 HTTP 或 HTTPS 的 `scheme://host[:port]` 格式，不包含路径、末尾斜线、用户名密码、通配符或显式填写的默认端口。同源 Dashboard 请求及没有 Origin 的请求可以访问，所有请求仍需 Bearer 身份验证。此设置控制 Origin 检查，服务不会因此添加 CORS 响应头。

直接修改配置文件后，daemon 在每秒检查时读取候选内容，校验通过后发布。JSON 错误、无效容量、文件无法访问，以及任意启动字段的变化都会保留完整的当前配置，并在设置页面显示错误。修改启动字段后，需要重启 daemon 才能应用整个文件。修正文件后，下一次检查会清除错误。建议通过原子替换文件提交修改，避免读取正在写入的 JSON。daemon 不监听 token 文件内容。

带身份验证的 HTTP 接口：

- `GET /api/settings`：返回当前 `runtime`、只读 `startup` 字段、`config_path`、配置版本 `revision` 和 `reload_error`。
- `PUT /api/settings`：提交 `{ "revision": "<GET 返回的版本>", "runtime": { "cache": { ... }, "allowed_origins": [], "remote": null } }`。需要提供 GET 返回的完整运行配置，成功后返回保存的设置。

两个接口都要求身份属于 `administrators`，工作区授权不会授予设置权限。响应包含 token 文件路径，但不会返回 token 内容或 S3 环境凭据。缺少认证返回 401，设置权限不足返回 403，版本过期或检测到外部文件修改返回 409。校验失败时，当前配置和文件保持原有内容。页面检测到新版本时会保留尚未保存的表单内容，由用户选择重新载入。

## 本地缓存容量

`cache.max_bytes` 管理此 daemon 全部工作区的不可变对象缓存。后台每 30 秒检查一次，超过设置容量时清理到容量的 75%，按字节向下取整。程序内部的磁盘剩余空间阈值为 64 MiB：低于此值时也会启动清理，直到同时满足容量目标和剩余空间阈值，或者已经没有可清理的内容。

清理只处理已获得远端确认、没有活动读取者且没有受保护引用的对象。当前分支内容、待发布数据、合并候选内容和固定缓存受到保护。受保护内容较多时，实际占用可能超过设置容量。SQLite 元数据、工作文件和临时文件另外占用磁盘空间。本地模式完整保留历史，不执行缓存清理，Dashboard 隐藏缓存容量输入框。

## 运行期间切换存储

在设置页面选择本地或 S3 模式，保存即可切换。`remote.enabled: false` 保留 S3 配置和凭据，便于再次启用。直接修改配置文件也使用相同的切换过程。

启用 S3 时检查目标的读取、条件写入与删除权限，复制本地已知的历史内容和分支引用，随后保存配置并更换连接。切换回本地时补齐已被缓存清理的历史内容，并停止清理本地历史文件。工作目录和挂载继续使用；待同步操作保留到下一次启用 S3。

切换期间部分操作会等待历史内容传输完成。内容缺失、存储空间不足、目标分支已更新、凭据错误或配置保存失败时，当前连接继续有效。目标可能已经收到部分不可变对象和分支引用，重复提交会继续处理。拥有尚未完成的分支所有权转移时，需要完成该操作后才能切换。配置文件的外部修改失败时保留原有运行配置，页面显示错误，后续检查继续尝试。S3 连接检查会创建并删除命名空间内的 `.connection-check/` 临时对象。

`<data_dir>/meta.db` 是本地 SQLite 数据库，并使用 WAL 文件。S3 同步上传不可变文件内容、目录和 inode 索引、版本历史、已发布的操作结果、工作区控制记录和分支引用；数据库文件本身不会上传。目标 daemon 通过 `sync` 的 `direction: "pull"` 将已发布记录导入自己的数据库。本机身份、分支选择、挂载、缓存保留设置、尚未完成的工作和配置分别由各台机器管理。存储结构见[架构说明](architecture.zh-CN.md)。

## S3 字段

以下对象应配置在 `agentfs.json` 的**顶层 `remote` 字段**中：

```json
{
  "remote": {
    "bucket": "agentfs",
    "prefix": "agentfs/v1",
    "region": "us-east-1",
    "endpoint": "http://127.0.0.1:9000",
    "allow_http": true
  }
}
```

这是使用本地 RustFS S3 API endpoint 的配置片段，请保留其他已生成的字段。`endpoint` 是 S3 API 的基础 URL，不包含 bucket 名称、凭据、查询参数或 fragment。适配器使用 path-style 请求，并追加 bucket 名称。端口应采用部署环境提供的 S3 API 端口。

| `remote` 字段 | 必填要求 / 默认值 | 行为 |
| --- | --- | --- |
| `enabled` | `true` | 设置为 `false` 使用本地模式，并保留 S3 配置。 |
| `credentials` | 省略或 `null` | 使用服务环境或 IAM 角色；也可以填写 `access_key_id`、`secret_access_key` 和可选的 `session_token`。 |
| `bucket` | 必填，非空字符串 | 已存在的 bucket 名称。守护进程不会创建 bucket。 |
| `prefix` | `agentfs/v1` | bucket 内共享的命名空间；首尾 `/` 会被移除，结果必须是有效且非空的对象路径。 |
| `region` | `us-east-1` | 签名使用的 region；AWS S3 应填写 bucket 所在的 region。 |
| `endpoint` | 省略或 `null` | 可选的 S3 基础 URL。没有 endpoint 覆盖设置时，依赖库会生成 AWS regional endpoint。 |
| `allow_http` | `false` | 明确使用 HTTP 开发环境 endpoint 时设置为 `true`；HTTPS endpoint 保持 `false`。 |

使用 AWS S3 时，替换 bucket 和 region，并省略自定义 endpoint：

```json
{
  "remote": {
    "bucket": "your-existing-bucket",
    "prefix": "agentfs/v1",
    "region": "eu-west-1",
    "allow_http": false
  }
}
```

## 凭据与环境变量优先级

Dashboard 可以编辑并保存 S3 凭据到配置文件，Unix 保存权限为 `0600`。GET 设置接口会清空 `secret_access_key` 和 `session_token`；PUT 中同一个 Access Key ID 的空密钥表示保留已有凭据。修改 Session Token 时同时填写 Secret Access Key；将 `credentials` 设为 `null` 使用环境凭据。配置中的凭据优先于环境凭据。

S3 凭据由**守护进程**读取。请在启动 `agentfsd serve` 的终端或服务环境中设置变量。`AGENTFS_TOKEN_FILE` 用于 CLI 访问 OpenCSG Workspace 的身份验证。

```sh
export AWS_ACCESS_KEY_ID='your-access-key-id'
export AWS_SECRET_ACCESS_KEY='your-secret-access-key'
# Temporary credentials also require AWS_SESSION_TOKEN.
./target/debug/agentfsd serve --config "$AGENTFS_DIRECTORY/agentfs.json"
```

PowerShell 使用 `$env:AWS_ACCESS_KEY_ID = 'your-access-key-id'` 和 `$env:AWS_SECRET_ACCESS_KEY = 'your-secret-access-key'`，随后启动 `agentfsd.exe`。

当前锁定版本的 `object_store` 按以下顺序选择凭据来源：

1. `AWS_ACCESS_KEY_ID` 和 `AWS_SECRET_ACCESS_KEY`，以及可选的 `AWS_SESSION_TOKEN`。两个 key 字段需要同时提供。
2. `AWS_WEB_IDENTITY_TOKEN_FILE` 和 `AWS_ROLE_ARN`，以及可选的 `AWS_ROLE_SESSION_NAME`。
3. 用于容器任务角色的 `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`。
4. 用于 EKS Pod Identity 的 `AWS_CONTAINER_CREDENTIALS_FULL_URI` 和 `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE`。
5. 前述来源均未提供时，使用实例元数据凭据。

当前适配器不会读取 `~/.aws/credentials`、`AWS_PROFILE` 或 AWS CLI SSO 会话。请导出有效凭据，或者使用支持的角色凭据来源。静态环境变量凭据在启动时读取，替换后需要重新启动守护进程。

OpenCSG Workspace 调用 `AmazonS3Builder::from_env()` 后应用自身配置。配置中的 `bucket`、`region` 和 `allow_http`，包括默认值，都会覆盖对应的 AWS 环境变量。配置中的 `endpoint` 会覆盖 `AWS_ENDPOINT` 和 `AWS_ENDPOINT_URL_S3`。省略 `endpoint` 时沿用环境设置；使用默认 AWS regional endpoint 时，需要移除这两个环境变量。适配器会选择 path-style 请求和基于 ETag 的条件写入。

## 存储要求与验证

存储服务需要支持 GET/HEAD、按前缀列举、对象写入、分段上传和终止上传，以及强一致读取。OpenCSG Workspace 使用 `If-None-Match: *` 创建不可变对象，并以相同条件完成分段上传；更新引用时使用携带之前 ETag 的 `If-Match`。条件写入行为见 [AWS S3 文档](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html)。大于 8 MiB 的文件使用分段上传，分段大小根据文件大小在 8 至 64 MiB 之间选择。存储凭据权限应限制在选定的 bucket 和命名空间内。

守护进程启动时会构建 S3 客户端，但启动成功不能证明 bucket 或凭据可用。按照 [CLI 示例](cli.zh-CN.md)导入内容后，明确执行推送，并查看原始导入操作的结果记录：

```sh
jq -n --arg workspace "$WORKSPACE" '{workspace:$workspace,direction:"push"}' |
  agentfs call sync --request-id "$RUN_ID-push" --json -
jq -n --arg operation "$(jq -r '.id' "$AGENTFS_DIRECTORY/demo-import.json")" '{operation:$operation}' |
  agentfs call operation_get --json -
```

检查原始操作的 `remote_confirmed`。`workspace_status.capabilities.remote_configured` 表示当前是否启用 S3，`pending_jobs` 表示队列长度。保存了 S3 配置但设置 `remote.enabled: false` 时，返回 `remote_configured: false`。启用 S3 后，守护进程每三秒重试待同步任务。`local_saved: true` 的结果记录仍然可能包含远端错误；请保留 operation ID，并参考[恢复说明](operations.zh-CN.md)。

要检查条件写入、并发更新、20 MiB 分段内容和两个守护进程之间的同步，请使用隔离的 [S3/RustFS 测试](validation.zh-CN.md)。这些测试使用独立的测试 bucket，不适合作为生产环境健康检查。

## 多台机器

各守护进程需要配置相同的 S3 endpoint、bucket、prefix 和签名 region。每台机器分别初始化状态目录并获得自己的 `LocationId`，然后使用源守护进程返回的 workspace ID。目标机器上通过身份验证的 principal 需要是工作区所有者，或者获得适当授权。各机器的 token 可以不同。

在源机器执行推送，然后在目标机器使用相同 workspace ID 调用 `sync`，设置 `direction: "pull"`。拉取会导入历史和远端分支引用。可以使用已保留的版本创建本地分支；需要接管已有可写分支时，完成在线 `ownership_transfer`。详见[操作说明](operations.zh-CN.md)和 [MCP 参考](mcp.zh-CN.md)。

配置定义见[文件结构](../crates/agentfs-config/src/file.rs)、[运行配置管理](../crates/agentfs-config/src/store.rs)和 [S3 适配器](../crates/agentfs-s3/src/lib.rs)，依赖版本见 [Cargo.lock](../Cargo.lock)。
