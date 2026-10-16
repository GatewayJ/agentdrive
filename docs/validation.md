# Validation

[English](validation.md) | [简体中文](validation.zh-CN.md)

Run all standard checks through multicore Make:

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" verify
make -j "$(getconf _NPROCESSORS_ONLN)" build
python3 tests/cli_smoke.py
```

The workflow tests exercise concurrent request retries, atomic append, rename with open handles, restart recovery, idempotent mount saves, concurrent mount reuse, branch isolation across locations, autosave timing, remote failure and lost acknowledgements, ownership transfer, merge file edits, stale mount changes, cache pinning/eviction, and offline prefetch. Persistence tests cover digest failures, reader leases, transaction rollback, request recovery, and unknown format rejection. MCP tests verify concurrent clients, principal isolation, authentication, and Host/Origin policy. The CLI test starts real processes and verifies stdio MCP, import/export content and metadata, allowed paths, and process restart.

## Native filesystems

Install and enable the platform driver, then run:

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" test CARGO_FLAGS='-p agentdrive-platform --test native_mount -- --ignored --nocapture'
```

On macOS add `--features macos-mount` inside `CARGO_FLAGS`. On Windows use `make -j $env:NUMBER_OF_PROCESSORS` in PowerShell. The integration test owns its temporary state and mount directories. It tests a file over 3 MiB, truncation, random write, atomic append, rename, fsync, over 128 directory entries, historical read-only access, and unmount.

The WinFsp wrapper's null volume-flush regression is tested separately on Windows:

```powershell
make -j $env:NUMBER_OF_PROCESSORS test CARGO_FLAGS='--manifest-path vendor/winfsp_wrs/Cargo.toml --lib'
```

## S3 / RustFS

Use a disposable endpoint and credentials permitted to create the `agentdrive-integration` bucket. Test objects use unique prefixes. Do not point these tests at production storage.

```sh
export AGENTDRIVE_TEST_S3_ENDPOINT=http://127.0.0.1:9000
export AWS_ACCESS_KEY_ID=AGENTDRIVETESTLOCAL
export AWS_SECRET_ACCESS_KEY=agentdrive-local-integration-secret-2026
make -j "$(getconf _NPROCESSORS_ONLN)" test CARGO_FLAGS='-p agentdrive-s3 -- --ignored --nocapture'
python3 tests/replica_smoke.py
python3 tests/storage_smoke.py
```

Backend tests exercise competing conditional reference creation, stale conditional updates, empty files, digest rejection, and 20 MiB multipart object publication. The replication test pushes an imported revision, pulls and exports it at another location, and transfers branch ownership through the real S3 backend. With a native driver, it also runs three locations against one workspace: a fresh location imports the workspace automatically, creates its own branch from the published root, reuses that branch and binding on repeated mounts, and writes files independently.

`storage_smoke.py` verifies live local/S3 transitions without changing the daemon PID, persisted credentials, failed-transition recovery, file-based changes, historical export, and restart recovery.

## Execution evidence

| Check | Verified environment |
| --- | --- |
| Workspace tests and Clippy | macOS, Linux, Windows |
| Native mounted file operations and revision access | Linux as root and as an ordinary user; Windows with WinFsp 2.1 |
| CLI, stdio MCP, directory metadata, restart recovery | macOS, Linux, Windows |
| Conditional S3 writes and multipart content | Isolated RustFS instance |
| Two-daemon S3 replication and ownership transfer | macOS and Linux |
| macFUSE callback compilation | macOS |

[GitHub Actions](https://github.com/GatewayJ/agentdrive/actions) runs the checked-in test matrix for each commit. Native macOS mounting requires an installed and enabled macFUSE driver; that kernel mount test has not been executed in the development environment.

The create/import/read/export/mount examples in [CLI usage](cli.md) were executed against a temporary daemon. The [MCP parameter tables](mcp.md) were checked against the running daemon's 29 tool schemas. S3 configuration was checked against the daemon config loader, adapter, and credential/endpoint selection in the locked `object_store` dependency. These documentation checks do not add an AWS S3 deployment to the verified environments above.

Additional lifecycle, branch, and dashboard checks:

```sh
python3 tests/lifecycle_smoke.py
python3 tests/config_smoke.py
```

`lifecycle_smoke.py` requires native mounting and covers ACP and Codex app-server completion, cancellation, failures, process interruption, duplicate requests, autosave, daemon restart, reconnect, two Agent connections sharing one mount, and CLI branch switching with an existing mount. Rust tests cover lost response bodies, concurrent connections sharing a mount, handle checks, persistent selection, and HTTP authorization. Browser checks cover ancestry, revision details, token login, and narrow layouts. The installed Codex app-server initialize and thread/start handshake verified the AgentDrive working directory; complete turn scenarios use protocol fixtures.

`config_smoke.py` runs the daemon with a temporary directory and verifies file reload, invalid configuration retention, startup-change reporting, HTTP saves of the single cache-capacity field, revision conflicts, and restart recovery. It does not require a mount driver. Configuration tests cover existing-file loading and consistent snapshots; the engine cache test verifies that changing capacity starts collection toward 75% while retaining current branch contents.
