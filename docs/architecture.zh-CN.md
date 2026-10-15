# 架构

[English](architecture.md) | [简体中文](architecture.zh-CN.md)

`agentfsd` 负责组装应用：创建具体适配器，将其注入 engine，并通过生成的 `Services` 提供服务。engine 依赖 model、ports 和 config crate；持久化、原生驱动、S3 和协议依赖由适配器管理。

| Crate | 职责 |
| --- | --- |
| `agentfs-model` | ID、路径、对象、版本、分支、会话、操作、合并记录和错误 |
| `agentfs-config` | 配置定义、默认值、校验、文件原子替换、管理员授权和运行时配置快照 |
| `agentfs-ports` | 存储、文件系统、挂载、时钟、目录和应用接口 |
| `agentfs-engine` | 文件行为，以及工作区、版本、会话、同步、缓存、所有权和合并流程 |
| `agentfs-local` | 进程锁定、SQLite 元数据、不可变对象、工作文件、租约和读取验证 |
| `agentfs-s3` | 不可变对象的流式传输和引用的条件更新 |
| `agentfs-platform` | FUSE/WinFsp 回调、受目录范围限制的主机目录访问和时钟 |
| `agentfs-mcp` | 具有明确类型的工具、带身份验证的 HTTP 路由、Dashboard 与设置接口、客户端和 stdio 转发 |
| `agentfs-test-support` | 可控时钟、挂载和存储测试替代对象、服务中断和确认丢失的故障注入 |
| `agentfsd` / `agentfs` | 守护进程生命周期和命令行客户端 |

## 模块职责

| 模块 | 职责 |
| --- | --- |
| `engine/runtime` | 工作区授权、本地身份、修改顺序与共享运行状态 |
| `engine/files` | 文件操作、打开的句柄、权限与 generation 检查 |
| `engine/content`、`engine/trees` | 内容读取验证、不可变索引与关联对象遍历 |
| `engine/snapshots`、`engine/revisions` | 捕获工作内容、保存版本、创建分支与恢复内容 |
| `engine/workspaces` | 创建工作区、查询状态，以及主机目录的导入导出 |
| `engine/sessions`、`engine/mounts` | 将 Agent 会话关联到当前分支、保存回合结果、协调挂载切换 |
| `engine/replica`、`engine/ownership` | 同步任务、远程条件写入与写入所有权交接 |
| `engine/storage` | 历史内容补齐、远程引用复制与存储切换 |
| `engine/cache` | 预取、固定缓存，以及按照当前配置回收允许清理的内容 |
| `engine/merge` | 准备合并候选内容、编辑文件、解决冲突与应用结果 |
| `engine/operations` | 请求去重、操作进度与持久化结果记录 |
| `config/types` | 缓存与 S3 的配置类型及策略校验 |
| `config/file` | 配置文件结构、默认值、token 文件读取与初始化目录 |
| `config/store` | 校验候选配置、控制写入顺序、保存文件与发布 `ArcSwap` 快照 |
| `mcp/gateway`、`mcp/server` | 验证 HTTP 请求身份并调用应用接口 |
| `mcp/web`、`mcp/dashboard` | 工作区、生命周期、设置的 HTTP 接口与浏览器页面 |
| `apps/agentfs/integration` | ACP、Codex app-server 事件转发与持久化生命周期 |
| `apps/agentfsd/main` | 创建适配器、运行 HTTP 服务和后台任务、正常关闭服务 |

`agentfs-ports` 定义 engine 使用的接口，存储和平台适配器负责实现接口。配置模块依赖 model 中的数据类型，数据库、挂载、S3 和 HTTP 服务由 daemon 创建。daemon 将同一个 `LiveConfig` 传给 engine 和 HTTP 入口；`ConfigStore` 统一发布配置，使用配置的模块为每次操作取得完整快照。

## 配置生命周期

`ConfigStore::open` 读取并校验文件。`StartupConfig` 在服务创建时提供存储目录、挂载、连接与凭据配置。缓存策略、S3 连接和 HTTP Origin 允许列表由同一个 `ArcSwap<Snapshot>` 保存。缓存清理在操作期间保持同一份快照；HTTP 身份验证为每次请求取得当前快照。

daemon 每秒检查配置文件。Dashboard 保存与文件加载使用同一个 `ConfigStore` 和写入锁。保存时检查配置版本，在同目录写入并同步临时文件，替换配置文件，然后发布快照；Unix 还会同步目录。文件错误、无效内容以及启动配置变化都会保留当前有效配置，并提供错误信息。配置版本检查会拒绝过期的 API 保存请求及已经检测到的外部修改；外部编辑器不使用 daemon 的写入锁，建议通过原子替换文件提交修改。

