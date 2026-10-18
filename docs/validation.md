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

`verify` runs rustfmt, Clippy with warnings denied, workspace tests, and `tests/architecture.py`. The architecture check validates production crate dependencies, keeps database/native-driver/protocol adapters out of the core, requires inherited workspace lint settings, and checks Python test syntax. Default Make builds use the committed lockfile. CI runs these checks on Linux, macOS and Windows; Linux and Windows additionally run the native branch-switch smoke suite. On Windows use `PYTHON=python` if `python3` is unavailable.

CLI and MCP smoke tests decode process output and daemon files explicitly as UTF-8, including on Windows hosts with a different system code page. The upload concurrency test uses a four-request barrier and verifies that failed transfers release every permit.

## Revision changes and indexed reads (2026-10-18)

On macOS with FUSE-T 1.2.9, `make -j8 verify build FEATURES=macos-fuse-t` passed: formatting, Clippy, architecture checks and 78 Rust tests. Three native mount tests, the real RustFS test and all six CLI/config/replica/storage/lifecycle/branch-switch smoke suites also passed. The daemon advertises 31 MCP tools.

New regressions cover paginated comparisons, original revision preservation, inode-index rebalancing and height changes, decoding each cached page once, whole-directory and nested moves, ambiguous renames, uid/gid changes, timestamp-only changes, case-insensitive lookup, Unicode text, binary/size/line limits, missing final newlines, authorization and cross-workspace rejection. HTTP and MCP return matching results. Engine services retain access checks and object protection; `TreeQuery` owns immutable lookup, comparison and derived caching.

Chromium checks against a real daemon covered a 114-entry initial revision, pagination, both parents of a real merge, added/deleted/renamed files, Chinese text and line numbers, binary/large-file statuses, failed-request retry, refresh preserving the selected file, stale-request cancellation, logout and 390-pixel layout. Text containing HTML tags remained literal text. No browser script errors occurred. Desktop and mobile screenshots were inspected.

The following medians are from three runs of the 10,000-file query test in a debug build with local objects. One file changed, with unchanged directory paths. The baseline invokes the retained full-tree reader used by the earlier query path; it does not run an older binary. File-content transfer and network latency are excluded.

| Query | Full-tree baseline | Indexed query | Metadata reads in indexed query |
| --- | ---: | ---: | ---: |
| Locate one file | 404.29 ms | 11.36 ms | 6 |
| Compare revisions, new path projection | 859.26 ms | 206.78 ms | 87 |
| Compare revisions, cached path projection | 859.26 ms | 9.41 ms | 7 |

A separate whole-directory rename with 1,000 files took a median 82.89 ms for indexed comparison versus 98.22 ms for full-tree reads.

Reproduce with `make -j8 test CARGO_FLAGS='--locked -p agentdrive-engine --lib query_tests::indexed_queries_skip_shared_pages_and_preserve_old_roots -- --nocapture --test-threads=1'`. The test asserts read-count bounds and result correctness; timing has no pass/fail threshold. Cold directory projections and response pagination costs are described in [architecture](architecture.md).

The isolated FUSE-T/RustFS run preserved the adoption path, switched branches through MCP, recovered after daemon restart and verified downloads by SHA-256: 1,093 main files and 1,138 feature files, including AppleDouble files. Creating 543 files with explicit fsync took 37.35 seconds; 50 renames took 340 ms. The subsequent remote save confirmed in 211 ms; 21 queued saves recovered after a remote outage in 1.37 seconds. This change targets historical queries; the mounted-write measurements provide regression evidence.

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

