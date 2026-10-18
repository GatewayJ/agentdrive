# Architecture

[English](architecture.md) | [简体中文](architecture.zh-CN.md)

`agentdrived` is the composition root. It creates concrete adapters, injects them into the engine, and serves the resulting `Services` facade. The engine imports model, port, and configuration crates; persistence, native drivers, S3, and protocol dependencies stay in adapters.

| Crate | Responsibility |
| --- | --- |
| `agentdrive-model` | IDs, paths, objects, revisions, branches, mount bindings, operations, merge records, errors |
| `agentdrive-config` | Configuration schema, defaults, validation, atomic file replacement, administrator access, and shared runtime snapshots |
| `agentdrive-ports` | Storage, filesystem, mount, clock, directory, and application interfaces |
| `agentdrive-engine` | File semantics and workspace, revision, mount, replication, cache, ownership, merge workflows |
| `agentdrive-local` | A process lock, SQLite metadata, immutable objects, working files, leases, verified reads |
| `agentdrive-s3` | Streaming immutable object transport and conditional reference updates |
| `agentdrive-platform` | FUSE/WinFsp callbacks, contained host-directory access and clock |
| `agentdrive-mcp` | Typed tools, authenticated HTTP routing, dashboard and settings HTTP API, client, stdio forwarding |
| `agentdrive-test-support` | Deterministic clocks, mount and storage doubles, injected outages and lost acknowledgements |
| `agentdrived` / `agentdrive` | Daemon lifecycle and command-line client |

## Module responsibilities

| Module | Responsibility |
| --- | --- |
| `engine/runtime` | Workspace authorization, local identity, mutation coordination and shared runtime state |
| `engine/files` | File operations, open handles, permission and generation checks |
| `engine/content` | Verified content reads and coordinated downloads through `ContentResolver` |
| `engine/trees`, `engine/indexes` | Tree interpretation and copy-on-write page editing through `RevisionReader`, `ObjectGraph` and `TreeEditor` |
| `local/batch`, `local/queries` | Durable object publication batches and consistent SQLite read views |
| `engine/snapshots`, `engine/revisions` | Capture working changes, save revisions, create branches and restore content |
| `engine/workspaces` | Workspace creation, status and host-directory import/export |
| `engine/mounts` | Select the local branch for a workspace, reuse mount bindings, save mounted files and coordinate mount transitions |
| `engine/replica`, `engine/ownership` | Synchronization jobs, conditional remote publication and writer ownership transfer |
| `engine/storage` | Hydrate retained history, copy remote references and switch storage |
| `engine/cache` | Prefetch, pin and reclaim eligible local objects using the current cache policy |
| `engine/merge` | Prepare candidates, edit files, resolve conflicts and apply results |
| `engine/operations` | Durable request deduplication, progress and result receipts |
| `config/types` | Cache and S3 configuration types and policy validation |
| `config/file` | File schema, defaults, token-file loading and initial directory creation |
| `config/store` | Validate candidate settings, serialize writers, save configuration and publish `ArcSwap` snapshots |
| `mcp/gateway`, `mcp/server` | Authenticate HTTP requests and dispatch typed application tools |
| `mcp/web`, `mcp/dashboard` | Workspace/lifecycle/settings HTTP endpoints and browser views |
| `apps/agentdrive/integration` | ACP and Codex app-server event forwarding and persistence lifecycle |
| `apps/agentdrived/main` | Construct adapters, start HTTP and background workers, shut down cleanly |

`agentdrive-ports` describes the interfaces used by the engine. Storage and platform adapters implement those interfaces. The configuration crate depends on model types and does not construct database, mount, S3 or HTTP services. The daemon supplies one `LiveConfig` handle to the engine and HTTP gateway. Only `ConfigStore` publishes replacement snapshots; consumers obtain a consistent snapshot for each operation.

## File interfaces and transaction boundaries

