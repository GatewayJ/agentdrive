# MCP connection, parameters, and capabilities

[English](mcp.md) | [简体中文](mcp.zh-CN.md)

## Connect a client

Start `agentdrived` using the [CLI guide](cli.md). For clients supporting an `mcpServers` configuration, a stdio connection can use:

```json
{
  "mcpServers": {
    "agentdrive": {
      "command": "/absolute/path/to/agentdrive",
      "args": ["mcp"],
      "env": {
        "AGENTDRIVE_ENDPOINT": "http://127.0.0.1:7421/mcp",
        "AGENTDRIVE_TOKEN_FILE": "/absolute/path/to/agentdrive-data/token"
      }
    }
  }
}
```

Replace both paths with actual absolute paths; use `agentdrive.exe` on Windows. The exact outer configuration format depends on the client. The proxy uses stdin/stdout for MCP and stderr for logs. It connects to an already running daemon and shares that daemon's state with other clients.

For direct Streamable HTTP, use `http://127.0.0.1:7421/mcp` and `Authorization: Bearer <token-file-contents>`. Use an MCP client that performs initialization and preserves the returned session information. The accepted Host values are the bound address and `localhost:<port>`. A supplied Origin must match the request's own HTTP origin or `allowed_origins`; native CLI/stdio connections omit Origin. The daemon binds only to loopback; access from other machines requires an authenticated TLS proxy preserving an accepted Host value.

The server advertises the MCP `tools` capability. It provides 32 tools, including their input JSON schemas and annotations. Read the connected server's catalog with `tools/list` or:

```sh
agentdrive tools > tools.json
jq '.tools[] | select(.name == "mount_prepare") | .inputSchema' tools.json
```

## Argument forms and results

The 19 command tools in the next table require an envelope. For example, the `arguments` object in a `tools/call` for `workspace_create` is:

```json
{
  "request_id": "create-demo-1",
  "request": {
    "name": "demo"
  }
}
```

The equivalent CLI is `agentdrive call workspace_create --request-id create-demo-1 --json '{"name":"demo"}'`. `request_id` must be nonempty, contain no control characters, and be at most 256 UTF-8 bytes. Its identity belongs to the authenticated principal and survives process restarts. Retry with the same ID, tool, and arguments; conflicting reuse returns `REQUEST_ID_CONFLICT`.

The eleven query tools receive direct argument objects. `operation_cancel` also receives direct arguments and modifies the referenced operation; it does not accept the command envelope. For `operation_by_request`, use:

```json
{
  "request_id": "create-demo-1"
}
```

Tool argument objects and command request structs reject unknown fields. Consult nested schemas for their accepted properties. Optional nullable fields can be omitted or set to `null`; fields with defaults, such as `durability` and `grants`, can be omitted but do not accept `null`.

Successful data is returned through `structuredContent`. Failures set `isError: true` and include `error: {code, message, retryable}`. Commands return an `OperationRecord`; `operation_get`, `operation_by_request`, and `operation_cancel` also return that record. Inspect `local_saved`, `remote_confirmed`, `mount_ready`, `phase`, `result`, and `error`. `mount_ready` is nullable. A completed local save can coexist with a remote error, and an error-free response can still require mount release. See [operations](operations.md).

## Common parameter types

IDs such as `workspace`, `branch`, `revision`, `merge`, `operation`, and `location` are UUID strings returned by the service. Byte counts and generations are JSON integers. Workspace paths use absolute slash-separated paths such as `/src/main.rs`; host paths refer to the daemon machine and must satisfy its configured roots.

