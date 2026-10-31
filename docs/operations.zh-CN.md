# 操作与恢复

[English](operations.md) | [简体中文](operations.zh-CN.md)

## 请求身份与结果记录

对于命令工具，相同工具和相同参数的重试应复用 `request_id`。请求身份由接收请求的 daemon 和通过身份验证的 principal 确定，并在重启后保留。冲突复用会返回 `REQUEST_ID_CONFLICT`。命令会在执行记录中的流程前登记操作。`operation_by_request` 查询提交给当前 daemon 的请求；`operation_get` 通过 operation ID 查询结果，也支持从其他节点同步的已发布操作。不同节点可以使用相同的请求名称，同步时会保留每条操作记录。`operation_cancel` 直接接收已有 operation ID，详见 [MCP 参数参考](mcp.zh-CN.md)。

升级时，已保存且没有对应本地分支记录的旧操作会转入同步历史，释放请求名称，并保留通过 operation ID 查询的能力。迁移在单个事务中执行一次，也适用于已经创建同步记录表的数据库。旧记录没有来源位置字段；对应分支在本地存在时，包括已经转移或接管的分支，原有请求登记会保留，保证本地命令的幂等性。接管分支中无法确认来源的旧记录仍可能要求新命令使用新的请求名称。

请分别检查以下结果字段：

| 字段 | 含义 |
| --- | --- |
| `local_saved` | 操作结果及其引用的本地状态已经提交 |
| `remote_confirmed` | 操作的远端发布已经确认 |
| `mount_ready` | 涉及原生挂载时，请求的挂载关联已经就绪 |
| `phase` | 当前操作阶段 |
| `error` | 具有明确类型的失败信息，可能与已完成的本地保存同时存在 |
| `sync_progress` | 可选的同步队列、阶段和传输计数 |
| `recovery` | 响应中的继续操作方式：`retry_request`，或携带预期挂载 generation 的 `release_mount` |

`recovery` 根据已保存的操作流程生成，独立于 `error.retryable`。`retry_request` 使用原命令、request ID 和参数继续操作。`release_mount` 在失败或响应丢失后保留原 request ID、operation 和 generation；目录恢复可用后，失败的释放操作可以继续执行，已完成请求返回已有结果。挂载就绪和远端发布分别报告，挂载恢复后仍保留远端发布失败信息。

远端错误可能伴随 `local_saved: true`。请保留已有 operation ID，恢复远端连接，然后运行 `sync`，或者等待守护进程重试持久化的发布队列。远端超时无法确认条件写入是否已提交，发布程序会检查已记录的操作身份。

## 挂载变更

恢复和应用合并可能要求停止使用已挂载的文件系统。处于 `waiting_for_quiesce` 的操作携带原始挂载 generation。请停止 Agent 和其他使用该挂载的进程，关闭文件，并使终端退出该目录。随后携带 operation ID 和 `expected_binding_generation` 调用 `mount_release`。重试 release 操作时，需要复用它自己的稳定 request ID。

`branch_switch` 直接尝试完成切换。挂载被占用时返回 `MOUNT_BUSY`、`retryable: true`，并提示关闭文件和相关进程，或重启 Agent。卸载失败时保留原分支选择和挂载。处理原因后，使用相同 request ID 重试 `branch_switch`，无需另行调用 `mount_release`。其他失败保留具体错误码和原因。如果旧挂载已经卸载、目标挂载失败，原路径可能暂时没有挂载；相同请求的重试会继续安装目标挂载。已完成的请求直接返回原结果，不会再次切换。

修改挂载关联前，操作会再次验证原始 branch guard。目标状态已经变化时，操作会失败，并保留尚未应用的候选结果。generation 检查防止旧句柄继续访问替换后的分支。本地提交成功但挂载失败时，会返回 `committed_mount_pending`，持久化结果保留用于恢复。只有原始挂载关联尚未变化且本地提交尚未完成时，才接受取消操作。

守护进程正常关闭时，会停止 HTTP 流量和后台任务，保存已修改的本地分支，并请求正常卸载。仍在使用中的挂载会报告恢复要求。守护进程或文件系统仍在活动时，请勿删除状态目录。

## 进程重启

启动时会从最后一次已确认的不可变分支根对象恢复工作元数据，并移除可重新生成的工作副本。进程意外终止后，尚未通过 fsync、自动保存、版本操作或 `mount_save`确认的写入可能丢失。写入失败后，如果元数据持久化也失败，受影响的分支会停止，等待恢复。

本机的分支选择在卸载和重启后仍然保留。重新执行 `agentdrive mount --workspace <workspace-id> <path>` 或调用 `mount_prepare` 即可复用。复用原生挂载前会检查其状态；匹配的挂载关联保留原有 ID 和 generation。路径上存在无法识别的文件系统时，需要操作人员清理；服务不会强制卸载无关文件系统。

状态目录应位于正确支持文件同步和原子重命名的本地文件系统上。文件系统和存储设备必须遵守同步请求。Unix 发布对象时会同步文件数据和目录；Windows 同步文件数据，并通过 write-through 操作发布文件名称。配置远端存储后，远端确认为数据持久性提供额外保障。

## 同步、缓存与所有权

`sync` 支持 `push`、`pull` 和 `both`。在新的 location 拉取工作区会导入其历史。首次可写 `mount_prepare` 也会自动导入尚未存在的工作区，并从根分支最新发布的工作根对象创建当前 location 的分支；后续挂载复用本机选择。持有远端历史引用不会获得本地写入所有权。

