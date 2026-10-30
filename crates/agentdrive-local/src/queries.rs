use crate::{
    LocalBackend,
    sqlite::{read_many, read_one, sql_error},
};
use agentdrive_model::*;
use agentdrive_ports::*;
use rusqlite::OptionalExtension;

impl LocalBackend {
    pub(crate) async fn read_relationship_snapshot(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<RelationshipSnapshot>> {
        let location = self.inner.location;
        self.db(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let args = [workspace.to_string()];
            let Some(workspace) = read_one::<Workspace>(
                &transaction, "SELECT data FROM workspaces WHERE id=?1", &args,
            )? else {
                return Ok(None);
            };
            let selection: Option<String> = transaction.query_row(
                "SELECT branch FROM workspace_selections WHERE workspace=?1",
                [&args[0]], |row| row.get(0),
            ).optional().map_err(sql_error)?;
            let branches: Vec<Branch> = read_many(
                &transaction, "SELECT data FROM branches WHERE workspace=?1 ORDER BY id", &args,
            )?;
            let mounts: Vec<MountBinding> = read_many(
                &transaction,
                "SELECT m.data FROM mounts m JOIN branches b ON b.id=m.branch WHERE b.workspace=?1 ORDER BY m.id",
                &args,
            )?;
            let creations: Vec<OperationRecord> = read_many(
                &transaction,
                "SELECT data FROM operations WHERE workspace=?1 AND json_extract(CAST(data AS TEXT), '$.result.kind')='branch' \
                 UNION ALL SELECT data FROM replicated_operations WHERE workspace=?1 AND json_extract(CAST(data AS TEXT), '$.result.kind')='branch' \
                 AND id NOT IN (SELECT id FROM operations)",
                &args,
            )?;
            // Read only revisions referenced by origins or mounts, independent of history size.
            let mut ids = std::collections::BTreeSet::from([workspace.initial_revision]);
            ids.extend(branches.iter().map(|branch| branch.base_revision));
            ids.extend(mounts.iter().filter_map(|mount| mount.revision));
            for operation in &creations {
                if let Some(CommandResult::Branch { branch }) = &operation.result {
                    ids.insert(branch.base_revision);
                }
            }
            let mut revisions = Vec::new();
            for id in ids {
                if let Some(revision) = read_one(
                    &transaction,
                    "SELECT data FROM revisions WHERE workspace=?1 AND id=?2",
                    &[args[0].clone(), id.to_string()],
                )? {
                    revisions.push(revision);
                }
            }
            Ok(Some(RelationshipSnapshot {
                workspace,
                location,
                selected_branch: selection.map(|value| value.parse()).transpose()?,
                branches,
                mounts,
                remote_refs: read_many(
                    &transaction, "SELECT data FROM remote_refs WHERE workspace=?1 ORDER BY id", &args,
                )?,
                creations,
                revisions,
            }))
        }).await
    }

    pub(crate) async fn read_file_view(
        &self,
        mount: MountId,
        query: FileQuery,
    ) -> Result<Option<FileView>> {
        self.db(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let data: Option<(Vec<u8>, Vec<u8>, Vec<u8>)> = transaction
                .prepare_cached(
                    "SELECT m.data,w.data,b.data FROM mounts m \
                     JOIN branches b ON b.id=m.branch \
                     JOIN workspaces w ON w.id=b.workspace WHERE m.id=?1",
                )
                .map_err(sql_error)?
                .query_row([mount.to_string()], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()
                .map_err(sql_error)?;
            let Some((mount, workspace, branch)) = data else {
                return Ok(None);
            };
            let mut view = FileView {
                mount: decode(&mount)?,
                workspace: decode(&workspace)?,
                branch: decode(&branch)?,
                node: None,
                child: None,
                children: Vec::new(),
            };
            if view.mount.revision.is_none() {
                let inode = match &query {
                    FileQuery::Binding => None,
                    FileQuery::Inode(id) | FileQuery::Children(id) => Some(*id),
                    FileQuery::Child { parent, .. } => Some(*parent),
                };
                if let Some(inode) = inode {
                    view.node = read_one(
                        &transaction,
                        "SELECT data FROM nodes WHERE branch=?1 AND inode=?2",
                        &[view.branch.id.to_string(), inode.0.to_string()],
                    )?;
                }
                if let Some(node) = &view.node {
                    match query {
                        FileQuery::Child { name, .. } => {
                            let path = match name.as_str() {
                                "." => node.path.clone(),
                                ".." => node.path.parent().unwrap_or_else(WorkspacePath::root),
                                name => node.path.join(name)?,
                            };
                            view.child = read_one(
                                &transaction,
                                "SELECT data FROM nodes WHERE branch=?1 AND path=?2",
                                &[view.branch.id.to_string(), path.lookup_key()],
                            )?;
                        }
                        FileQuery::Children(_) => {
                            view.children = read_many(
                                &transaction,
                                "SELECT data FROM nodes WHERE branch=?1 AND parent=?2 ORDER BY path",
                                &[view.branch.id.to_string(), node.path.lookup_key()],
                            )?;
                        }
                        _ => (),
                    }
                }
            }
            Ok(Some(view))
        })
        .await
    }

    pub(crate) async fn read_retention_state(&self) -> Result<RetentionState> {
        self.db(|connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let workspaces: Vec<Workspace> =
                read_many(&transaction, "SELECT data FROM workspaces ORDER BY id", &[])?;
            let histories = {
                let mut statement = transaction
                    .prepare_cached("SELECT branch,root FROM branch_history ORDER BY branch")
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })
                    .map_err(sql_error)?;
                rows.map(|row| {
                    let (branch, root) = row.map_err(sql_error)?;
                    Ok((branch.parse()?, decode(&root)?))
                })
                .collect::<Result<Vec<_>>>()?
            };
            Ok(RetentionState {
                workspaces: workspaces
                    .into_iter()
                    .map(|workspace| workspace.id)
                    .collect(),
                branches: read_many(&transaction, "SELECT data FROM branches ORDER BY id", &[])?,
                histories,
                revisions: read_many(&transaction, "SELECT data FROM revisions ORDER BY id", &[])?,
                jobs: read_many(&transaction, "SELECT data FROM sync_jobs ORDER BY id", &[])?,
                merges: read_many(&transaction, "SELECT data FROM merges ORDER BY id", &[])?,
                pins: read_many(&transaction, "SELECT data FROM cache_pins ORDER BY id", &[])?,
            })
        })
        .await
    }
}
