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
| `engine/content`, `engine/trees` | Verified content reads, immutable indexes and object closure traversal |
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

## Configuration lifecycle

`ConfigStore::open` reads and validates the file. `StartupConfig` supplies storage paths, mounts, transport and credentials during construction. Cache policy, S3 connections and allowed HTTP origins are held together in one `ArcSwap<Snapshot>`. Cache collection retains one snapshot for the operation; HTTP authorization checks the snapshot for each request.

A daemon worker checks the configuration file once per second. A dashboard save uses the same store and writer lock, checks the expected file revision, writes and synchronizes a temporary file in the same directory, replaces the configuration file, and publishes the snapshot. On Unix the directory is also synchronized. File errors, invalid values and changes to startup settings preserve the active configuration and report an error. Configuration revision checks reject stale API writers and detected external file edits; external editors do not participate in the daemon's writer lock. Use atomic file replacement when editing externally.

Settings administration uses the startup `administrators` list, default `["owner"]`, independently of workspace grants. The settings API never reads or returns token contents. Changing HTTP credentials, administrator membership, local directories or listen address requires a restart. S3 settings use `ConfigApplier` to prepare content, persist configuration, and replace the remote adapters while holding lifecycle, publication and object-retention locks. See [configuration](configuration.md) for the fields and HTTP interface.

## Local persistence

Each daemon state directory contains `engine.lock`, `meta.db`, SQLite WAL files, `objects/`, `staging/`, and `working/`. A process lock permits one writer. SQLite uses WAL and `synchronous=FULL`. Persisted format versions are checked before schema changes. Storage errors propagate as typed errors.

`meta.db` and its WAL files remain local in both storage modes. S3 receives immutable objects and the control/branch references described below. Pulling imports published history into the receiving daemon's database; local identity, branch selections, mount bindings, cache pins and pending work remain local state. Cache capacity covers immutable object files only. In S3 mode, the collector uses `cache.max_bytes`, a derived 75% collection target, and an internal 64 MiB minimum-free-space threshold. Local mode retains all historical content.

Object IDs hash a domain separator, format version, object kind, and bytes with SHA-256. Publication verifies the digest and size, flushes the temporary file, publishes its immutable name, and registers it. Unix directories are flushed after publication; Windows uses a write-through file move. A metadata transaction references installed immutable objects. Reader leases protect open content from cache eviction.

A `TreeRoot` refers to bounded directory and inode indexes. Index pages contain at most 128 entries and metadata objects are bounded by 2 MiB. File content is streamed with bounded buffers. Directory entry names are NFC normalized and compared using Unicode case folding. Portable-name checks reject traversal, reserved Windows names, trailing dots/spaces, and nonportable characters.

Each branch has a serialization gate covering file mutation and snapshot capture. A capture seals changed files, builds immutable indexes, then commits the root, revision, operation receipt, and synchronization job in one local transaction. A filesystem `fsync` confirms a local snapshot. Autosave uses a five-second quiet period and a thirty-second continuous-write deadline; the daemon checks once per second.

A lifecycle gate serializes mount changes, restore, merge apply, ownership transfer, and cancellation. File handles carry mount and branch generations. Generation changes reject stale handles. A mount path has one active binding, and a branch has at most one writable mount.

Each state directory persists one `location_id`. Therefore `workspace_selections(workspace, branch)` records the branch selected by that location for each workspace. The workspace creator selects its root branch. A different location’s first writable mount creates and selects an owned branch in one transaction, using the latest published saved revision of the root branch identified by `Workspace.initial_revision`. Repeated connections, unmounting, path changes and restarts reuse this selection. `branch_switch` changes it explicitly. Matching requests share the existing mount binding; ending an Agent connection leaves that binding available.

## Replication and ownership

A workspace has immutable objects plus a conditional workspace control record and conditional branch references. A branch reference contains its owner, epoch, generation, source generation, heads, and immutable history index. The history index records revisions and operation receipts.

Publication uploads the referenced closure before compare-and-swap of the branch reference. Opaque provider versions are used for conditional writes. When an acknowledgement is lost, the publisher inspects the original operation's history record. Durable local jobs survive retries and process restarts. Workspace control records protect in-progress publication/read roots and coordinate the GC phase protocol. Physical remote deletion is disabled.

Pull imports remote history and references without changing an active local branch. Explicit adoption follows a confirmed ownership transfer. Transfer stops the old writer, drains its publication queue, then conditionally advances owner and epoch. The new location adopts the confirmed state. An unreachable owner cannot be forcibly replaced by this API.

## Merge

Merge preparation selects a common ancestor or validates an explicit one, compares path changes, and produces an immutable candidate. AI can list candidate files, select source or target contents, delete paths, submit replacement bytes, or copy files from an existing revision. Each edit creates a new candidate. Apply requires clean target state, current ownership/generation/head, resolved conflicts, and mount quiescence. The resulting revision retains source and target parents.


## Authentication boundary

Bearer credentials select a principal before creating the MCP service. Each credential has a separate HTTP session manager. Tool arguments cannot override the principal. Host and Origin allowlists and an 8 MiB request limit apply at the HTTP boundary. The daemon binds to loopback. Remote access requires an administrator-managed authenticated TLS proxy preserving the accepted Host value.

Workspace grants authorize application actions. Native filesystem access is tied to the daemon's OS identity. Keep configuration, credentials, state, mount roots, and exchange roots restricted to that identity; on Windows configure their NTFS permissions accordingly. S3 credentials and administrators are trusted storage writers.

See [configuration and S3](configuration.md), [CLI usage](cli.md), and [MCP parameters and capabilities](mcp.md).