使用 `cache_prefetch` 下载指定版本或路径。使用 `cache_pin` 并设置 `enabled: true`，保留完整选择范围，以供离线使用。`cache_status` 会报告内容是否完整以及是否受到保留设置保护。取消保留后，如果不存在其他保护，缓存可以在之后回收。容量回收移除已经获得远端确认、没有活动读取者且没有受保护引用的不可变对象。被后续检查点替代的未确认对象，在检查全部保留引用及活动读取者后也可以回收。

启用 S3 后，通过 Dashboard 设置或 `cache.max_bytes` 调整本地缓存容量。超过容量或磁盘剩余空间低于内部的 64 MiB 阈值时开始清理，容量目标为设置值的 75%。受保护的内容可能使实际占用超过设置容量。本地模式完整保留历史；从 S3 切换回本地时，会下载缺少的历史内容，完成后停用远程连接。存储切换和本地 SQLite 数据库的职责见[配置说明](configuration.zh-CN.md)。

所有权交接在线执行。请在旧 location 停止写入文件，并释放可写挂载。使用当前 `BranchGuard` 和目标 `LocationId` 提交 `ownership_transfer`。旧 location 会停止分支写入，完成发布队列，再更新条件引用。新的 location 通过拉取接管已确认的所有权。远端响应无法确定时，请保留原始交接操作。

## 合并文件

通过 `workspace_status` 取得分支的 `formal_head`，使用 `revision_files` 列出该版本的文件。每页最多 200 个路径；返回 `next` 时，将它作为下一次请求的 `after`。可选的 `path` 限制目录范围。`revision_read` 读取文件字节，`revision_diff` 比较两个版本。需要包含挂载目录的修改时，通过 `mount_save` 提交，也可提供 branch guard 调用 `revision_commit`，随后使用返回的版本。

`merge_prepare` 接收来源版本和目标 `BranchGuard`，生成候选内容。AI 可以使用 `revision_files`、`revision_read` 检查候选版本，再通过 `merge_resolve` 编辑任意文件。每次编辑返回新的候选版本，后续操作使用这个版本：

- `source`、`target`：选择该路径在来源或目标版本中的内容，包括目录内容。
- `delete`：删除指定文件或目录。
- `replace`：提供 `content` 字节数组和 `mode`，创建或替换文件；单个文件最多 1 MiB。
- `copy`：提供 `revision` 和 `source_path`，将同一工作区已有版本中的文件复制到 `path`。图片及大型二进制文件直接复用内容，保留原有属性。

自行生成的图片可以通过 `workspace_import` 导入允许的交换目录，再使用返回的版本作为 `copy.revision`。新文件所在的父目录需要已经存在于候选内容中。处理目录冲突后才能编辑其中的文件；选择整个目录会同时处理该目录下的冲突。

所有冲突处理完成后调用 `merge_apply`，提供当前候选版本和目标 guard。系统检查目标分支是否变化、是否存在未提交修改以及挂载使用情况。成功后创建保留来源与目标两个父版本的合并记录。正在使用的挂载需要通过 `mount_release` 完成切换。CLI 使用相同名称和参数，例如：

```sh
agentdrive call revision_files --json "{\"workspace\":\"$WORKSPACE\",\"revision\":\"$SOURCE_REVISION\"}"
agentdrive call merge_resolve --request-id "$RUN_ID-edit" --json - < merge-edits.json
agentdrive call merge_apply --request-id "$RUN_ID-apply" --json - < merge-apply.json
```

`merge-edits.json` 的参数示例：

```json
{
  "merge": "<merge-id>",
  "candidate": "<current-candidate-revision-id>",
  "resolutions": [
    {"choice": "source", "path": "/README.md"},
    {"choice": "copy", "path": "/cover.png", "revision": "<image-revision-id>", "source_path": "/cover.png"}
  ]
}
```

远端凭据见[配置与 S3](configuration.zh-CN.md)，可执行示例见 [CLI 使用](cli.zh-CN.md)。

## 检查点与同步进度

Dashboard 同步状态同时核验本机修改与保存序号，以及远端确认的 owner、authority epoch、generation、保存序号、版本和内容对象。发布队列为空时，新修改仍可能等待本机保存或远端确认。工作区状态表示本机负责写入的分支，档案室分别显示每个分支的保存与确认状态。

本地 fsync 保存当前工作内容，不增加 revision。`mount_save` 创建正式 commit 并更新 `formal_head`；内容没有变化时复用当前 commit。`revision_commit` 还支持提供提交说明或创建 checkpoint。后台自动保存继续按照五秒无新写入、三十秒持续写入的期限生成 `Autosave` revision。远端 fsync 发布检查点并等待确认；合并检查点同步任务保证最新持久状态，显式 revision 继续保留在历史中。

`mount_save.wait_for_remote_ms` 接受 0–120000 毫秒，默认 30000。该参数限制本地保存完成后的远端等待时间，本地内容捕获耗时单独计算。等待到期时返回 `local_saved: true`、`remote_confirmed: false`，发布继续进行，等待到期本身不代表失败。使用返回的 ID 查询 `operation_get`，或使用原 request ID 查询 `operation_by_request`，无需重复创建保存请求。实际远端错误保留在 `error` 中，持久化任务会继续重试。文件系统的远端 fsync 必须确认完成才能成功返回。

`sync_progress.stage` 包含 `queued`、`preparing`、`transferring`、`publishing` 和 `complete`。`pending_jobs` 表示 workspace 队列数量，等待时的 `jobs_before` 表示队列位置，`batch_jobs` 表示本次共同发布的操作数量。对象计划生成前，`total_objects` 和 `total_bytes` 为 null；完成计数表示本次验证或传输的对象，包含元数据，不包含直接复用的远端子树，也不等于网络传输字节。活动计数保存在内存，完成计数随操作结果持久保存；重启或重试后返回队列状态，新一轮发布重新计数。`updated_ns` 记录采样时间。最终确认仍然检查 `remote_confirmed`。
