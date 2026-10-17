# Agent integrations and dashboard

[English](integrations.md) | [简体中文](integrations.zh-CN.md)

Start `agentdrived` and configure `AGENTDRIVE_TOKEN_FILE`. Native directories require FUSE, FUSE-T/macFUSE, or WinFsp. Mount paths must be inside the daemon's `mount_roots`. The agent's execution environment must be able to access the mounted directory.

## MCP and mounted directories

Configure an existing agent with the stdio MCP command `agentdrive mcp`, or connect to the daemon's `/mcp` HTTP endpoint. MCP connections share the long-running daemon.

```sh
agentdrive mount --workspace "$WORKSPACE" "$AGENTDRIVE_DIRECTORY/mounts/project"
```

The corresponding MCP tool is `mount_prepare`, with these minimum arguments:

```json
{
  "request_id": "mount-project-1",
  "request": {
    "workspace": "<workspace-id>",
    "path": "/absolute/path/to/project"
  }
}
```

A `location_id` belongs to the daemon data directory and survives restarts. Repeated mounts of a workspace at the same location reuse its selected branch. A new location creates and selects `location-<location-id>` from the workspace root branch's latest published saved revision. With S3 configured, mounting can import the remote workspace directly by ID. The creating location uses its already selected `main`. A missing or unpublished root branch produces an error.

Identical mount requests at the same path from the same principal return the existing binding, allowing multiple agents to share the directory. A different principal cannot reuse that binding, including one with `manage` permission. File edits and branch switches affect everyone using that directory. A branch permits one writable mount; explicitly unmount it before moving to another directory. Unmounting and agent disconnection preserve the selected branch. Explicit CLI or MCP branch creation provides separate working content.

MCP returns `result.binding`, including `id`, `branch`, `path`, and `generation`. Agents use the returned path for file operations. Connecting MCP does not change an agent's working directory or filesystem permissions. `mount_save` accepts `mount` and `expected_binding_generation` to save the mounted branch.

## Convert an existing directory

Use `workspace_adopt_directory` to mount an existing local directory at the same path. By default, it creates a workspace named after the directory, owned by the caller, with the original contents as the initial revision of `main`. Supply `request.workspace` to import into a new branch of an existing workspace; this requires `manage` permission. The path must be strictly below a configured `mount_roots` entry and within `directory_roots`; its parent must exist. Native mount support must be available. Existing mounts, including overlapping parent or child paths, must be released first.

Pause all file writes before calling this tool. Set `restart_agent: true` to acknowledge that the Agent and its terminals must restart after conversion. The tool does not terminate or launch those processes. Old working-directory references and file handles can still refer to the backup; do not perform further file operations through the running Agent after conversion.

```json
{
  "request_id": "adopt-project-1",
  "request": {
    "path": "/absolute/path/to/project",
    "restart_agent": true
  }
}
```

The tool imports the directory using the same validation as `workspace_import`, then moves the original directory to the sibling `.agentdrive-backup-<operation-id>/original` and mounts the imported branch at the original path. Unsupported files, including symbolic links, fail import before the directory is moved. When supplying a workspace ID, existing workspace content and the default branch selection are preserved; the new branch is explicitly mounted at `path`.

Success requires `phase: "complete"`, `mount_ready: true`, and no `error`. The operation returns the workspace ID in `workspace`. The result contains `branch`, `path`, `backup_path`, `binding`, and `restart_required: true`. Restart the Agent with the original path as its working directory. For later explicit mounting, use the returned branch ID. Keep the backup until the restarted Agent has verified its files; backup cleanup is manual.

If mounting fails after the move, the original files remain at `backup_path`; the original path may be empty. Pause file access and retry with the same request ID and identical arguments, including after daemon restart. A completed retry reuses the imported branch. Inspect `operation_get` or `operation_by_request` after a lost response. If import itself failed, correct the source and use a new request ID. Do not start another conversion on an already moved path.

## Application launch commands

Applications that need protocol-managed working directories and saving at task completion can use these commands:

```sh
agentdrive integrate --protocol acp --workspace "$WORKSPACE" \
  --mount-path "$AGENTDRIVE_DIRECTORY/mounts/project" -- your-acp-agent
```