| Interface or module | Responsibility | Caller boundary |
| --- | --- | --- |
| `FileSystem` / `engine/files` | Permissions, handles, paths, capacity checks and node mutations | Platform adapters translate requests and errors; they do not access SQLite or select branches |
| `LocalStateStore` | Consistent file/retention views, working-tree usage and atomic guarded commits | The engine submits `LocalCommit`, without SQL or schema dependencies |
| `ObjectWriter` / `ObjectBatch` | Stage complete immutable objects and finish durable publication | The caller retains object protection until the new root commits; staging alone does not make a reference durable |
| `TreeEditor` / `ObjectGraph` | Edit immutable indexes and traverse retained dependencies | Storage adapters own synchronization; the engine owns retention decisions |
| `WorkingTreeStore` | Mutable local file I/O | No quota, authorization, revision or remote durability decisions |
| `ContentResolver` | Immutable content retrieval, download coordination and verification | File operations do not call S3 directly |
| `SnapshotCapture` / revisions | Consistent capture, revisions and sync jobs | Local persistence and remote confirmation remain separate outcomes |
| replica | Publication order, retries and bounded uploads | All dependencies must succeed before BranchRef publication |
| remote storage ports | Verified transfers and conditional reference updates | Adapters do not mutate branches, mounts or working trees |

`file_view` reads the mount, workspace, branch and requested nodes in one SQLite read transaction. File operations read the binding before taking the branch mutex and obtain a fresh view after acquiring it; authorization and generation checks use that fresh view. There is no authorization cache. Prepared statements are reused. Rename within one parent updates that parent once.

`usage` reads branch inode and logical-byte counters maintained in the same SQLite transaction as node mutations. Rollback restores both. The engine applies workspace limits; these counters do not measure historical objects, cache, temporary files or host free space.

`children` uses the `(branch, parent, path)` index for direct children. `subtree` uses normalized path ranges for directory moves. Checkpoint capture reads `node_deltas`; full `nodes` queries remain for full-tree operations and recovery. Create/unlink update affected nodes and parents; rename changes only the moved subtree, replacement target and parents. Directory moves remain proportional to the moved subtree.

The local adapter owns the `parent` projection and `node_usage` table. Existing databases gain the projection, backfilled parent keys, counters and triggers in one transaction. Node display names retain casing; query keys use Unicode normalization and case folding. Node payloads and remote formats remain unchanged. Use a version supporting these projections after migration; restore a pre-upgrade state-directory backup before downgrading.

Branch mutexes still coordinate file operations and snapshots. Cache misses and first writes can download content while holding that mutex. SQLite still uses one mutex-protected connection. These synchronization semantics are unchanged by the query optimizations.

A `ReplicaService` shares four upload permits across publication operations. Objects use streaming interfaces. Failure stops scheduling further objects, drains scheduled transfers for cleanup, releases permits and leaves retryable jobs; incomplete dependencies cannot be published through a BranchRef. New dependencies and objects outside the current remote reference receive full integrity verification. Common immutable subtrees retained by the current remote branch reference are reused without another transfer or integrity scan.

## Configuration lifecycle

`ConfigStore::open` reads and validates the file. `StartupConfig` supplies storage paths, mounts, transport and credentials during construction. Cache policy, S3 connections and allowed HTTP origins are held together in one `ArcSwap<Snapshot>`. Cache collection retains one snapshot for the operation; HTTP authorization checks the snapshot for each request.

A daemon worker checks the configuration file once per second. A dashboard save uses the same store and writer lock, checks the expected file revision, writes and synchronizes a temporary file in the same directory, replaces the configuration file, and publishes the snapshot. On Unix the directory is also synchronized. File errors, invalid values and changes to startup settings preserve the active configuration and report an error. Configuration revision checks reject stale API writers and detected external file edits; external editors do not participate in the daemon's writer lock. Use atomic file replacement when editing externally.

Settings administration uses the startup `administrators` list, default `["owner"]`, independently of workspace grants. The settings API never reads or returns token contents. Changing HTTP credentials, administrator membership, local directories or listen address requires a restart. S3 settings use `ConfigApplier` to prepare content, persist configuration, and replace the remote adapters while holding lifecycle, publication and object-retention locks. See [configuration](configuration.md) for the fields and HTTP interface.

