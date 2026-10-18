# Installation and startup

[English](installation.md) | [简体中文](installation.zh-CN.md)

Clone the repository and enter its directory:

```sh
git clone https://github.com/GatewayJ/agentdrive.git
cd agentdrive
```

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

Use `agentdrive tools` as the source for JSON input schemas. Command tools accept `{ "request_id": "stable-id", "request": { ... } }` over MCP. The CLI's `--request-id` option builds that envelope. Query tools and `operation_cancel` receive their arguments directly. `--json -` reads JSON from standard input. The CLI exits with 0 for successful tool responses, 2 for a tool error, and 1 for transport or input errors; command-line parsing errors also use 2. See [CLI usage](cli.md) for executable examples and [MCP parameters and capabilities](mcp.md) for the complete tool reference.


Open the dashboard with `./target/debug/agentdrive dashboard --open`. Its default landing page is the Archive. Before adopting an existing project directory, configure its parent under `directory_roots` and `mount_roots`; see [directory conversion](integrations.md#convert-an-existing-directory).
