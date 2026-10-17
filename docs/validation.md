# Validation

[English](validation.md) | [简体中文](validation.zh-CN.md)

Run all standard checks through multicore Make:

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" verify
make -j "$(getconf _NPROCESSORS_ONLN)" build
python3 tests/cli_smoke.py
```

The workflow tests exercise concurrent request retries, atomic append, rename with open handles, restart recovery, idempotent mount saves, concurrent mount reuse, branch isolation across locations, autosave timing, remote failure and lost acknowledgements, ownership transfer, merge file edits, stale mount changes, cache pinning/eviction, and offline prefetch. Persistence tests cover digest failures, reader leases, transaction rollback, request recovery, and unknown format rejection. MCP tests verify concurrent clients, principal isolation, authentication, and Host/Origin policy. The CLI test starts real processes and verifies stdio MCP, import/export content and metadata, allowed paths, and process restart.

Directory conversion tests cover initial `main` contents in a new workspace, import into an existing workspace, required Agent restart acknowledgement, preserved backups, invalid input, and retry after mount failure and daemon restart.

## Native filesystems

Install and enable the platform driver, then run:

```sh
make -j "$(getconf _NPROCESSORS_ONLN)" test CARGO_FLAGS='-p agentdrive-platform --test native_mount -- --ignored --nocapture'
```

On macOS add `--features macos-fuse-t` inside `CARGO_FLAGS`. On Windows use `make -j $env:NUMBER_OF_PROCESSORS` in PowerShell. The integration test owns its temporary state and mount directories. It tests a file over 3 MiB, truncation, random write, atomic append, rename, fsync, over 128 directory entries, historical read-only access, and unmount.

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
| Native mounted file operations and revision access | Linux as root and as an ordinary user; Windows with WinFsp 2.1; macOS 26.5 with FUSE-T 1.2.9 |
| CLI, stdio MCP, directory metadata, restart recovery | macOS, Linux, Windows |
| Conditional S3 writes and multipart content | Isolated RustFS instance |
| Two-daemon S3 replication and ownership transfer | macOS and Linux |
| macFUSE callback compilation | macOS |

[GitHub Actions](https://github.com/GatewayJ/agentdrive/actions) runs the checked-in test matrix for each commit. Native macOS mounting was verified with FUSE-T 1.2.9. The two native tests passed, including directory adoption, exclusive creation, rename with an open handle, more than 128 entries, read-only history, and normal unmount. macOS AppleDouble metadata files are excluded from directory-entry count assertions.

The create/import/read/export/mount examples in [CLI usage](cli.md) were executed against a temporary daemon. The [MCP parameter tables](mcp.md) were checked against the running daemon's tool schemas. S3 configuration was checked against the daemon config loader, adapter, and credential/endpoint selection in the locked `object_store` dependency. These documentation checks do not add an AWS S3 deployment to the verified environments above.

Additional lifecycle, branch, and dashboard checks:

```sh
python3 tests/lifecycle_smoke.py
python3 tests/config_smoke.py
```

`lifecycle_smoke.py` requires native mounting and covers ACP and Codex app-server completion, cancellation, failures, process interruption, duplicate requests, autosave, daemon restart, reconnect, two Agent connections sharing one mount, and CLI branch switching with an existing mount. Rust tests cover lost response bodies, concurrent connections sharing a mount, handle checks, persistent selection, and HTTP authorization. Browser checks cover ancestry, revision details, token login, and narrow layouts. The installed Codex app-server initialize and thread/start handshake verified the AgentDrive working directory; complete turn scenarios use protocol fixtures.

`config_smoke.py` runs the daemon with a temporary directory and verifies file reload, invalid configuration retention, startup-change reporting, HTTP saves of the single cache-capacity field, revision conflicts, and restart recovery. It does not require a mount driver. Configuration tests cover existing-file loading and consistent snapshots; the engine cache test verifies that changing capacity starts collection toward 75% while retaining current branch contents.

## FUSE-T and RustFS local run (2026-10-17)

On macOS 26.5 with FUSE-T 1.2.9 and an isolated RustFS container, Codex created the initial directory and continued writing after conversion at the same path. A 12 MiB file, an empty file, UTF-8 names, creation, updates, rename, and deletion were checked. Codex used stdio MCP to create and switch branches and save with remote durability. Main and feature revisions were restored into an independent daemon and compared by full file manifests and SHA-256. Switching back and forth preserved both branches. Normal daemon restart followed by `mount_prepare` preserved the selected branch and files. Import into an existing workspace created a new branch at the original path.

During a RustFS outage, local saving succeeded and remote saving reported failure. After recovery, sync confirmed the original save, and an independent remote restore matched every file. The feature build passed 44 workspace tests and Clippy, two native mount tests, 56 fuser tests, and the CLI/stdio smoke test. Native tests took 134.14 seconds. This run did not establish multi-process file-lock support or production throughput. See [FUSE-T usage](integrations.md#macos-fuse-t) for lifecycle and metadata behavior.

## Single-call branch switching

With the native driver installed and a native-mount build available, run:

```sh
python3 tests/branch_switch_smoke.py
```

The stdio MCP test verifies one `branch_switch` call per attempt, a process holding the mount as its working directory, an explicit `MOUNT_BUSY` recovery message, retry after restarting the MCP client, replay of completed requests, and branch isolation. It never calls `mount_release`. Engine tests inject unmount permission failures and target-mount failures and verify that the same request resumes without changing the original error cause. Native tests cover both open files and occupied working directories. These checks passed locally on macOS 26.5 with FUSE-T 1.2.9.

## Metadata and publication performance

Migration tests cover legacy Node data, incremental usage, rollback and reopen. Workflow tests cover directory moves, Unicode/case preservation, unchanged siblings, quota rejection, truncation and deletion. Upload tests cover the shared four-transfer limit, failure cleanup and permit recovery after failure.

Run the repeatable engine benchmark:

```sh
make -j8 test CARGO_FLAGS='-p agentdrive-engine --test workflows metadata_scaling_benchmark -- --ignored --nocapture'
```

On 2026-10-17, identical debug-build tests compared `099fe55` with this change on macOS. Each scenario adds unrelated files in another directory, then times 100 four-byte writes, 50 directory reads and 50 regular-file renames. Setup is excluded.

| Unrelated files | Writes before/after ms | Readdir before/after ms | Rename before/after ms |
| --- | --- | --- | --- |
| 100 | 103 / 50 | 38 / 11 | 395 / 30 |
| 1,000 | 563 / 38 | 286 / 10 | 3,714 / 30 |
| 10,000 | 5,487 / 33 | 2,797 / 11 | 37,951 / 29 |

This single local engine/SQLite comparison uses isolated temporary directories and excludes FUSE, MCP and S3. It demonstrates removal of work proportional to unrelated nodes for these operations. It is not an end-to-end speedup claim or a CI timing threshold.
