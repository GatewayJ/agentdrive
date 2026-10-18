# AgentDrive

[English](README.md) | [简体中文](README.zh-CN.md)

AgentDrive gives agents durable workspaces, explicit workspace branches, immutable revisions, and native filesystem paths. A long-running Rust daemon owns local state and mounts. Concurrent MCP clients and the CLI share its authenticated API.

The implementation includes:

- Portable file names, content-addressed objects, paged directory/inode indexes, and SQLite transactions.
- Linux FUSE, macOS FUSE-T/macFUSE, and Windows WinFsp adapters; snapshots, branch restore, and revision history.
- S3/RustFS replication, bounded streaming transfers, offline cache pins, online ownership transfer, and editable three-way merge candidates.
- ACP and Codex app-server lifecycle bridges, explicit branch commands, and a dashboard for branch history and runtime settings.
- 31 typed MCP tools over authenticated Streamable HTTP, with a stdio proxy and JSON CLI.

## Build

Use Rust stable (minimum declared version 1.90), a C compiler for bundled SQLite, and GNU Make. Build jobs use Make's jobserver.

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" build
make -j "$(getconf _NPROCESSORS_ONLN)" verify
```

| Platform | Native filesystem requirements | Build |
| --- | --- | --- |
| Linux | FUSE 3 utilities, `/dev/fuse`, permission to mount | Default features |
| macOS | [FUSE-T](https://github.com/macos-fuse-t/fuse-t) and `pkg-config` | `make -j "$(sysctl -n hw.ncpu)" build FEATURES=macos-fuse-t` |
| Windows | MSVC, [WinFsp 2.1](https://github.com/winfsp/winfsp/releases/tag/v2.1) including its Developer feature | PowerShell: `make -j $env:NUMBER_OF_PROCESSORS build` |

The default macOS build provides the daemon, CLI, and storage operations. Enable `macos-fuse-t` for native mounts without a kernel extension. Make selects the FUSE-T library; direct Cargo builds must set `PKG_CONFIG_PATH="$PWD/build/fuse-t"`. Existing macFUSE installations can use `FEATURES=macos-mount`. The Windows build loads WinFsp from its registered installation path. Use a local NTFS volume for Windows state.

## Run

```sh
AGENTDRIVE_DIRECTORY="$HOME/.local/share/agentdrive"
./target/debug/agentdrived init --directory "$AGENTDRIVE_DIRECTORY"
./target/debug/agentdrived serve --config "$AGENTDRIVE_DIRECTORY/agentdrive.json"
```

In another terminal:

```sh
export AGENTDRIVE_TOKEN_FILE="$HOME/.local/share/agentdrive/token"
./target/debug/agentdrive tools
./target/debug/agentdrive call workspace_create --request-id create-demo --json '{"name":"demo"}'
./target/debug/agentdrive call workspace_list
```

`init` creates configuration, a random bearer token, and directories for state, mounts, and file exchange. Existing credentials are preserved by refusing initialization over an existing configuration. The daemon defaults to `127.0.0.1:7421`; its MCP endpoint is `/mcp`. Run one daemon per state directory.

To connect an MCP client through stdio, configure the executable as `agentdrive`, arguments as `["mcp"]`, and set `AGENTDRIVE_TOKEN_FILE`. `AGENTDRIVE_ENDPOINT` selects another daemon endpoint. The stdio process forwards requests to the running daemon.

Use `agentdrive tools` as the source for JSON input schemas. Command tools accept `{ "request_id": "stable-id", "request": { ... } }` over MCP. The CLI's `--request-id` option builds that envelope. Query tools and `operation_cancel` receive their arguments directly. `--json -` reads JSON from standard input. The CLI exits with 0 for successful tool responses, 2 for a tool error, and 1 for transport or input errors; command-line parsing errors also use 2. See [CLI usage](docs/cli.md) for executable examples and [MCP parameters and capabilities](docs/mcp.md) for the complete tool reference.

## Branches, agents, and dashboard

A new workspace selects its writable `main` branch. Mounting the same workspace at the same location reuses its selected branch. A new location creates and selects its own branch from the root branch's latest published content. Create and switch branches explicitly through MCP (`branch_create`, `branch_switch`) or the CLI:

```sh
agentdrive mount --workspace "$WORKSPACE" "$AGENTDRIVE_DIRECTORY/mounts/project"
agentdrive branch list --workspace "$WORKSPACE"
agentdrive branch create --workspace "$WORKSPACE" --name feat/task
agentdrive branch switch --workspace "$WORKSPACE" --branch "$BRANCH"
agentdrive dashboard --open
```

Creation captures current working content without changing the selection. Switching saves the previous branch, checks native file handles, and updates this location's selection and mount. Agents sharing the directory use the same branch. The selection survives daemon restarts. The dashboard shows workspace statistics, branch origins connected to their exact source revisions, and local mount paths with their branches and access modes. Version dialogs show metadata, changed files and text differences. Administrators can use **Settings** to update cache policy, S3 storage and allowed HTTP origins while the daemon is running; see [configuration](docs/configuration.md).

Existing agents can connect through `agentdrive mcp` and use a mounted directory. `agentdrive integrate` manages the working directory and saves on completion, cancellation, failure, and reconnect. See [agent integrations](docs/integrations.md) for ACP and Codex app-server commands and protocol behavior. File changes trigger automatic saves, and shared mounts remain available after agent disconnection.

The branch owner, epoch, generation, and head are checked for guarded changes. Obtain current values from `workspace_status`; do not manufacture them. `branch_switch` attempts the complete switch in one call; on `MOUNT_BUSY`, close processes using the directory or restart the Agent, then retry with the same request ID. Restore and merge operations that need an active mount to stop return `waiting_for_quiesce`. Stop processes using that directory, close their handles and current working directories, then call `mount_release` with the operation ID and expected binding generation. See [operations and recovery](docs/operations.md).

## Storage and configuration

`agentdrive.json` uses absolute filesystem paths. `mount_roots` permits mount destinations; `directory_roots` permits import and export paths. Imports require a stable source directory and reject symbolic links and incompatible names. Exports create a new directory and preserve file bytes, modification times, and supported permissions.

Enable S3 and save its connection settings and credentials in Dashboard **Settings**, or add `remote` to the generated configuration:

```json
{
  "remote": {
    "bucket": "agentdrive",
    "prefix": "agentdrive/v1",
    "region": "us-east-1",
    "endpoint": "https://s3.example.com",
    "allow_http": false
  }
}
```

Credentials can be saved through Dashboard Settings or supplied through the AWS environment/role providers supported by `object_store`. The bucket must already exist and support conditional object writes, conditional multipart completion, and strongly consistent reads. Restrict credentials to the selected bucket/prefix. Local development endpoints may explicitly enable `allow_http`.

Switching between local and S3 storage takes effect without restarting: enabling S3 copies retained history, and returning to local storage downloads missing history. Preparation or configuration-save failures preserve the active connection. `meta.db` is a local SQLite database and is never uploaded as a file. S3 stores file contents, directory/inode indexes, revision history and branch references; pulling imports the published records into each machine's own database. Location identity, branch selection, mounts, cache state and working files stay local.

See [configuration and S3](docs/configuration.md) for every field, AWS S3/RustFS examples, supported credential sources, environment precedence, and multi-machine setup.

S3 mode exposes one **Local cache capacity** setting, default 2 GiB (`cache.max_bytes` in the configuration file, in bytes). Collection runs every 30 seconds and, when capacity is exceeded, targets 75% of that capacity. Low disk space also triggers collection. Current branch roots, pending publications, merge candidates, active readers and explicit pins remain protected; usage can exceed the setting. Only remotely confirmed historical objects are eligible. Local mode retains all history and hides the cache setting. This capacity applies to immutable object files; SQLite metadata and working files occupy additional space. `cache_status.offline_ready` requires a complete pinned selection.

## Validation and scope

CI runs workspace tests on Linux, macOS, and Windows; native mount tests on Linux and Windows; macFUSE callback compilation; CLI/stdio integration; and a real RustFS backend test. The native mount test covers multi-megabyte writes, truncation, append, rename with an open handle, directory enumeration, fsync, historical read-only mounts, and unmount. The same native tests pass on macOS 26.5 with FUSE-T 1.2.9. See [validation](docs/validation.md) for commands and current execution evidence.

File operations cover regular files and directories, atomic branch append, rename, permissions, timestamps, truncate, and fsync. File contents are stored as whole-file immutable objects; edited files are resealed by streaming. Metadata trees are materialized in memory for branch and merge operations. The configurable maximum is one million inodes per workspace; size limits should reflect available memory and disk.

The advertised capability response describes the supported behavior. Symbolic links, hard links, extended attributes, memory-mapped writes, advisory locking, automatic text conflict resolution, and physical remote garbage collection are outside the implemented interface. Unlinking an open file or replacing an open destination returns a busy error. Concurrent changes to the same path become explicit merge conflicts. Mounts are owned by the daemon's OS identity; workspace access grants govern MCP requests.

See the [documentation index](docs/README.md), [architecture](docs/architecture.md), [operations and recovery](docs/operations.md), and [validation](docs/validation.md). AgentDrive is licensed under Apache-2.0; the vendored WinFsp Rust wrapper retains its MIT license.
