# Operations and recovery

[English](operations.md) | [简体中文](operations.zh-CN.md)

## Request identity and receipts

For command tools, reuse a `request_id` for the same tool and identical arguments. Identity is scoped to the authenticated principal and persists across restarts. A conflicting reuse returns `REQUEST_ID_CONFLICT`. Commands reserve their operation before executing the recorded workflow. `operation_by_request` and `operation_get` recover the original receipt. `operation_cancel` takes an existing operation ID directly; see the [MCP parameter reference](mcp.md).

Read these receipt fields independently:

| Field | Meaning |
| --- | --- |
| `local_saved` | The operation's result and referenced local state have been committed |
| `remote_confirmed` | Remote publication of the operation has been confirmed |
| `mount_ready` | The requested native attachment is ready, when relevant |
| `phase` | Current workflow state |
| `error` | A typed failure, possibly alongside a completed local save |
| `sync_progress` | Optional queue, stage and transfer counters for publication |

A remote error can accompany `local_saved: true`. Keep the existing operation ID, restore remote access, and run `sync` or allow the daemon to retry its persistent publication queue. A remote timeout does not establish whether a conditional write committed; the publisher checks the recorded operation identity.

## Mount changes

Restore and merge apply may require an attached filesystem to become quiet. An operation in `waiting_for_quiesce` carries its original mount generation. Stop the agent and other processes using that mount, close open files, and leave the directory in any shell. Call `mount_release` with that operation ID and `expected_binding_generation`. Repeating release uses its own stable request ID.

`branch_switch` attempts the complete change directly. A busy mount returns `MOUNT_BUSY`, `retryable: true`, and instructions to close files/processes or restart the Agent. The previous selection and mount remain available when unmount fails. Retry `branch_switch` with the same request ID after resolving the cause; no separate `mount_release` call is needed. Other failures retain their error code and message. If the old mount was removed but mounting the target failed, the path may be temporarily unmounted; retry the same request to resume installation. Completed requests are replayed without switching again.

The operation verifies the original branch guard again before changing the attachment. A stale target fails without applying the candidate. Generation checks prevent a handle from continuing against a replacement branch. A successful local commit followed by mount failure returns `committed_mount_pending`; the durable result is retained for recovery. Cancellation is accepted only while the original attachment remains unchanged and no local commit has completed.

A normal daemon shutdown stops HTTP traffic and background tasks, saves modified local branches, and requests ordinary unmounts. Busy mounts are reported for recovery. Do not remove the state directory while a daemon or filesystem is active.

## Process restart

Startup restores working metadata from the last acknowledged immutable branch root and removes disposable working copies. Writes that had not reached fsync, autosave, a revision operation, or mount_save can be lost after abrupt process termination. A failed write followed by failed metadata persistence stops the affected branch for recovery.

The local branch selection survives shutdown and restart. Run `agentdrive mount --workspace <workspace-id> <path>` or call `mount_prepare` again to reuse it. Native mounts are inspected before reuse; a matching binding retains its ID and generation. An unknown existing filesystem at a path requires operator cleanup; the service does not force-unmount an unrelated filesystem.

Keep the state directory on a local filesystem with correct file flush and atomic rename behavior. Filesystem and storage hardware must honor flush requests. Unix object publication flushes file data and directories; Windows flushes file data and uses write-through namespace publication. Remote confirmation is the additional durability boundary when remote storage is configured.

## Replication, cache, and ownership

`sync` supports `push`, `pull`, and `both`. Pulling a workspace onto a fresh location imports its history. The first writable `mount_prepare` also imports a missing workspace and creates this location’s branch from the root branch’s latest published working root. Later mounts reuse the local selection. A remote history reference does not grant local write ownership.

Use `cache_prefetch` to download a revision or selected paths. Use `cache_pin` with `enabled: true` to retain a complete selection for offline use; `cache_status` reports both completeness and pin protection. Unpinning permits later collection after other protection is absent. Cache capacity eviction removes remotely confirmed objects without active readers or protected references. Unconfirmed objects superseded by checkpoints can also be reclaimed after checking all retained roots and active readers.

