use crate::{
    OperationService, RevisionService, Runtime, revisions::validate_label, runtime::increment,
};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use std::sync::Arc;

pub struct WorkspaceService {
    runtime: Arc<Runtime>,
    revisions: Arc<RevisionService>,
    operations: Arc<OperationService>,
    directories: Arc<dyn DirectoryAccess>,
    mount_capability: String,
    mounts: Arc<dyn MountApi>,
    driver: Arc<dyn MountDriver>,
    adoption: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for WorkspaceService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceService").finish_non_exhaustive()
    }
}

impl WorkspaceService {
    pub fn new(
        runtime: Arc<Runtime>,
        revisions: Arc<RevisionService>,
        operations: Arc<OperationService>,
        directories: Arc<dyn DirectoryAccess>,
        mount_capability: String,
        mounts: Arc<dyn MountApi>,
        driver: Arc<dyn MountDriver>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            revisions,
            operations,
            directories,
            mount_capability,
            mounts,
            driver,
            adoption: tokio::sync::Mutex::new(()),
        })
    }

    async fn create_workspace(
        &self,
        context: OperationContext,
        request: CreateWorkspace,
        source: Option<&std::path::Path>,
    ) -> Result<OperationRecord> {
        validate_label(&request.name, "workspace name")?;
        let _protection = self.runtime.objects.read().await;
        let now = self.runtime.clock.now_ns();
        let id = WorkspaceId::new();
        let initial_revision = RevisionId::new();
        let workspace = Workspace {
            id,
            name: request.name,
            owner: context.operation.principal.clone(),
            initial_revision,
            format_version: FORMAT_VERSION,
            name_policy: "portable_v1".into(),
            grants: request.grants,
            max_file_bytes: request.max_file_bytes.unwrap_or(64 * 1024 * 1024 * 1024),
            max_working_bytes: request
                .max_working_bytes
                .unwrap_or(256 * 1024 * 1024 * 1024),
            max_inodes: request.max_inodes.unwrap_or(1_000_000),
        };
        if workspace.max_inodes == 0
            || workspace.max_inodes > 1_000_000
            || workspace.max_file_bytes == 0
            || workspace.max_file_bytes > workspace.max_working_bytes
            || workspace.max_working_bytes > i64::MAX as u64
        {
            return Err(Error::invalid("invalid workspace capacity limits"));
        }
        let inode = Inode {
            id: InodeId::ROOT,
            kind: FileKind::Directory,
            mode: 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            created_ns: now,
            modified_ns: now,
            content: None,
        };
        let tree = match source {
            Some(source) => {
                self.directories
                    .import_tree(
                        &workspace,
                        source,
                        self.revisions.trees.content.local.clone(),
                    )
                    .await?
            }
            None => FileTree::from([(WorkspacePath::root(), inode)]),
        };
        let root = self.revisions.trees.save_tree(id, &tree).await?;
        let branch_id = BranchId::new();
        let revision = Revision {
            message: None,
            id: initial_revision,
            workspace: id,
            branch: Some(branch_id),
            parents: vec![],
            root: root.clone(),
            kind: RevisionKind::Commit,
            actor: workspace.owner.clone(),
            created_ns: now,
            operation: context.operation.id,
        };
        let branch = Branch {
            id: branch_id,
            workspace: id,
            name: "main".into(),
            owner: self.runtime.location,
            authority_epoch: 1,
            generation: 1,
            mutation_seq: 0,
            saved_seq: 0,
            source_generation: 1,
            formal_head: initial_revision,
            autosave_head: None,
            working_root: root,
            base_revision: initial_revision,
            source_branch: None,
            next_inode: increment(tree.values().map(|inode| inode.id.0).max().unwrap_or(1))?,
            dirty: false,
            state: BranchState::Writable,
            protected: false,
        };
        let mut operation = context.operation;
        operation.workspace = Some(id);
        operation.branch = Some(branch_id);
        operation.local_saved = true;
        operation.phase = OperationPhase::LocalSaved;
        operation.result = Some(CommandResult::Workspace {
            workspace: workspace.clone(),
        });
        let job = self
            .revisions
            .publisher
            .build_job(&branch, Some(&revision), &operation)
            .await?;
        self.runtime
            .state
            .commit(LocalCommit {
                selections: vec![WorkspaceSelection {
                    workspace: id,
                    branch: branch_id,
                }],
                new_workspaces: vec![workspace],
                new_branches: vec![branch],
                revisions: vec![revision],
                operations: vec![operation.clone()],
                sync_jobs: vec![job],
                node_changes: vec![NodeChange::Replace {
                    branch: branch_id,
                    nodes: tree
                        .into_iter()
                        .map(|(path, inode)| Node {
                            path,
                            inode,
                            dirty: false,
                        })
                        .collect(),
                }],
                ..Default::default()
            })
            .await?;
        Ok(operation)
    }
}