| Type | Fields and behavior |
| --- | --- |
| `BranchGuard` | Required `branch`, `authority_epoch`, `generation`, `expected_head`; optional nullable `mutation_seq`. Obtain the branch from `workspace_status.branches`, map `id` to `branch` and `formal_head` to `expected_head`, and copy the remaining values. Including `mutation_seq` also detects unsaved mutations. Refresh before each new guarded operation. |
| `durability` | `local` by default; `remote` requests remote confirmation. Inspect the receipt if remote publication fails. |
| `RevisionRange` | Required `workspace`, `revision`; optional nullable `paths`. Omitted/null selects `/`, including all content. A provided list must be nonempty; directory paths include descendants. |
| `Grant` | Required `principal` and `permissions`; optional nullable `branch`. Permissions are `read`, `write`, `merge`, `manage`; `manage` authorizes every permission. A null branch applies throughout the workspace. |
| `Resolution` | Required `choice` and `path`. `choice` is `source`, `target`, `delete`, `replace`, or `copy`. `replace` requires `content` as an integer byte array (up to 1 MiB) and `mode`, for example `420` for octal `0644`. `copy` requires `revision` and `source_path` for a file in the same workspace and preserves its contents and attributes. |

## Command tools

Fields below are inside `request`. A suffix `?` means the field may be omitted. Shared types are defined above. Runtime checks can be stricter than the JSON schema's enum or numeric range.

