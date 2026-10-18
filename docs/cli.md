# Command-line usage

[English](cli.md) | [简体中文](cli.zh-CN.md)

## Start the daemon

Build from the repository root using [README](../README.md). The examples below use POSIX shell and `jq` to construct JSON and read returned IDs. Add the built binaries to `PATH` in each terminal:

```sh
export PATH="$PWD/target/debug:$PATH"
export AGENTDRIVE_DIRECTORY="$HOME/.local/share/agentdrive"
agentdrived init --directory "$AGENTDRIVE_DIRECTORY"
agentdrived serve --config "$AGENTDRIVE_DIRECTORY/agentdrive.json"
```

Run `init` once for a new directory. It prints the generated configuration path and rejects an existing configuration or token. `serve` runs in the foreground; Ctrl+C requests shutdown. S3 credentials can be saved through Dashboard Settings. When using environment credentials, set them in this terminal before `serve`; see [remote configuration](configuration.md).

| Command | Arguments |
| --- | --- |
| `agentdrived init` | Required `--directory <PATH>`; optional `--listen <IP:PORT>`, default `127.0.0.1:7421`. |
| `agentdrived serve` | Required `--config <PATH>`. |
| `agentdrive tools` | Lists the tool catalog, including `inputSchema` and annotations, as JSON. |
| `agentdrive call <TOOL>` | Optional `--json <JSON>` (default `{}`) and `--request-id <ID>`. |
| `agentdrive mcp` | Runs a stdio MCP proxy connected to the existing daemon. |
| `agentdrive mount` | Required `--workspace <ID>` and directory; optional `--request-id <ID>`. Relative paths resolve from the CLI working directory. |

Both binaries provide `--help` and `--version`; subcommands provide `--help`.

## Client connection and output

Place connection options **before the subcommand**:

```sh
agentdrive --endpoint http://127.0.0.1:7421/mcp --token-file "$AGENTDRIVE_DIRECTORY/token" tools
```

| Option / environment | Behavior |
| --- | --- |
| `--endpoint` / `AGENTDRIVE_ENDPOINT` | Full MCP URL, including `/mcp`; default `http://127.0.0.1:7421/mcp`. The option overrides the environment. |
| `--token-file` / `AGENTDRIVE_TOKEN_FILE` | Reads a token file and trims surrounding whitespace. The option overrides the environment. |
| `AGENTDRIVE_TOKEN` | Token fallback when no token file is selected. A selected but unreadable token file causes an error. |
| `RUST_LOG` | Log filter; the client defaults to `warn`. Logs use stderr. |

The daemon must already be running. Host paths in tool arguments, such as import sources and mount paths, refer to the daemon machine. `--token-file` and standard input refer to the client machine.

`tools` prints the MCP catalog object with a `tools` array. `call` prints the tool's `structuredContent` as JSON. Errors have `error.code`, `error.message`, and `error.retryable`; an operation receipt can contain both a saved result and an error. For operations, inspect `phase`, `local_saved`, `remote_confirmed`, and `mount_ready` independently.

The client exits with 0 when the tool response has no error, 2 for a tool error, and 1 for transport, authentication, token-reading, or JSON input errors. Command-line parsing errors also use exit code 2. Exit code 0 can accompany `waiting_for_quiesce`; inspect the receipt before continuing dependent work.

`--json -` reads standard input, with an 8 MiB input limit. The HTTP request body also has an 8 MiB limit, including its MCP envelope. `--request-id` wraps the supplied JSON as `{ "request_id": "...", "request": { ... } }`. Use it for the command tools listed in the [MCP reference](mcp.md). Queries and `operation_cancel` receive direct arguments. Keep the same request ID and arguments for retries; a new intended operation needs a new request ID.

## Create, import, read, and export

In another terminal, set the same `PATH` and `AGENTDRIVE_DIRECTORY` as above. Use a fresh `RUN_ID` and source/export paths for a separate demonstration. This example works with the default macOS build and does not require a native mount.

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