## Local persistence

Each daemon state directory contains `engine.lock`, `meta.db`, SQLite WAL files, `objects/`, `staging/`, and `working/`. A process lock permits one writer. SQLite uses WAL and `synchronous=FULL`. Persisted format versions are checked before schema changes. Storage errors propagate as typed errors.

`meta.db` and its WAL files remain local in both storage modes. S3 receives immutable objects and the control/branch references described below. Pulling imports published history into the receiving daemon's database; local identity, branch selections, mount bindings, cache pins and pending work remain local state. Cache capacity covers immutable object files only. In S3 mode, the collector uses `cache.max_bytes`, a derived 75% collection target, and an internal 64 MiB minimum-free-space threshold. Local mode retains all historical content.

Object IDs hash a domain separator, format version, object kind, and bytes with SHA-256. Publication verifies the digest and size, flushes the temporary file, publishes its immutable name, and registers it. Unix directories are flushed after publication; Windows uses a write-through file move. A metadata transaction references installed immutable objects. Reader leases protect open content from cache eviction.

A `TreeRoot` refers to bounded directory and inode indexes. Index pages contain at most 128 entries and metadata objects are bounded by 2 MiB. File content is streamed with bounded buffers. Directory entry names are NFC normalized and compared using Unicode case folding. Portable-name checks reject traversal, reserved Windows names, trailing dots/spaces, and nonportable characters.

Each branch has a serialization gate covering mutation and capture. `node_deltas` records the first previous value of each changed path in the same SQLite transaction as the node change. Capture seals changed files as complete immutable file objects and rewrites affected inode/directory index pages. The branch root, saved sequence, captured node values and journal removal commit atomically. Old roots remain readable.

A checkpoint stages its complete file objects, directory pages, inode pages and `TreeRoot` through one `ObjectBatch`. Each new object file is synchronized before publication. After publishing the batch, the local adapter synchronizes the workspace object directory once, and its parent once when first needed in the process. Windows retains write-through publication. One SQLite transaction registers the new objects, then the engine atomically advances the branch checkpoint. Failed publication leaves the checkpoint and pending deltas unchanged. WAL and `synchronous=FULL` remain required.

Index editing distributes overflowing pages evenly, and merges or redistributes changed pages below half capacity with adjacent siblings. Unchanged pages are reused and single-child roots collapse. Existing format-1 indexes remain readable; existing sparse pages are reorganized when affected by edits, without rewriting all history.

Filesystem `fsync` creates a durable working checkpoint without creating a revision. Local checkpoints do not create operation receipts or synchronization jobs; unchanged local fsync is a no-op. Remote fsync additionally publishes the working root and waits for remote confirmation. Its receipt records an operation, but intermediate checkpoints are not retained as historical versions. Autosave creates revisions after five seconds without writes or thirty seconds of continuous writes; the daemon checks once per second. Explicit `mount_save`, `revision_commit`, branch creation and branch handoff retain versions when needed. Checkpointing does not reset the autosave deadline. Restart restores the latest checkpoint and schedules any unversioned checkpoint for autosave.

`branch_history` caches the latest immutable history index for incremental updates. A missing projection on an existing database is rebuilt on the next publication job. `object_usage` stores confirmed-cache and unconfirmed-object byte counts; SQLite triggers update them with object insertion, removal and confirmation. `LocalObjectStore::volume_stats` supplies host capacity to filesystem STATFS without scanning object metadata. Cache collection can reclaim unconfirmed objects proven unreachable from branches, queued publications, retained revisions and pins, while holding the object retention barrier and respecting active readers. SQLite and complete file storage remain the persistence model. The latest history index pages of each retained local branch remain cached so incremental local saves work while remote storage is unavailable. This retention covers index pages only; historical file contents remain eligible for normal eviction.

Cache collection reads a consistent retention snapshot and traverses object graphs without the exclusive object barrier. Before each deletion batch of at most 32 objects, it tries to acquire that barrier and rechecks branch roots, queued publications, retained revisions, history indexes, candidates and pins. Busy writers or changed retention roots defer collection. Reader leases and confirmation state are checked again during deletion. Each affected directory is synchronized once per deletion batch. Concurrent collectors are excluded.

