# OpenCSG Workspace

[English](README.md) | [简体中文](README.zh-CN.md)

OpenCSG Workspace gives agents durable workspaces, explicit workspace branches, immutable revisions, and native filesystem paths. A long-running Rust daemon owns local state and mounts. Concurrent MCP clients and the CLI share its authenticated API.

The implementation includes:

- Portable file names, content-addressed objects, paged directory/inode indexes, and SQLite transactions.
- Linux FUSE, macOS macFUSE, and Windows WinFsp adapters; snapshots, branch restore, session and turn history.
- S3/RustFS replication, bounded streaming transfers, offline cache pins, online ownership transfer, and editable three-way merge candidates.
- ACP and Codex app-server lifecycle bridges, explicit branch commands, and a dashboard for branch history and runtime settings.
- 32 typed MCP tools over authenticated Streamable HTTP, with a stdio proxy and JSON CLI.

## Build

Use Rust stable (minimum declared version 1.90), a C compiler for bundled SQLite, and GNU Make. Build jobs use Make's jobserver.

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" build
make -j "$(getconf _NPROCESSORS_ONLN)" verify
```

| Platform | Native filesystem requirements | Build |
| --- | --- | --- |
| Linux | FUSE 3 utilities, `/dev/fuse`, permission to mount | Default features |
| macOS | [macFUSE](https://macfuse.github.io/) installed and enabled, `pkg-config` | `make -j "$(sysctl -n hw.ncpu)" build FEATURES=macos-mount` |
| Windows | MSVC, [WinFsp 2.1](https://github.com/winfsp/winfsp/releases/tag/v2.1) including its Developer feature | PowerShell: `make -j $env:NUMBER_OF_PROCESSORS build` |

The default macOS build provides the daemon, CLI, and storage operations. Enable `macos-mount` for native mounts. The Windows build loads WinFsp from its registered installation path. Use a local NTFS volume for Windows state.

## Run

```sh
AGENTFS_DIRECTORY="$HOME/.local/share/agentfs"
./target/debug/agentfsd init --directory "$AGENTFS_DIRECTORY"
./target/debug/agentfsd serve --config "$AGENTFS_DIRECTORY/agentfs.json"
```

In another terminal:

```sh
export AGENTFS_TOKEN_FILE="$HOME/.local/share/agentfs/token"
./target/debug/agentfs tools
./target/debug/agentfs call workspace_create --request-id create-demo --json '{"name":"demo"}'
./target/debug/agentfs call workspace_list
```

`init` creates configuration, a random bearer token, and directories for state, mounts, and file exchange. Existing credentials are preserved by refusing initialization over an existing configuration. The daemon defaults to `127.0.0.1:7421`; its MCP endpoint is `/mcp`. Run one daemon per state directory.

To connect an MCP client through stdio, configure the executable as `agentfs`, arguments as `["mcp"]`, and set `AGENTFS_TOKEN_FILE`. `AGENTFS_ENDPOINT` selects another daemon endpoint. The stdio process forwards requests to the running daemon.

Use `agentfs tools` as the source for JSON input schemas. Command tools accept `{ "request_id": "stable-id", "request": { ... } }` over MCP. The CLI's `--request-id` option builds that envelope. Query tools and `operation_cancel` receive their arguments directly. `--json -` reads JSON from standard input. The CLI exits with 0 for successful tool responses, 2 for a tool error, and 1 for transport or input errors; command-line parsing errors also use 2. See [CLI usage](docs/cli.md) for executable examples and [MCP parameters and capabilities](docs/mcp.md) for the complete tool reference.

## Branches, agents, and dashboard

A new workspace selects its writable `main` branch. Sessions attach to the selected branch. Create and switch branches explicitly through MCP (`branch_create`, `branch_switch`) or the CLI:

```sh
agentfs branch list --workspace "$WORKSPACE"
agentfs branch create --workspace "$WORKSPACE" --name feat/task
agentfs branch switch --workspace "$WORKSPACE" --branch "$BRANCH"
agentfs dashboard --open
```

Creation captures current working content without changing the selection. Switching saves the previous branch, checks active turns and native file handles, and updates the workspace selection and attached sessions. The selection survives daemon restarts. The dashboard shows branch ancestry, commit messages, authors, timestamps, and parent revisions. Administrators can use **Settings** to update cache policy, S3 storage and allowed HTTP origins while the daemon is running; see [configuration](docs/configuration.md).

Configure the agent application's stdio command through `agentfs integrate`. It manages session attachment, the working directory, turn boundaries, cancellation, and reconnect recovery. See [agent integrations](docs/integrations.md) for ACP and Codex app-server commands and protocol behavior. Automatic saves remain enabled throughout each turn.

The branch owner, epoch, generation, and head are checked for guarded changes. Obtain current values from `workspace_status`; do not manufacture them. Operations that need an active mount to stop return `waiting_for_quiesce`. Stop processes using that directory, close their handles and current working directories, then call `mount_release` with the operation ID and expected binding generation. See [operations and recovery](docs/operations.md).

## Storage and configuration

`agentfs.json` uses absolute filesystem paths. `mount_roots` permits mount destinations; `directory_roots` permits import and export paths. Imports require a stable source directory and reject symbolic links and incompatible names. Exports create a new directory and preserve file bytes, modification times, and supported permissions.

To enable replication, set `remote` in the generated configuration:

```json
{
  "bucket": "agentfs",
  "prefix": "agentfs/v1",
  "region": "us-east-1",
  "endpoint": "https://s3.example.com",
  "allow_http": false
}
```

Supply credentials using the AWS environment/provider configuration supported by `object_store`. The bucket must already exist and support conditional object writes, conditional multipart completion, and strongly consistent reads. Restrict credentials to the selected bucket/prefix. Local development endpoints may explicitly enable `allow_http`.

See [configuration and S3](docs/configuration.md) for every field, AWS S3/RustFS examples, supported credential sources, environment precedence, and multi-machine setup.

The cache defaults to a 2 GiB limit and a 1.5 GiB collection target. Current branch roots, pending publications, merge candidates, and explicit pins remain protected. Historical file content confirmed remotely can be evicted. `cache_status.offline_ready` requires a complete pinned selection. Protected data can exceed the cache target; the workspace capacity limits still apply.

## Validation and scope

CI runs workspace tests on Linux, macOS, and Windows; native mount tests on Linux and Windows; macFUSE callback compilation; CLI/stdio integration; and a real RustFS backend test. The native mount test covers multi-megabyte writes, truncation, append, rename with an open handle, directory enumeration, fsync, historical read-only mounts, and unmount. macOS kernel mounting requires an enabled macFUSE installation and is verified manually with the same test. See [validation](docs/validation.md) for commands and current execution evidence.

File operations cover regular files and directories, atomic branch append, rename, permissions, timestamps, truncate, and fsync. File contents are stored as whole-file immutable objects; edited files are resealed by streaming. Metadata trees are materialized in memory for branch and merge operations. The configurable maximum is one million inodes per workspace; size limits should reflect available memory and disk.

The advertised capability response describes the supported behavior. Symbolic links, hard links, extended attributes, memory-mapped writes, advisory locking, automatic text conflict resolution, physical remote garbage collection, and global session-location arbitration are outside the implemented interface. Unlinking an open file or replacing an open destination returns a busy error. Concurrent changes to the same path become explicit merge conflicts. Mounts are owned by the daemon's OS identity; workspace access grants govern MCP requests.

See the [documentation index](docs/README.md), [architecture](docs/architecture.md), [operations and recovery](docs/operations.md), and [validation](docs/validation.md). OpenCSG Workspace is licensed under Apache-2.0; the vendored WinFsp Rust wrapper retains its MIT license.