```sh
agentdrive integrate --protocol codex-app-server --workspace "$WORKSPACE" \
  --mount-path "$AGENTDRIVE_DIRECTORY/mounts/project" -- codex app-server --listen stdio://
```

The bridge exchanges newline-delimited JSON-RPC, reuses the mount, and launches the agent after the directory is ready. ACP `session/new`, `session/load`, and `session/resume`, plus Codex `thread/start`, `thread/resume`, `thread/fork`, and `turn/start`, receive the mounted directory as `cwd`. Conversation IDs remain managed by the application.

## Saving and recovery

Autosave runs after approximately five seconds without changes or thirty seconds of continuous changes; the daemon checks once per second. A save captures the current files of the shared branch.

The bridge queries the current mount binding before each task and saves on completion, cancellation, failure, agent exit, or disconnection. Normal completion responses are forwarded after a successful local save. Save failures are reported. Retried requests retain their request ID, and duplicate protocol requests replay completed responses. Shared mounts remain available after disconnection.

Switching or restoring a branch can change the binding ID or generation. Delayed save requests fail with `STALE_BINDING`; the next task queries the current binding again. Reconnection after a daemon restart restores saved content and checks the native mount. Changes that never reached a save can be lost.

Custom applications can call bearer-authenticated `POST /api/lifecycle`:

| Event | Fields | Behavior |
| --- | --- | --- |
| `attach` | `request_id`, `workspace`, `mount_path` | Prepare or reuse the mount; return `binding` and `mount_path`. |
| `check` | `request_id`, `workspace`, `mount_path` | Query the current writable binding at the normalized path returned by attach. |
| `save` | `request_id`, `mount`, `expected_binding_generation` | Save that mount generation and return its operation record. |

The daemon publishes remotely in the background. `remote_confirmed` indicates successful remote publication of the operation.

## Branches and dashboard

`agentdrive branch create` and MCP `branch_create` explicitly create branches while preserving the selection. `agentdrive branch switch` and MCP `branch_switch` save the previous branch and update this location's workspace selection. The CLI attempts release of occupied mounts. Close open files and processes using the directory before retrying the original request ID. MCP callers continue pending changes with `mount_release`.

Existing databases retain their branches and selections. With no selection, one writable local branch is reused automatically; multiple writable local branches require an explicit `branch_switch`.

Run `agentdrive dashboard --open` to open the daemon page. The browser reads the token from the URL fragment, removes it, and retains it in session storage. Manual token entry is also available. The page refreshes every 15 seconds and shows branch ancestry, revision history, and commit details. Selecting a branch changes the displayed history.

Read-only endpoints are `GET /api/workspaces` and `GET /api/workspaces/{workspace}`. The latter returns `status` and `revisions`; `selected_branch` is this location's selection. Administrators edit runtime settings at `/settings`; see [configuration](configuration.md).

Protocol references: [ACP sessions](https://agentclientprotocol.com/protocol/v1/session-setup), [ACP turns](https://agentclientprotocol.com/protocol/v1/prompt-turn), and [Codex app-server](https://developers.openai.com/codex/app-server/).

## macOS FUSE-T

Install FUSE-T 1.2.9 and pkg-config, then build with `make -j "$(sysctl -n hw.ncpu)" build FEATURES=macos-fuse-t`. AgentDrive uses the local NFS backend and disables its attribute cache. FUSE-T runs without a kernel extension. Its [license](https://github.com/macos-fuse-t/fuse-t) requires a commercial license for commercial use or distribution with commercial software.

macOS can create AppleDouble `._` metadata files on this filesystem; AgentDrive stores them as ordinary workspace files. With an existing mount, `branch_switch` always returns `waiting_for_quiesce`; call `mount_release` to complete the change. Release open handles and processes using the mount before continuing. A running Agent or its MCP stdio proxy can keep the directory occupied: stop that Agent, run `mount_release` from a process outside the mount, and restart the Agent at the same path. Configure the MCP proxy's working directory outside the mount when the client supports it.

File writes and `fsync` follow the binding's durability setting. With the default `local` setting, use `mount_save` with `durability: "remote"` and check `remote_confirmed` to confirm publication to RustFS. A successful local file write alone does not confirm remote publication.