A lifecycle gate serializes mount changes, restore, merge apply, ownership transfer, and cancellation. File handles carry mount and branch generations. Generation changes reject stale handles. A mount path has one active binding, and a branch has at most one writable mount.

Each state directory persists one `location_id`. Therefore `workspace_selections(workspace, branch)` records the branch selected by that location for each workspace. The workspace creator selects its root branch. A different location’s first writable mount creates and selects an owned branch in one transaction, using the latest published working root of the root branch identified by `Workspace.initial_revision`. Repeated connections, unmounting, path changes and restarts reuse this selection. `branch_switch` changes it explicitly. Matching requests from the same principal share the existing mount binding; ending an Agent connection leaves that binding available.

All locations sharing a workspace should run a version that understands working checkpoints before enabling remote fsync. The object encoding remains format 1, but a published working root can now differ from both revision heads; older clients can reject that state or fork from an older revision. Back up the state directory before upgrading and use a matching pre-upgrade backup for a local downgrade.

## Replication and ownership

A workspace has immutable objects plus a conditional workspace control record and conditional branch references. A branch reference contains its owner, epoch, generation, source generation, heads, and immutable history index. The history index records revisions and operation receipts.

Publication groups consecutive jobs for the same branch, owner, authority epoch and generation, with increasing source generations and nondecreasing durable sequences. A stopped/deleted reference or a generation/authority change ends the group. The newest history index must retain every covered operation with its original identity and result. The publisher compares the new graph with roots retained by the current remote reference, uploads new dependencies once, and conditionally publishes the newest reference. All covered receipts and queue removals then commit together in SQLite. Publication and mount completion update receipts with transactional expected-record checks. A concurrent change causes the update to reload and retry, preserving both remote confirmation and mount state. Confirmed receipts expose their persisted completed progress during retention cleanup. A workspace publication gate preserves ordering; independent workspace requests can proceed concurrently under the shared upload limit. Storage switching takes the exclusive publication barrier.

Publication completes its dependency uploads before compare-and-swap of the branch reference. Opaque provider versions are used for conditional writes. When an acknowledgement is lost, the publisher inspects the original operation's history record. Durable local jobs survive retries and process restarts. Workspace control records protect in-progress publication/read roots and coordinate the GC phase protocol. Physical remote deletion is disabled. `OperationRecord.sync_progress` exposes queue position, publication stage and object/byte counts. `mount_save` bounds remote waiting and leaves publication running after returning its durable receipt; filesystem remote fsync continues to require confirmation before success.

Pull imports remote history and references without changing an active local branch. Explicit adoption follows a confirmed ownership transfer. Transfer stops the old writer, drains its publication queue, then conditionally advances owner and epoch. The new location adopts the confirmed state. An unreachable owner cannot be forcibly replaced by this API.

## Merge

Merge preparation selects a common ancestor or validates an explicit one, compares path changes, and produces an immutable candidate. AI can list candidate files, select source or target contents, delete paths, submit replacement bytes, or copy files from an existing revision. Each edit creates a new candidate. Apply requires clean target state, current ownership/generation/head, resolved conflicts, and mount quiescence. The resulting revision retains source and target parents.


## Authentication boundary

Bearer credentials select a principal before creating the MCP service. Each credential has a separate HTTP session manager. Tool arguments cannot override the principal. Host and Origin allowlists and an 8 MiB request limit apply at the HTTP boundary. The daemon binds to loopback. Remote access requires an administrator-managed authenticated TLS proxy preserving the accepted Host value.

Workspace grants authorize application actions. Native filesystem access is tied to the daemon's OS identity. Keep configuration, credentials, state, mount roots, and exchange roots restricted to that identity; on Windows configure their NTFS permissions accordingly. S3 credentials and administrators are trusted storage writers.

See [configuration and S3](configuration.md), [CLI usage](cli.md), and [MCP parameters and capabilities](mcp.md).
