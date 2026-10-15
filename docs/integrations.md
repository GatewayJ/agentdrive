# Agent integrations and dashboard

[English](integrations.md) | [简体中文](integrations.zh-CN.md)

Start `agentfsd`, configure `AGENTFS_TOKEN_FILE`, and create a workspace. Native integration requires FUSE, macFUSE, or WinFsp. The mount path must be absolute and within the daemon's configured `mount_roots`. Run the bridge on the daemon machine, under the same OS account.

## Application commands

Configure an ACP client's agent command as:

```sh
agentfs integrate --protocol acp --workspace "$WORKSPACE" \
  --session-id editor-project --mount-path "$AGENTFS_DIRECTORY/mounts/project" \
  -- your-acp-agent
```

Configure a client that launches Codex app-server over stdio as:

```sh
agentfs integrate --protocol codex-app-server --workspace "$WORKSPACE" \
  --session-id codex-project --mount-path "$AGENTFS_DIRECTORY/mounts/project" \
  -- codex app-server --listen stdio://
```

The application exchanges newline-delimited JSON-RPC with the bridge. Keep `--session-id` stable across reconnects. The ACP or Codex conversation ID remains managed by the application. One bridge manages one OpenCSG Workspace session and permits one active turn at a time. A per-session process lock prevents concurrent bridges under the same OS account. Standard output carries protocol messages; diagnostics use standard error.

The bridge attaches the OpenCSG Workspace session and waits for the native mount before launching the agent. It sets `cwd` in ACP `session/new`, `session/load`, and `session/resume`, and Codex `thread/start`, `thread/resume`, `thread/fork`, and `turn/start`. Every new OpenCSG Workspace session uses the workspace's selected branch. Existing sessions resume their branch; explicit workspace branch switches update those attachments.

## Persistence and recovery

ACP `session/prompt` starts a tracked turn. Its response saves the result before the response reaches the application. `end_turn`, `max_tokens`, `max_turn_requests`, and `refusal` record completion; `cancelled` records cancellation; an RPC error records failure. `session/cancel` is forwarded, and persistence waits for the prompt's terminal result.

Codex `turn/start` and `review/start` begin tracking before forwarding. `turn/started` associates the server's turn ID; `turn/completed` saves `completed`, `failed`, or `interrupted` results before forwarding. Protocol interruption records cancellation. An unexpected agent exit or application disconnect records an interrupted turn, pauses the session, and unmounts after saving.

Autosave continues with a five-second quiet period or thirty-second continuous-write deadline. HTTP lifecycle retries keep the same request ID, including when a response body is lost. Duplicate in-flight requests are forwarded once; recent completed responses are replayed from a bounded connection cache. Duplicate completion notifications preserve the original turn result. Reusing a cached RPC ID with different parameters returns an error. Requests submitted after a new connection are new protocol requests.

On reconnect, the bridge resumes the session and records an unfinished previous turn as interrupted. After a daemon restart, the same recovery uses the persisted turn identity. A failed save is reported instead of forwarding a successful completion. File handles that prevent unmount must be closed before retrying attachment.

Custom application integrations can use bearer-authenticated `POST /api/lifecycle` with `{request_id, event, key, ...}`. `key` is the standard `SessionKey`. Events are `attach` with `mount_path`, `begin` with `turn_id`, `end` with `turn_id` and terminal `status`, and `detach`. Use stable request IDs for retries. End events commit locally; remote publication proceeds through the daemon's existing replication worker. `turn_begin` and `turn_end` remain internal service operations and are absent from the MCP catalog.

## Explicit branches and existing state

`agentfs branch create` and MCP `branch_create` create a branch from current working content or an explicit revision. Creation preserves the selected branch. `agentfs branch switch` and MCP `branch_switch` save the previous branch and update the persisted selection. Each branch permits one active turn across its sessions. Active turns must complete before switching. The CLI attempts mount release immediately. Busy mounts return an error and keep the pending switch; retry with its original request ID after closing handles and leaving the directory. MCP callers continue pending switches with `mount_release`. A pending switch prevents new turns until it completes or is cancelled.

New workspaces have a writable, selected `main`. Existing databases keep their branch protection settings. An older workspace without a saved selection must explicitly select a writable branch before opening a default session. Create one from `--source <REVISION>` or `--source-branch <BRANCH>` when needed. Old revision objects remain readable; commit messages are optional. Creation ancestry is retained for new local branches; older and remotely adopted branches can have an unknown source.

## Dashboard

Run `agentfs dashboard --open` to open the daemon's root URL. The browser receives the token in a URL fragment, removes it immediately, and retains it in session storage. Alternatively open the URL and enter the daemon token. Workspace data follows existing grants. The page refreshes every 15 seconds and provides workspace selection, a branch creation tree, reachable revision history and commit details. Selecting a branch changes only the displayed history.

Read-only JSON endpoints are `GET /api/workspaces` and `GET /api/workspaces/{workspace}`. The latter returns `status` and `revisions`. Revision details distinguish the revision's original branch from the branch whose history is being browsed. Parent IDs preserve merge ancestry.

Protocol references: [ACP sessions](https://agentclientprotocol.com/protocol/v1/session-setup), [ACP prompt turns](https://agentclientprotocol.com/protocol/v1/prompt-turn), and [Codex app-server](https://developers.openai.com/codex/app-server/).

Administrators can open **Settings** at `/settings` to edit runtime configuration and inspect startup settings; see [configuration](configuration.md).