While S3 is enabled, set the local cache capacity in Dashboard Settings or `cache.max_bytes`. Collection starts above that capacity or below the internal 64 MiB free-space threshold and targets 75% of the configured capacity. Protected content can keep usage above the setting. Local mode retains all history; switching from S3 hydrates missing historical content before disabling the remote connection. See [configuration](configuration.md) for storage switching and the local SQLite database's role.

Ownership transfer is online. Stop writing files and release writable mounts on the old location. Submit `ownership_transfer` with the current `BranchGuard` and the destination `LocationId`. The old location stops branch writes before draining publication and updating the conditional reference. Pull at the new location to adopt confirmed ownership. Retain the original transfer operation during an uncertain remote response.

## Merging files

Use `workspace_status` to obtain a branch's `formal_head`, then `revision_files` to list that version. Pages contain up to 200 paths; pass a returned `next` value as `after` to continue. Optional `path` limits the subtree. Use `revision_read` for bytes and `revision_diff` to compare versions. Commit working-directory edits with `revision_commit` before selecting their returned revision.

`merge_prepare` takes a source revision and target `BranchGuard` and creates a candidate. AI can inspect the candidate using `revision_files` and `revision_read`, then edit any path through `merge_resolve`. Each edit returns a new candidate revision for subsequent requests:

- `source` / `target`: select the path's contents from that version, including directory contents.
- `delete`: remove a file or directory.
- `replace`: create or replace a file using a `content` byte array and `mode`, up to 1 MiB per file.
- `copy`: copy a file from `revision` and `source_path` within the same workspace to `path`. Images and large binary files reuse existing content and attributes.

Import generated images from an allowed exchange directory with `workspace_import`, then use the returned revision as `copy.revision`. Parent directories must exist in the candidate. Resolve a directory conflict before editing its children; replacing a whole directory resolves its descendant conflicts.

Once conflicts are resolved, `merge_apply` takes the current candidate and target guard. It checks target changes, uncommitted content and mount use, then creates a revision with both source and target parents. Active mounts require `mount_release` to finish the switch. The CLI uses the same tool names and parameters:

```sh
agentdrive call revision_files --json "{\"workspace\":\"$WORKSPACE\",\"revision\":\"$SOURCE_REVISION\"}"
agentdrive call merge_resolve --request-id "$RUN_ID-edit" --json - < merge-edits.json
agentdrive call merge_apply --request-id "$RUN_ID-apply" --json - < merge-apply.json
```

Example `merge-edits.json` arguments:

```json
{
  "merge": "<merge-id>",
  "candidate": "<current-candidate-revision-id>",
  "resolutions": [
    {"choice": "source", "path": "/README.md"},
    {"choice": "copy", "path": "/cover.png", "revision": "<image-revision-id>", "source_path": "/cover.png"}
  ]
}
```

See [configuration and S3](configuration.md) for remote credentials and [CLI usage](cli.md) for executable examples.

## Checkpoints and synchronization progress

Local fsync persists the current working tree without adding a revision. Use `mount_save` or `revision_commit` for a historical version. Autosave continues on its five-second quiet/thirty-second continuous-write schedule. Remote fsync publishes the checkpoint and waits for confirmation; merged checkpoint jobs guarantee the latest durable state, while explicit revisions remain in history.

`mount_save.wait_for_remote_ms` accepts 0–120000 milliseconds (default 30000). This bounds waiting after the local save, not local capture time. A timeout returns `local_saved: true` with `remote_confirmed: false`; publication continues and the timeout alone is not a failure. Poll `operation_get` using the returned ID, or `operation_by_request` using the original request ID. Do not submit a new save to poll progress. Actual remote failures remain in `error` and persistent jobs are retried. Filesystem remote fsync has no bounded-success shortcut.

`sync_progress.stage` is `queued`, `preparing`, `transferring`, `publishing`, or `complete`. `pending_jobs` counts queued workspace jobs; `jobs_before` is the queue position when waiting; `batch_jobs` counts operations sharing this publication. `total_objects` and `total_bytes` are null until the object plan is ready; completion counters describe objects verified/transferred in the current attempt, including metadata. They exclude reused remote subtrees and are not wire-byte counters. Active counters live in memory; completed counters persist with the receipt. After restart or retry, pending operations return queue status and the next attempt starts new counters. `updated_ns` timestamps the sample. Publication confirmation remains the meaning of `remote_confirmed`.
