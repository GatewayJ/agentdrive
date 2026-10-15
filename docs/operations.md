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

A remote error can accompany `local_saved: true`. Keep the existing operation ID, restore remote access, and run `sync` or allow the daemon to retry its persistent publication queue. A remote timeout does not establish whether a conditional write committed; the publisher checks the recorded operation identity.

## Mount changes

Restore, merge apply, and session changes may require an attached filesystem to become quiet. An operation in `waiting_for_quiesce` carries its original mount generation. Stop the agent and other processes using that mount, close open files, and leave the directory in any shell. Call `mount_release` with that operation ID and `expected_binding_generation`. Repeating release uses its own stable request ID.

The operation verifies the original branch guard again before changing the attachment. A stale target fails without applying the candidate. Generation checks prevent a handle from continuing against a replacement branch. A successful local commit followed by mount failure returns `committed_mount_pending`; the durable result is retained for recovery. Cancellation is accepted only while the original attachment remains unchanged and no local commit has completed.

A normal daemon shutdown stops HTTP traffic and background tasks, saves modified local branches, and requests ordinary unmounts. Busy mounts are reported for recovery. Do not remove the state directory while a daemon or filesystem is active.

## Process restart

Startup restores working metadata from the last acknowledged immutable branch root and removes disposable working copies. Writes that had not reached fsync, autosave, a revision operation, or a saved turn can be lost after abrupt process termination. A failed write followed by failed metadata persistence stops the affected branch for recovery.

An interrupted session requires `session_resume` with an explicit `recovery_action` and the affected `turn_id` when an active turn exists. `continue` resumes the recorded turn; `interrupt` records interruption before resuming. Native mounts are inspected before reuse. An unknown existing filesystem at a path requires operator cleanup; the service does not force-unmount an unrelated filesystem.

Keep the state directory on a local filesystem with correct file flush and atomic rename behavior. Filesystem and storage hardware must honor flush requests. Unix object publication flushes file data and directories; Windows flushes file data and uses write-through namespace publication. Remote confirmation is the additional durability boundary when remote storage is configured.

## Replication, cache, and ownership

`sync` supports `push`, `pull`, and `both`. Pulling a workspace onto a fresh location imports its history. Branch creation and session opening can then use the imported revisions. A remote history reference does not grant local write ownership.

Use `cache_prefetch` to download a revision or selected paths. Use `cache_pin` with `enabled: true` to retain a complete selection for offline use; `cache_status` reports both completeness and pin protection. Unpinning permits later collection after other protection is absent. Cache collection only removes remotely confirmed immutable objects without active readers or protected references.

Ownership transfer is online. Finish active turns and release writable mounts on the old location. Submit `ownership_transfer` with the current `BranchGuard` and the destination `LocationId`. The old location stops branch writes before draining publication and updating the conditional reference. Pull at the new location to adopt confirmed ownership. Retain the original transfer operation during an uncertain remote response.

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
agentfs call revision_files --json "{\"workspace\":\"$WORKSPACE\",\"revision\":\"$SOURCE_REVISION\"}"
agentfs call merge_resolve --request-id "$RUN_ID-edit" --json - < merge-edits.json
agentfs call merge_apply --request-id "$RUN_ID-apply" --json - < merge-apply.json
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