设置管理权限由启动配置中的 `administrators` 决定，默认为 `["owner"]`，与工作区授权分别管理。设置接口不会读取或返回 token 内容。修改 HTTP 凭据、管理员、本地目录或监听地址需要重启。S3 设置通过 `ConfigApplier` 准备内容、保存配置，并在持有生命周期、发布和内容保护锁期间更换远程适配器。字段说明与 HTTP 接口见[配置说明](configuration.zh-CN.md)。

## 本地持久化

每个守护进程状态目录包含 `engine.lock`、`meta.db`、SQLite WAL 文件，以及 `objects/`、`staging/` 和 `working/`。进程锁定确保只有一个写入者。SQLite 使用 WAL 和 `synchronous=FULL`。修改数据库结构前会检查持久化格式版本，存储错误通过明确的错误类型返回。

两种存储模式均将 `meta.db` 和 WAL 文件保存在本地。S3 接收不可变对象，以及下文说明的控制记录和分支引用。拉取操作将已发布历史导入接收端的数据库；本机身份、会话关联、挂载关联、缓存保留设置和尚未完成的工作属于本地状态。缓存容量仅统计不可变对象文件。S3 模式使用 `cache.max_bytes`，自动计算 75% 的清理目标，并使用内部固定的 64 MiB 磁盘剩余空间阈值。本地模式完整保留历史内容。

object ID 使用 SHA-256 对域分隔标记、格式版本、对象类型和内容字节计算摘要。发布时验证摘要和大小，将临时文件同步到存储设备，以不可变名称发布文件并登记对象。Unix 会在发布后同步目录，Windows 使用 write-through 文件移动。元数据事务引用已经安装的不可变对象，读取租约保护打开的内容，使其不会被缓存回收。

`TreeRoot` 引用大小受限的目录和 inode 索引。每个索引页面最多包含 128 项，元数据对象上限为 2 MiB。文件内容使用大小受限的缓冲区进行流式处理。目录项名称使用 NFC 规范化，并通过 Unicode case folding 比较。跨平台名称检查会拒绝路径穿越、Windows 保留名称、末尾句点或空格，以及不兼容的字符。

每个分支通过串行控制管理文件修改与快照保存。保存快照时，将变化的文件生成不可变对象，构建不可变索引，然后在一个本地事务中提交根对象、版本、轮次或会话结果、操作结果记录和同步任务。文件系统 `fsync` 确认本地快照。自动保存会等待五秒没有新的写入；持续写入时，最长等待三十秒。守护进程每秒检查一次。

生命周期控制依次执行会话和挂载变更、恢复、合并应用、所有权交接和取消操作。文件句柄携带挂载和分支的 generation，generation 变化后会拒绝旧句柄。一个挂载路径只能有一个活动关联，一个分支最多具有一个可写挂载。

## 同步与所有权

工作区由不可变对象、通过条件写入更新的工作区控制记录和分支引用组成。分支引用包含 owner、epoch、generation、source generation、head 和不可变历史索引。历史索引记录版本、轮次结果和操作结果。

发布时先上传全部被引用的对象，再对分支引用执行 compare-and-swap。条件写入使用存储提供程序返回的不透明版本值。确认丢失时，发布程序检查原始操作的历史记录。本地持久化任务支持重试和进程重启。工作区控制记录保护发布或读取过程中使用的根对象，并协调 GC 阶段。远端物理删除已禁用。

拉取会导入远端历史和引用，并保留活动本地分支的状态。接管已有分支需要经过确认的所有权交接。交接时停止旧写入者，完成其发布队列，然后通过条件写入更新 owner 和 epoch。新的 location 接管已确认状态。该 API 无法强制替换无法联系的 owner。

## 合并

合并准备会选择共同祖先，或检查调用者指定的祖先，然后比较路径变化并生成不可变候选内容。AI 可以查询候选版本的文件列表，选择 source、target，删除文件，提交替换内容，或从已有版本复制文件。编辑会生成新的候选版本。应用要求目标分支没有未提交内容、所有权与版本匹配、全部冲突已处理，并且挂载允许切换。合并版本保留来源与目标两个父版本。


## 身份验证边界

创建 MCP 服务前，bearer 凭据会选择 principal。每项凭据拥有独立的 HTTP 会话管理器，工具参数无法覆盖 principal。HTTP 入口使用 Host 和 Origin 允许列表，并将请求正文限制为 8 MiB。守护进程监听本机回环地址。远程访问需要管理员管理的身份验证 TLS 代理，并保留允许的 Host 值。

工作区授权控制应用操作。原生文件系统访问使用守护进程的操作系统身份。配置、凭据、状态、挂载根目录和交换根目录应限制为该身份访问；Windows 需要相应配置 NTFS 权限。S3 凭据持有者和管理员被视为可信的存储写入者。

参见[配置与 S3](configuration.zh-CN.md)、[CLI 使用](cli.zh-CN.md)和 [MCP 参数与能力](mcp.zh-CN.md)。