Creation returns `result.workspace`. Import creates a new branch and returns `result.branch`; its `formal_head` is the imported revision. New workspaces select a writable `main` branch. `revision_read` returns `{ "bytes": [0, 1, ...], "length": ... }`, with integer bytes and a maximum requested size of 4 MiB. Export requires an existing parent directory and a destination that does not already exist.

## Mounts and branches

```sh
agentdrive mount --workspace "$WORKSPACE" "$AGENTDRIVE_DIRECTORY/mounts/project"
agentdrive branch list --workspace "$WORKSPACE"
agentdrive branch create --workspace "$WORKSPACE" --name feat/task --request-id "$RUN_ID-branch"
agentdrive branch switch --workspace "$WORKSPACE" --branch "$BRANCH" --request-id "$RUN_ID-switch"
agentdrive dashboard --open
```

`branch create` accepts optional `--source <REVISION>` or `--source-branch <BRANCH>`; omission captures the selected branch's current working content. It returns the new branch in `result.branch` and preserves the current selection. `branch switch` changes this location's workspace selection and mounted directory. CLI and MCP complete the switch in one call. On `MOUNT_BUSY`, close files and processes using the directory or restart the Agent, then retry with the original request ID. Other errors preserve their specific cause. Both commands generate a request ID when omitted; reuse an explicit ID for retries.

`mount` takes a workspace ID and directory; repeated calls reuse the mount. The same location uses its selected branch. A new location creates its branch from the root branch's latest published revision. With S3 configured, a remote workspace ID can be mounted directly. A branch permits one writable mount; unmount it before moving directories. Agent disconnection preserves the directory. See [agent integrations](integrations.md) for MCP setup and task saving.

`dashboard` prints the browser URL without reading credentials or connecting to the daemon. `dashboard --open` requires credentials and opens the URL with the selected token in a URL fragment; the page removes the fragment immediately and keeps the token in session storage. Data requests use bearer authentication. The dashboard provides branch trees and revision details, with refresh every 15 seconds. Administrators can open **Settings** at `/settings` to edit runtime configuration and inspect startup settings; see [configuration](configuration.md).

The cards show active branches, local mount bindings, distinct branches used by ready mounts, and remote synchronization for the selected workspace. Released bindings are excluded; preparing, paused and recovery-required bindings remain visible with their state. The branch graph connects each branch to its exact creation revision using `source_branch` and `base_revision`; source revisions can be commits, autosaves or checkpoints. Click a source revision to inspect it. **Default** marks the local selection for requests that omit a branch; mounted badges count ready directories. The mount table lists full paths, branches, access modes and states, with branch links to their history.

Each history row has a **More (⋯)** menu. **Version information** opens revision metadata in a dialog; **Changes** opens a paginated changed-file list and automatically displays the first entry’s details and text differences on the right. Loading more entries preserves the current selection. Close the dialog with its close button, Escape, or a click outside it. Comparison always uses the parent revision, or the first parent for a merge. Initial revisions compare against an empty tree. The previous version is determined by parent relationships. Exact-content moves with a unique source and destination are marked as renames. Ambiguous moves and moves with edited content appear as additions/deletions. Added and deleted empty files are explicitly labeled as empty (0 bytes); unchanged content from a rename or attribute change is labeled separately. Binary and oversized files show metadata. Browsing does not change mounts or the selected working branch.

## Read a saved request from a file

JSON files contain the tool request fields. For example, `create-request.json` can contain `{ "name": "another-workspace" }`:

```sh
agentdrive call workspace_create --request-id create-from-file --json - < create-request.json
```

In PowerShell:

```powershell
$env:AGENTDRIVE_TOKEN_FILE = 'C:\Users\alice\agentdrive\token'
Get-Content -Raw .\create-request.json | .\target\debug\agentdrive.exe call workspace_create --request-id create-from-file --json -
```

Use [S3 configuration](configuration.md) for push/pull examples and [MCP parameters](mcp.md) for guards, cache ranges, merges, and receipt recovery.