[GitHub Actions](https://github.com/GatewayJ/agentdrive/actions) runs the checked-in test matrix for each commit. Native macOS mounting was verified with FUSE-T 1.2.9. The three native tests passed, including directory adoption, exclusive creation, rename with an open handle, more than 128 entries, read-only history, and normal unmount. macOS AppleDouble metadata files are excluded from directory-entry count assertions.

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

Local query tests cover current schema initialization, incremental usage, rollback and reopen. Workflow tests cover directory moves, Unicode/case preservation, unchanged siblings, quota rejection, truncation and deletion. Upload tests cover the shared four-transfer limit, failure cleanup and permit recovery after failure.

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

## Checkpoints and publication optimization (2026-10-18)

macOS 26.5, FUSE-T 1.2.9, debug builds, and a dedicated RustFS container. `make -j8 verify build FEATURES=macos-fuse-t` passed with 59 workspace tests. All three native mount tests, the real S3 backend test, and all six Python smoke suites passed. The engine benchmark now includes ten writes followed by fsync and 1000 capacity queries:

| Background files | Write ×100 ms | Readdir ×50 ms | Rename ×50 ms | Write + fsync ×10 ms | STATFS ×1000 ms |
| --- | --- | --- | --- | --- | --- |
| 100 | 45 | 11 | 34 | 391 | 115 |
| 1000 | 36 | 10 | 33 | 505 | 117 |
| 10000 | 35 | 11 | 33 | 577 | 114 |

The native run imported an existing directory into a new workspace at the same path, created 543 background files with explicit fsync, saved remotely, switched branches through MCP, wrote independent contents, and restarted the daemon. RustFS was stopped while twenty local saves and one remote save accumulated. After restart, all 21 operations were confirmed in a batch of 21 in 1.10 seconds; every original operation result was preserved. A fresh daemon downloaded main (1093 files) and feature (1138 files), with all paths, sizes and SHA-256 values matching. Counts include AppleDouble files. The queue ended empty.

| Measurement | Result |
| --- | --- |
| Create 543 files + explicit fsync | 82.19 s; revisions 1 → 3 |
| Native write ×100 / explicit fsync | 0.40 / 57.29 ms |
| Native target-directory readdir ×50 | 12.39 ms; warm repeat 0.77 ms |
| Native rename ×50 | 637.11 ms; prior run 1149.95 ms |
| Final bulk-save return / remote confirmation | 25.74 / 140.53 ms |
| SQLite SUM / maintained counters, 1000 reads over 6949 objects | 344.03 / 1.21 ms; equal byte counts |

The earlier 1507-second save waited behind hundreds of fsync-created revision jobs. The new run allowed autosave/background publication while creating files, and the final save transferred 12 changed objects. These are workload observations, not a fixed speedup ratio. OS caches affect native write/readdir/STATFS timings; SQL measurements isolate database query costs. Whole-file sealing and durable object flushes remain measurable costs, and 543 native creates still take about 82 seconds.

New regression tests cover checkpoint/revision separation, checkpoint recovery and forks, remote checkpoint mounting, incremental index growth/deletion and old-root preservation, unconfirmed-object reclamation, persistent object counters and bounded save polling. Merged publication tests inject remote outages and a lost branch CAS response. A FUSE-T mirror probe checked cache/xattr feasibility; see [integration observations](integrations.md#macos-fuse-t). Cache options and xattr behavior remain unchanged. Linux and Windows execution was not repeated in this local run.

## Follow-up review validation (2026-10-18)

Regression tests reproduce and cover two failure cases: a local save after cache collection and a remote outage, and publication completing while a restored revision is being remounted. The cache test grows the history beyond one index page and verifies that historical file contents can still be evicted. The remount test pauses the mount driver to enforce the interleaving and checks the persisted receipt after both operations finish. A SQLite test verifies that stale receipt guards reject the entire update and that a retry using the current receipt succeeds.

The follow-up `make -j8 verify build FEATURES=macos-fuse-t` passed with 62 workspace tests. All three real FUSE-T mount tests, the S3 backend test, and six Python smoke suites passed on macOS. SQLite retains transaction checks; retry policy and mount/publication state transitions remain in the engine.

The complete RustFS rerun also passed: the original directory path and branch isolation were preserved, 21 queued saves were confirmed in 1.37 seconds after the outage, and fresh downloads of main (1093 files) and feature (1138 files) matched every path, size and SHA-256 value. Restart verification passed and the final synchronization queue was empty.

## Batched checkpoints and index balance (2026-10-18)

The local FUSE-T run passed 69 workspace tests, rustfmt, Clippy, the architecture check, all three native mount tests, the real S3 backend test, and six Python smoke suites. Added regressions cover staged-object cleanup, batch publication/reopen, publication failure without checkpoint advancement, reader/confirmation checks during reclamation, retention changes during graph traversal, current authorization/generation checks, and index balance with preserved historical roots.

The retained `ab0d422` debug binary and the new debug binary ran the same native workload against isolated RustFS prefixes. The new binary ran twice. Filesystem settings and file-level synchronization remain unchanged.

| Native workload | Before | After, two runs |
| --- | --- | --- |
| Create 543 files, explicit fsync per file | 78.836 s | 35.175 / 35.228 s |
| Rename a regular file 50 times | 669.663 ms | 347.672 / 390.050 ms |
| Rename its AppleDouble file 50 times | 293.282 ms | 147.446 / 155.802 ms |
| Create 100 files without application fsync | 18.191 s | 6.136 / 6.174 s |
| Create 100 AppleDouble control files | 647.561 ms | 366.033 / 356.635 ms |
| Final local save | 172.914 ms | 156.118 / 139.408 ms |

The final tree has 1399 directory entries and 1400 inodes. Directory leaf pages decreased from 111 to 19; single-entry directory pages decreased from 97 to zero. Inode leaf pages increased from 11 to 21 because balanced page splits retain room for subsequent writes. Total active directory/inode index pages decreased from 124 to 42. This counts the current root, not retained historical pages.

| Background files | Write ×100 before/after ms | Readdir ×50 before/after ms | Rename ×50 before/after ms | Write + fsync ×10 before/after ms | STATFS ×1000 before/after ms |
| --- | --- | --- | --- | --- | --- |
| 100 | 44 / 40 | 11 / 5 | 34 / 29 | 394 / 287 | 104 / 58 |
| 1000 | 39 / 36 | 11 / 5 | 35 / 28 | 486 / 324 | 115 / 53 |
| 10000 | 35 / 31 | 11 / 5 | 32 / 28 | 530 / 370 | 117 / 50 |

The complete native workflow preserved the original directory path and MCP branch isolation. Independent RustFS downloads matched every path, size and SHA-256 for main (1093 files) and feature (1138 files). Restart and outage recovery passed. A separate outage queued 554 explicit saves; after the daemon restarted against the restored endpoint, all were remotely confirmed in 11.223 seconds, transferring 3612 objects / 34,354,999 bytes. All original results were retained and a fresh location restored 1093 matching files. The previous 554-explicit-save run included two additional autosaves and confirmed in 13.592 seconds. Both measurements include daemon startup and 0.5-second polling. Final queues were empty.

These measurements preserve SQLite WAL/FULL and complete-file objects. Native creates still include FUSE-T/macOS metadata traffic and per-object file synchronization. No internal exclusive-time profile was repeated in this run. The local measurements cover macOS; Linux and Windows are checked by CI after publication.

## Batch optimization review (2026-10-18)

Regression tests reproduce two defects in the batch changes: a leased oldest object could stop collection before later objects were considered, and 16,385 index entries could leave a one-entry page. Collection now continues using actual reclaimed bytes while preserving the 32-object batch limit and retention checks. Index creation and editing share the same balanced page distribution; tests verify every entry and non-root page occupancy across multiple levels.

Before the first release, the model contains current revision and operation records. Database creation installs the complete current schema atomically; reopening preserves its counters. Historical session/turn variants and schema migration paths have been removed.

Validation passed: 73 workspace tests, formatting, Clippy with warnings denied, architecture checks, three native FUSE-T tests, the real RustFS backend test, and all six Python smoke suites. The native end-to-end run preserved the adopted path and branch isolation, confirmed all 21 saves queued during an outage, and restored 1138 files after restart. Independent RustFS downloads matched every path, size and SHA-256 for main (1093 files) and feature (1138 files); the final synchronization queue was empty. Creating 543 files took 34.739 seconds, 50 renames took 339.562 milliseconds, and the final remote save was confirmed in 177.651 milliseconds after background publication during file creation. This run did not repeat the separate 554-save backlog benchmark.

## Revision dialogs (2026-10-18)

The dashboard history menu opens version information and file changes in separate dialogs. Browser checks against saved daemon data passed for keyboard navigation, focus return, outside-click and Escape dismissal, lazy difference requests, merge-parent selection, 114-entry pagination, UTF-8 text and safe HTML display, empty/binary/oversized files, rename messages, retry and stale-response cancellation, automatic refresh, and a 390-pixel viewport. Workspace verification passed 78 Rust tests, formatting, Clippy and architecture checks on macOS with FUSE-T enabled.

## Branch origins and mount inventory (2026-10-18)

The dashboard workspace response now includes typed counts and branch origins derived through the existing application interfaces. Regression tests cover distinct ready branches across read/write and read-only mounts, preparing/paused/recovery states, released bindings, deleted branches, and preserved creation revisions after the source head advances. The authenticated HTTP test verifies that a second mount of one branch increases only the mount count, and that unmounting removes its inventory entry.

Browser checks against an isolated daemon with two native FUSE-T mounts passed for exact source-version connections, version information dialogs, focus return after automatic refresh, mount-to-branch navigation, unchanged default branch selection, revision differences, settings navigation, logout and a 390-pixel viewport. Additional browser fixtures cover deleted source branches, unavailable revisions, read-only/recovery display, empty mount inventory and safe text rendering. Workspace verification passed 80 Rust tests, formatting, Clippy and architecture checks on macOS with FUSE-T enabled.

## Combined revision details (2026-10-18)

Double-clicking a history entry opens version metadata and file changes in one dialog. Enter, Space and source revisions in the branch graph also open it. Browser checks passed for single-click behavior without difference requests, automatic first-file loading, refresh preservation, focus return, empty-file messages, failed-request retry and a 390-pixel viewport. JavaScript syntax checks and the FUSE-T build passed for this change.

## Dashboard refresh regression (2026-10-18)

The browser regression suite reproduces a stale current-commit marker when a new commit arrives while the previous revision's dialog is open. The fix refreshes branch metadata without reloading file differences or changing the selected file. The suite also checks branch-owned history, empty child history, local durability and commit states, parent comparisons, source revisions, double-click and keyboard activation, focus return and narrow layouts.

The `dashboard` CI job runs these checks against the shipped frontend modules with controlled API responses. Run locally with Node.js 24:

```sh
npm --prefix tests/dashboard ci
(cd tests/dashboard && npx playwright install chromium && npm test)
```

These browser checks cover presentation and interaction. The Rust workspace tests cover checkpoint persistence, revision creation and the HTTP API; native mounts and RustFS remain separate integration checks.

## Current branch file preview (2026-10-18)

`make -j8 verify FEATURES=macos-fuse-t` passed 82 Rust tests (5 environment-dependent tests ignored), formatting, Clippy and architecture checks. Read regressions cover unsaved bytes, immutable range reads, EOF and size limits, unmounted branches, branch-scoped access, workspace isolation, direct-child pagination and stale versions. Authenticated HTTP checks cover access denial, path validation and cache headers. The 8 Playwright tests include existing history/save-state/revision-dialog behavior plus branch double-click, automatic preview, directories, hexadecimal bytes, HTML-safe text, UTF-8 chunk boundaries, pagination, stale content, cancellation and narrow layouts.

The local FUSE-T service was restarted from the consolidated `agentdrive` checkout with its existing RustFS configuration and three mount paths. All 36 files across four branches matched SHA-256 baselines through the new preview API, including the unmounted main branch. MCP switching from the feature branch to main and back preserved paths and file contents; RustFS synchronization completed with zero pending jobs. Live Chromium checks passed for branch previews, mounted file content, revision details and a 390-pixel viewport.