#[async_trait]
impl WorkspaceApi for WorkspaceService {
    async fn adopt_directory(
        &self,
        context: RequestContext,
        mut request: AdoptDirectory,
    ) -> Result<OperationRecord> {
        if !request.restart_agent {
            return Err(Error::invalid(
                "restart_agent must be true: pause file writes now and restart the Agent and its terminals after conversion",
            ));
        }
        if self.mount_capability == "macos-macfuse-feature-required" {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "directory conversion requires FUSE-T with macos-fuse-t, or macFUSE with macos-mount",
            ));
        }
        request.path = self.driver.normalize_path(&request.path).await?;
        if let Some(workspace) = request.workspace {
            self.runtime
                .authorize(&context.principal, workspace, None, Permission::Manage)
                .await?;
        }
        let _adoption = self.adoption.lock().await;
        let mut record = match self
            .operations
            .start(
                context,
                "workspace_adopt_directory",
                &request,
                request.workspace,
                None,
            )
            .await?
        {
            OperationStart::New(context) => context.operation,
            OperationStart::Existing(record) if record.phase == OperationPhase::Complete => {
                return Ok(record);
            }
            OperationStart::Existing(record) => record,
        };
        let backup = request
            .path
            .parent()
            .ok_or_else(|| Error::invalid("directory needs a parent"))?
            .join(format!(".agentdrive-backup-{}", record.id));
        let child = |suffix: &str| RequestContext {
            principal: record.principal.clone(),
            request_id: format!("{}-{suffix}", record.id),
        };
        let import_context = child("import");
        let mount_context = child("mount");
        let result: Result<MountBinding> = async {
            let mount_operation = self
                .runtime
                .state
                .operation_by_request(&record.principal, &mount_context.request_id)
                .await?;
            if mount_operation.is_none() {
                let _lifecycle = self.runtime.lifecycle.lock().await;
                if self.runtime.state.mounts().await?.iter().any(|mount| {
                    (mount.path.starts_with(&request.path) || request.path.starts_with(&mount.path))
                        && mount.state != MountState::Released
                }) {
                    return Err(Error::new(
                        ErrorCode::MountBusy,
                        "directory already has a mount binding",
                    ));
                }
            }
            let imported = if let Some(workspace) = request.workspace {
                self.import_directory(
                    import_context,
                    ImportDirectory {
                        workspace,
                        source: request.path.clone(),
                    },
                )
                .await?
            } else {
                match self
                    .operations
                    .start(
                        import_context,
                        "workspace_adopt_create",
                        &request,
                        None,
                        None,
                    )
                    .await?
                {
                    OperationStart::Existing(record) => record,
                    OperationStart::New(operation) => {
                        let create = CreateWorkspace {
                            name: request
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .ok_or_else(|| Error::invalid("directory needs a UTF-8 name"))?
                                .to_owned(),
                            grants: vec![],
                            max_file_bytes: None,
                            max_working_bytes: None,
                            max_inodes: None,
                        };
                        match self
                            .create_workspace(operation.clone(), create, Some(&request.path))
                            .await
                        {
                            Ok(record) => record,
                            Err(error) => self.operations.fail(&operation, error).await?,
                        }
                    }
                }
            };
            if let Some(error) = imported.error {
                return Err(error);
            }
            let branch = match imported.branch {
                Some(branch) if imported.local_saved => self.runtime.branch(branch).await?,
                _ => {
                    return Err(Error::new(
                        ErrorCode::RecoveryRequired,
                        "directory import has not completed",
                    ));
                }
            };
            record.workspace = Some(branch.workspace);
            record.branch = Some(branch.id);
            record.local_saved = true;
            record.phase = OperationPhase::CommittedMountPending;
            record.mount_ready = Some(false);
            record.error = None;
            record.result = Some(CommandResult::DirectoryAdoption {
                branch: branch.id,
                path: request.path.clone(),
                backup_path: backup.join("original"),
                binding: None,
                restart_required: true,
            });
            self.runtime
                .state
                .commit(LocalCommit {
                    operations: vec![record.clone()],
                    ..Default::default()
                })
                .await?;
            if mount_operation.is_none() {
                let _lifecycle = self.runtime.lifecycle.lock().await;
                if self.runtime.state.mounts().await?.iter().any(|mount| {
                    (mount.path.starts_with(&request.path) || request.path.starts_with(&mount.path))
                        && mount.state != MountState::Released
                }) {
                    return Err(Error::new(
                        ErrorCode::MountBusy,
                        "directory already has a mount binding",
                    ));
                }
                self.directories.stage_mount(&request.path, &backup).await?;
            }
            let mounted = self
                .mounts
                .prepare(
                    mount_context,
                    PrepareMount {
                        workspace: branch.workspace,
                        branch: Some(branch.id),
                        revision: None,
                        path: request.path.clone(),
                        access: AccessMode::ReadWrite,
                        durability: Durability::Local,
                    },
                )
                .await?;
            if let Some(error) = mounted.error {
                return Err(error);
            }
            match mounted.result {
                Some(CommandResult::Mount { binding }) if mounted.mount_ready == Some(true) => {
                    Ok(binding)
                }
                _ => Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "directory mount has not completed; retry the same request_id",
                )),
            }
        }
        .await;
        match result {
            Ok(binding) => {
                record.phase = OperationPhase::Complete;
                record.mount_ready = Some(true);
                record.error = None;
                record.result = Some(CommandResult::DirectoryAdoption {
                    branch: binding.branch,
                    path: request.path,
                    backup_path: backup.join("original"),
                    binding: Some(binding),
                    restart_required: true,
                });
            }
            Err(error) => {
                record.phase = OperationPhase::RecoveryRequired;
                record.error = Some(error);
            }
        }
        self.runtime
            .state
            .commit(LocalCommit {
                operations: vec![record.clone()],
                ..Default::default()
            })
            .await?;
        Ok(record)
    }

    async fn create(
        &self,
        context: RequestContext,
        request: CreateWorkspace,
    ) -> Result<OperationRecord> {
        let operation = match self
            .operations
            .start(context, "workspace_create", &request, None, None)
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        match self
            .create_workspace(operation.clone(), request, None)
            .await
        {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn list(&self, actor: &Principal) -> Result<Vec<Workspace>> {
        Ok(self
            .runtime
            .state
            .workspaces()
            .await?
            .into_iter()
            .filter(|workspace| workspace.authorize(actor, None, Permission::Read).is_ok())
            .collect())
    }

    async fn relationships(
        &self,
        actor: &Principal,
        query: WorkspaceRelationshipsQuery,
    ) -> Result<WorkspaceRelationships> {
        let snapshot = self
            .runtime
            .state
            .relationship_snapshot(query.workspace)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "workspace does not exist"))?;
        crate::relationships::query(snapshot, actor, query.filter)
    }

    async fn status(&self, actor: &Principal, workspace: WorkspaceId) -> Result<WorkspaceStatus> {
        let workspace = self
            .runtime
            .authorize(actor, workspace, None, Permission::Read)
            .await?;
        Ok(WorkspaceStatus {
            selected_branch: self.runtime.state.selected_branch(workspace.id).await?,
            location: self.runtime.location,
            branches: self.runtime.state.branches(workspace.id).await?,
            remote_branches: self.runtime.state.remote_refs(workspace.id).await?,
            mounts: self
                .runtime
                .state
                .mounts()
                .await?
                .into_iter()
                .filter(|mount| mount.workspace == workspace.id)
                .collect(),
            pending_jobs: self.runtime.state.sync_jobs(workspace.id).await?.len(),
            capabilities: Capabilities {
                format_version: FORMAT_VERSION,
                platform_mount: self.mount_capability.clone(),
                file_operations: [
                    "lookup", "getattr", "readdir", "create", "mkdir", "read", "write", "truncate",
                    "chmod", "rename", "unlink", "rmdir", "fsync",
                ]
                .map(str::to_owned)
                .to_vec(),
                remote_configured: self.revisions.trees.content.remote().is_some(),
                automatic_text_merge: false,
                physical_remote_gc: false,
            },
            storage: self.revisions.trees.content.local.stats().await?,
            workspace,
        })
    }

    async fn import_directory(
        &self,
        context: RequestContext,
        request: ImportDirectory,
    ) -> Result<OperationRecord> {
        let workspace = self
            .runtime
            .authorize(
                &context.principal,
                request.workspace,
                None,
                Permission::Write,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "workspace_import",
                &request,
                Some(workspace.id),
                None,
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            let _protection = self.runtime.objects.read().await;
            let tree = self
                .directories
                .import_tree(
                    &workspace,
                    &request.source,
                    self.revisions.trees.content.local.clone(),
                )
                .await?;
            let root = self.revisions.trees.save_tree(workspace.id, &tree).await?;
            let revision_id = RevisionId::new();
            let branch_id = BranchId::new();
            let revision = Revision {
                message: None,
                id: revision_id,
                workspace: workspace.id,
                branch: Some(branch_id),
                parents: vec![workspace.initial_revision],
                root: root.clone(),
                kind: RevisionKind::Commit,
                actor: operation.operation.principal.clone(),
                created_ns: self.runtime.clock.now_ns(),
                operation: operation.operation.id,
            };
            let branch = Branch {
                id: branch_id,
                workspace: workspace.id,
                name: format!("import-{branch_id}"),
                owner: self.runtime.location,
                authority_epoch: 1,
                generation: 1,
                mutation_seq: 0,
                saved_seq: 0,
                source_generation: 1,
                formal_head: revision_id,
                autosave_head: None,
                working_root: root,
                base_revision: workspace.initial_revision,
                source_branch: self
                    .revisions
                    .trees
                    .revision(workspace.id, workspace.initial_revision)
                    .await?
                    .branch,
                next_inode: increment(tree.values().map(|inode| inode.id.0).max().unwrap_or(1))?,
                dirty: false,
                state: BranchState::Writable,
                protected: false,
            };
            let mut record = operation.operation.clone();
            record.branch = Some(branch_id);
            record.phase = OperationPhase::LocalSaved;
            record.local_saved = true;
            record.result = Some(CommandResult::Branch {
                branch: branch.clone(),
            });
            let job = self
                .revisions
                .publisher
                .build_job(&branch, Some(&revision), &record)
                .await?;
            self.runtime
                .state
                .commit(LocalCommit {
                    new_branches: vec![branch],
                    revisions: vec![revision],
                    operations: vec![record.clone()],
                    sync_jobs: vec![job],
                    node_changes: vec![NodeChange::Replace {
                        branch: branch_id,
                        nodes: tree
                            .into_iter()
                            .map(|(path, inode)| Node {
                                path,
                                inode,
                                dirty: false,
                            })
                            .collect(),
                    }],
                    ..Default::default()
                })
                .await?;
            Ok(record)
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn export_revision(
        &self,
        context: RequestContext,
        request: ExportRevision,
    ) -> Result<OperationRecord> {
        self.runtime
            .authorize(
                &context.principal,
                request.workspace,
                None,
                Permission::Read,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "revision_export",
                &request,
                Some(request.workspace),
                None,
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            let _protection = self.runtime.objects.read().await;
            let revision = self
                .revisions
                .trees
                .revision(request.workspace, request.revision)
                .await?;
            let mut tree = self
                .revisions
                .trees
                .tree(request.workspace, &revision.root)
                .await?;
            if let Some(paths) = request.paths {
                if paths.is_empty() {
                    return Err(Error::invalid("export paths must not be empty"));
                }
                for path in &paths {
                    if !tree
                        .keys()
                        .any(|existing| existing.lookup_key() == path.lookup_key())
                    {
                        return Err(Error::new(
                            ErrorCode::NotFound,
                            "export path does not exist",
                        ));
                    }
                }
                tree.retain(|path, _| {
                    paths
                        .iter()
                        .any(|selected| selected.contains(path) || path.contains(selected))
                });
            }
            self.directories
                .export_tree(
                    request.workspace,
                    tree,
                    &request.destination,
                    self.revisions.trees.content.clone(),
                )
                .await?;
            self.operations
                .finish(
                    operation.clone(),
                    CommandResult::Updated,
                    LocalCommit::default(),
                )
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }
}