| Tool | Request fields | Behavior / result |
| --- | --- | --- |
| `workspace_create` | `name`, `grants?`, `max_file_bytes?`, `max_working_bytes?`, `max_inodes?` | Creates a workspace and selected writable `main`; `result.workspace`. Defaults: 64 GiB/file, 256 GiB working capacity, 1,000,000 inodes. Inodes must be 1–1,000,000; file capacity must be positive and no greater than working capacity; working capacity must be at most `i64::MAX`. |
| `workspace_adopt_directory` | `workspace?`, `path`, `restart_agent` | Requires `restart_agent: true`. Omit `workspace` to create a workspace with the directory contents as the initial `main` revision. Supply `workspace` to import a new branch with `manage` permission. Pause writes, retain the original at `result.backup_path`, and mount the branch at the original path. Returns `result.branch`, `result.binding`, and `result.restart_required: true`. Restart the Agent and its terminals before further file access. See [directory conversion](integrations.md#convert-an-existing-directory). |
| `workspace_import` | `workspace`, `source` | Imports a stable allowed host directory into a new branch; `result.branch`. |
| `workspace_export` | `workspace`, `revision`, `destination`, `paths?` | Exports selected paths into a new allowed host directory. Omitted/null paths select the whole revision. |
| `revision_commit` | `guard`, `kind`, `message?`, `durability?` | `kind` accepts `commit` or `checkpoint`; returns `result.revision` and `result.branch`. `message` permits up to 4096 UTF-8 bytes without NUL. |
| `branch_create` | `workspace`, `name`, `source?`, `source_branch?` | Provide at most one source. Omission uses the selected branch; branch sources include unsaved working content. Returns `result.branch` without changing the selection. |
| `branch_switch` | `workspace`, `branch` | Save the previous branch, switch selection, and remount the original path in one call. On `MOUNT_BUSY`, close files/processes using the directory or restart the Agent, then retry `branch_switch` with the same request ID. Other failures retain their specific cause. Completed retries return the original receipt. |
| `branch_restore` | `guard`, `revision`, `durability?` | Restores a branch, with mount release when required. |
| `sync` | `workspace`, `direction`, `branches?` | `direction`: `push`, `pull`, `both`; omission/null of `branches` selects all. `both` pushes before pulling. On pull, discovery imports remote history before filtering branch IDs in the result. |
| `cache_prefetch` | `workspace`, `revision`, `paths?` | Downloads a `RevisionRange`; `result.status`. |
| `cache_pin` | `range`, `enabled` | `range` is a `RevisionRange`; `true` downloads and protects content, `false` removes that pin. |
| `ownership_transfer` | `guard`, `target` | `target` is the destination `LocationId`; performs an online ownership transfer. |
| `merge_prepare` | `workspace`, `source`, `target`, `base?` | `source` and optional `base` are revision IDs; `target` is a `BranchGuard`; returns `result.candidate`. |
| `merge_resolve` | `merge`, `candidate`, `resolutions` | `candidate` is the current candidate revision ID; edits any candidate path using `source`, `target`, `delete`, `replace` or `copy` and returns a new candidate. |
| `merge_apply` | `merge`, `candidate`, `target`, `durability?` | Requires resolved conflicts and a clean target matching its `BranchGuard`; may require mount release. |
| `mount_prepare` | `workspace`, `path`, `branch?`, `access?`, `revision?`, `durability?` | Defaults to `read_write`. An omitted branch reuses this location's selection; a new location imports the workspace and creates its branch from the latest published working root. Identical requests from the same principal reuse the binding. An explicit branch must exist locally; a revision requires `read_only`. |
| `mount_save` | `mount`, `expected_binding_generation`, `durability?`, `wait_for_remote_ms?` | Save the mounted files as a formal commit, update `formal_head`, and clear `dirty` for the saved content. Unchanged content reuses the current commit. Returns a revision receipt; a stale binding returns `STALE_BINDING`. |
| `mount_release` | `operation`, `expected_binding_generation` | Continues a pending operation after processes release the original mount. The operation ID comes from the pending receipt. |
| `mount_unmount` | `mount`, `generation` | Uses the binding ID and generation from `workspace_status.mounts`; requests normal unmount. |

## Direct-argument tools

| Tool | Arguments | Result |
| --- | --- | --- |
| `workspace_list` | `{}` | Visible workspace array. |
| `workspace_status` | `workspace` | Workspace, local location, `selected_branch`, branches, remote references, mounts, pending jobs, capabilities, and storage statistics. |
| `workspace_relationships` | `workspace`, `search?`, `mounted?`, `include_released?`, `branch?`, `mount?`, `owner_location?`, `mount_location?`, `mount_state?`, `limit?`, `cursor?` | Authorized branch ownership, local mount bindings and creation origins; bounded pages and explicit remote observation status. |
| `revision_list` | `workspace`, `branch?` | Retained revision array; optional branch filter. |
| `revision_diff` | `workspace`, `after`; optional `before`, `cursor`, `limit` | `{entries, next, total}`. Omitted `before` means an empty tree. Default page size 100, maximum 200; pass `next` as `cursor`. Entries contain `path`, `previous_path`, `kind`, `before`, `after`. |
| `revision_text_diff` | `workspace`, `after`, `path`; optional `before`, `previous_path` | File attributes and UTF-8 line hunks with three context lines, old/new line numbers and final-newline information. Status is `text`, `unchanged`, `binary`, `too_large` or `directory`. Maximum 256 KiB and 10,000 lines per side. |
| `revision_files` | `workspace`, `revision`, `path?`, `after?` | Up to 200 paths and attributes per page from a branch head or candidate revision. Pass returned `next` as `after`; `path` restricts the subtree. |
| `revision_read` | `workspace`, `revision`, `path`, `offset`, `size` | `{bytes, length}`; offset and size are bytes, size at most 4,194,304. |
| `cache_status` | `workspace`, `revision`, `paths?` | `complete`, `offline_ready`, `completed_bytes`, and missing object IDs for the range. `offline_ready` additionally requires pin protection. |
| `merge_get` | `merge` | Current candidate and conflicts. |
| `operation_get` | `operation` | Operation receipt visible to the principal. |
| `operation_by_request` | `request_id` | Original operation receipt for this principal. |
| `operation_cancel` | `operation` | Cancels an eligible operation before commit or attachment changes; returns its receipt. This tool modifies state. |

`mount_save.wait_for_remote_ms` defaults to 30000 and accepts 0–120000. When waiting expires, publication continues; poll the original operation and inspect `remote_confirmed` and `sync_progress`. See [progress semantics](operations.md#checkpoints-and-synchronization-progress).

## Runtime capabilities

Call `workspace_status` for an existing workspace and inspect `capabilities`:

| Field | Current meaning |
| --- | --- |
| `format_version` | Persisted format version, currently `1`. |
| `platform_mount` | `linux-fuse`, `windows-winfsp`, `macos-macfuse`, `macos-fuse-t`, or `macos-macfuse-feature-required`, according to the build. This describes the compiled adapter; a successful mount also needs the OS driver and permissions. |
| `file_operations` | `lookup`, `getattr`, `readdir`, `create`, `mkdir`, `read`, `write`, `truncate`, `chmod`, `rename`, `unlink`, `rmdir`, `fsync`. These describe mounted filesystem operations. |
| `remote_configured` | Whether S3 or OpenCSG is currently enabled; a saved profile with `enabled: false` returns `false`. This flag does not test connectivity or credentials. |
| `automatic_text_merge` | `false`; conflicts require explicit resolution. |
| `physical_remote_gc` | `false`; automatic deletion of unreferenced historical objects in remote storage is disabled. |

File editing normally uses the mounted native path; the MCP tools manage workspaces, versions, and lifecycle. The catalog marks the eleven queries `readOnlyHint: true` and `destructiveHint: false`; commands and cancellation use the opposite values. All tools currently advertise `idempotentHint: true` and `openWorldHint: true`. These annotations describe tool behavior; authentication, grants, request identities, and state checks enforce access and retries.

Source definitions: [catalog and dispatch](../crates/agentdrive-mcp/src/server.rs), [request types](../crates/agentdrive-ports/src/api.rs), [shared types](../crates/agentdrive-model/src/entities.rs), and [error codes](../crates/agentdrive-model/src/error.rs).

Turn persistence is managed by [agent integrations](integrations.md) through the authenticated lifecycle API. Branches expose `source_branch` and `base_revision` for creation ancestry.

## Workspace relationships

```sh
agentdrive call workspace_relationships --json '{"workspace":"<workspace-id>","limit":50}'
```

The same query is available through `WorkspaceApi::relationships` and bearer-authenticated `GET /api/workspaces/{workspace}/relationships`. HTTP uses query parameters for the filters. `workspace` is required; all filters are optional. `owner_location` identifies the location owning the branch, while `mount_location` identifies the daemon hosting the mount. `mount_state` accepts `preparing`, `ready`, `paused`, `released`, or `recovery_required`.

`search` matches authorized branch names, local paths, principals, branch IDs, mount IDs and owner location IDs using case-insensitive substrings. `mounted: true` selects ready local bindings; `mounted: false` selects rows without a local binding or with a binding that is not ready. `include_released: true` includes retained released bindings. Filtering precedes counts and pagination.

Each entry contains `branch`, optional `mount` and `mount_location`. Branches without current local mounts have a row with `mount: null`. Supplying a mount ID, mount state or mount location restricts results to actual matching bindings. Default listings omit released bindings; an explicit mount ID or `mount_state: "released"` includes retained released bindings. `total` counts authorized matching rows. `selected_branch` reports the observing location's selection when the caller can read that branch.

`branch.ownership` includes `location`, `authority_epoch`, `generation` and its source: `local_authority` for the local owner's record, `remote_reference` for an observed remote reference, or `local_record` for a stored foreign-owner record without an applicable remote observation. Remote observations include `storage_version` and `observed_ns`; these can be null for records saved before observation tracking. Queries read locally retained state. Pull through `sync` to observe remote updates. `remote_mounts_known` and `remote_selections_known` are false; empty local results do not establish remote inventory or machine availability.

`branch.origin` includes the available source branch/name, base revision and captured creation working root. The captured root comes from the creation receipt or the workspace's initial revision and remains independent of later edits. `status` is `available`, `unavailable`, or `restricted`. Missing metadata retains known authorized IDs. Restricted origins omit source IDs, revision IDs and roots. Mount revision IDs are also omitted when the source revision is inaccessible. Workspace and branch read permissions govern all returned identities, paths, ancestry and counts.

`limit` defaults to 50 and accepts 1–200. Rows are ordered by branch ID and then mount ID. Supply `next` as `cursor` with the same filters and limit. The cursor is bound to the authorized view, observing location and principal. A changed view returns retryable `TARGET_MOVED` (HTTP 409); restart from the first page. The local adapter reads selection, branches, mounts, creation receipts, origin revisions and remote observations in one SQLite transaction. Relationship queries do not create mounts, revisions or publication operations.
