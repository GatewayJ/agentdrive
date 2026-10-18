use crate::{Runtime, files::read_content};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;

const PAGE_SIZE: usize = 200;
const READ_LIMIT: u32 = 256 * 1024;

pub(crate) struct BranchFileService {
    runtime: Arc<Runtime>,
    content: Arc<dyn ContentResolver>,
    working: Arc<dyn WorkingTreeStore>,
}

impl BranchFileService {
    pub(crate) fn new(
        runtime: Arc<Runtime>,
        content: Arc<dyn ContentResolver>,
        working: Arc<dyn WorkingTreeStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            content,
            working,
        })
    }

    async fn authorize(
        &self,
        actor: &Principal,
        workspace: WorkspaceId,
        branch: BranchId,
        expected: Option<&str>,
    ) -> Result<String> {
        self.runtime
            .authorize(actor, workspace, Some(branch), Permission::Read)
            .await?;
        let branch = self.runtime.branch(branch).await?;
        if branch.state == BranchState::Deleted {
            return Err(Error::new(ErrorCode::NotFound, "branch has been deleted"));
        }
        let version = format!(
            "{}:{}:{}:{}",
            branch.id, branch.authority_epoch, branch.generation, branch.mutation_seq
        );
        if expected.is_some_and(|expected| expected != version) {
            return Err(Error::new(
                ErrorCode::TargetMoved,
                "Branch contents changed; refresh the file browser.",
            ));
        }
        Ok(version)
    }

    async fn node(&self, branch: BranchId, path: &WorkspacePath) -> Result<Node> {
        self.runtime
            .state
            .node_at(branch, path)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "path does not exist"))
    }
}

#[async_trait]
impl BranchFilesApi for BranchFileService {
    async fn list(&self, actor: &Principal, request: ListBranchFiles) -> Result<BranchFiles> {
        let _gate = self.runtime.lock_branch(request.branch).await;
        let version = self
            .authorize(
                actor,
                request.workspace,
                request.branch,
                request.version.as_deref(),
            )
            .await?;
        if request.after.is_some() && request.version.is_none() {
            return Err(Error::invalid("directory pagination requires a version"));
        }
        if request.after.as_ref().is_some_and(|after| {
            after.parent().map(|parent| parent.lookup_key()) != Some(request.path.lookup_key())
        }) {
            return Err(Error::invalid(
                "cursor must be a direct child of the directory",
            ));
        }
        let node = self.node(request.branch, &request.path).await?;
        if node.inode.kind != FileKind::Directory {
            return Err(Error::invalid("path is not a directory"));
        }
        let mut entries = self
            .runtime
            .state
            .children_page(
                request.branch,
                &request.path,
                request.after.as_ref(),
                PAGE_SIZE + 1,
            )
            .await?;
        let next = if entries.len() > PAGE_SIZE {
            entries.truncate(PAGE_SIZE);
            entries.last().map(|node| node.path.clone())
        } else {
            None
        };
        Ok(BranchFiles {
            version,
            entries,
            next,
        })
    }

    async fn read(&self, actor: &Principal, request: ReadBranchFile) -> Result<Bytes> {
        if request.size == 0 || request.size > READ_LIMIT {
            return Err(Error::invalid(
                "file reads must contain between 1 and 262144 bytes",
            ));
        }
        let gate = self.runtime.lock_branch(request.branch).await;
        self.authorize(
            actor,
            request.workspace,
            request.branch,
            Some(&request.version),
        )
        .await?;
        let node = self.node(request.branch, &request.path).await?;
        if node.inode.kind != FileKind::File {
            return Err(Error::invalid("path is not a file"));
        }
        if request.offset >= node.inode.size {
            return Ok(Bytes::new());
        }
        if node.dirty {
            return self
                .working
                .read(
                    WorkingFile {
                        workspace: request.workspace,
                        branch: request.branch,
                        inode: node.inode.id,
                    },
                    request.offset,
                    request.size,
                )
                .await;
        }
        let _objects = self.runtime.objects.read().await;
        // The immutable object remains protected while a remote read is in flight.
        drop(gate);
        read_content(
            self.content.as_ref(),
            request.workspace,
            node.inode
                .content
                .as_ref()
                .ok_or_else(|| Error::integrity("file content is missing"))?,
            request.offset,
            request.size,
        )
        .await
    }
}
