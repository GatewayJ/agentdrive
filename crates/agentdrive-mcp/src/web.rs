use crate::workspace_view::WorkspaceView;
use agentdrive_config::{ConfigStore, Settings, SettingsUpdate};
use agentdrive_model::*;
use agentdrive_ports::*;
use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

pub(crate) fn api(services: Services) -> Router {
    Router::new()
        .route("/api/workspaces", get(workspaces))
        .route("/api/workspaces/{workspace}", get(workspace))
        .route("/api/workspaces/{workspace}/archive", get(archive))
        .route(
            "/api/workspaces/{workspace}/revisions/{revision}/changes",
            get(revision_changes),
        )
        .route(
            "/api/workspaces/{workspace}/revisions/{revision}/diff",
            get(revision_diff),
        )
        .route(
            "/api/workspaces/{workspace}/branches/{branch}/files",
            get(branch_files),
        )
        .route(
            "/api/workspaces/{workspace}/branches/{branch}/file",
            get(branch_file),
        )
        .route("/api/lifecycle", post(lifecycle))
        .with_state(services)
}

pub(crate) fn settings_api(settings: Arc<ConfigStore>) -> Router {
    Router::new()
        .route("/api/settings", get(get_settings).put(update_settings))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .with_state(settings)
}

async fn get_settings(
    State(settings): State<Arc<ConfigStore>>,
    Extension(actor): Extension<Principal>,
) -> std::result::Result<Json<Settings>, ApiError> {
    Ok(Json(settings.settings(&actor)?))
}

async fn update_settings(
    State(settings): State<Arc<ConfigStore>>,
    Extension(actor): Extension<Principal>,
    Json(update): Json<SettingsUpdate>,
) -> std::result::Result<Json<Settings>, ApiError> {
    let result = tokio::task::spawn_blocking(move || settings.update(&actor, update))
        .await
        .map_err(|error| Error::new(ErrorCode::Io, error.to_string()))??;
    Ok(Json(result))
}

pub(crate) fn assets() -> Router {
    Router::new()
        .route("/favicon.svg", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], include_str!("../dashboard/favicon.svg")) }))
        .route("/", get(|| async { Html(include_str!("../dashboard/index.html")) }))
        .route("/archive", get(|| async { Html(include_str!("../dashboard/index.html")) }))
        .route("/settings", get(|| async { Html(include_str!("../dashboard/index.html")) }))
        .route("/dashboard", get(|| async { Html(include_str!("../dashboard/index.html")) }))
        .route("/dashboard.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/dashboard.js")) }))
        .route("/workspace.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/workspace.js")) }))
        .route("/settings.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/settings.js")) }))
        .route("/revisions.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/revisions.js")) }))
        .route("/branch-files.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/branch-files.js")) }))
        .route("/changes.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/changes.js")) }))
        .route("/archive.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/archive.js")) }))
        .route("/file-browser.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/file-browser.js")) }))
        .route("/file-preview.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/file-preview.js")) }))
        .route("/archive.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../dashboard/archive.css")) }))
        .route("/dashboard.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../dashboard/dashboard.css")) }))
        .layer(axum::middleware::map_response(|mut response: Response| async move {
            response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
            response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
            response.headers_mut().insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
            response
        }))
}

struct ApiError(Error);
impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::InvalidArgument => StatusCode::BAD_REQUEST,
            ErrorCode::Io | ErrorCode::Integrity => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::CONFLICT,
        };
        (status, Json(json!({"error": self.0}))).into_response()
    }
}

async fn workspaces(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!(services.workspace.list(&actor).await?)))
}

async fn workspace(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Path(workspace): Path<WorkspaceId>,
) -> std::result::Result<Json<WorkspaceView>, ApiError> {
    let status = services.workspace.status(&actor, workspace).await?;
    let revisions = services.revisions.list(&actor, workspace, None).await?;
    Ok(Json(WorkspaceView::new(status, revisions)))
}

// File browsing only needs current workspace state, independent of history size.
async fn archive(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Path(workspace): Path<WorkspaceId>,
) -> std::result::Result<Json<WorkspaceStatus>, ApiError> {
    Ok(Json(services.workspace.status(&actor, workspace).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchFilesQuery {
    path: WorkspacePath,
    after: Option<WorkspacePath>,
    version: Option<String>,
}

async fn branch_files(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Path((workspace, branch)): Path<(WorkspaceId, BranchId)>,
    Query(query): Query<BranchFilesQuery>,
) -> std::result::Result<Json<BranchFiles>, ApiError> {
    Ok(Json(
        services
            .branch_files
            .list(
                &actor,
                ListBranchFiles {
                    workspace,
                    branch,
                    path: query.path,
                    after: query.after,
                    version: query.version,
                },
            )
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchFileQuery {
    path: WorkspacePath,
    version: String,
    offset: u64,
    size: u32,
}

async fn branch_file(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Path((workspace, branch)): Path<(WorkspaceId, BranchId)>,
    Query(query): Query<BranchFileQuery>,
) -> std::result::Result<Response, ApiError> {
    let bytes = services
        .branch_files
        .read(
            &actor,
            ReadBranchFile {
                workspace,
                branch,
                path: query.path,
                version: query.version,
                offset: query.offset,
                size: query.size,
            },
        )
        .await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesQuery {
    before: Option<RevisionId>,
    cursor: Option<WorkspacePath>,
    limit: Option<usize>,
}

async fn revision_changes(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Path((workspace, revision)): Path<(WorkspaceId, RevisionId)>,
    Query(query): Query<ChangesQuery>,
) -> std::result::Result<Json<RevisionDifferences>, ApiError> {
    Ok(Json(
        services
            .revisions
            .diff(
                &actor,
                CompareRevisions {
                    workspace,
                    before: query.before,
                    after: revision,
                    cursor: query.cursor,
                    limit: query.limit,
                },
            )
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDiffQuery {
    before: Option<RevisionId>,
    path: WorkspacePath,
    previous_path: Option<WorkspacePath>,
}

async fn revision_diff(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Path((workspace, revision)): Path<(WorkspaceId, RevisionId)>,
    Query(query): Query<FileDiffQuery>,
) -> std::result::Result<Json<RevisionTextDiff>, ApiError> {
    Ok(Json(
        services
            .revisions
            .text_diff(
                &actor,
                CompareRevisionFile {
                    workspace,
                    before: query.before,
                    after: revision,
                    path: query.path,
                    previous_path: query.previous_path,
                },
            )
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct LifecycleInput {
    request_id: String,
    #[serde(flatten)]
    event: LifecycleEvent,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum LifecycleEvent {
    Attach {
        workspace: WorkspaceId,
        mount_path: PathBuf,
    },
    Check {
        workspace: WorkspaceId,
        mount_path: PathBuf,
    },
    Save {
        mount: MountId,
        expected_binding_generation: u64,
    },
}

fn checked(record: OperationRecord) -> Result<OperationRecord> {
    match record.error.clone() {
        Some(error) => Err(error),
        None => Ok(record),
    }
}

async fn lifecycle(
    State(services): State<Services>,
    Extension(actor): Extension<Principal>,
    Json(input): Json<LifecycleInput>,
) -> std::result::Result<Json<Value>, ApiError> {
    let context = RequestContext {
        principal: actor,
        request_id: input.request_id,
    };
    context.validate()?;
    let value = match input.event {
        LifecycleEvent::Attach {
            workspace,
            mount_path,
        } => {
            let record = checked(
                services
                    .mounts
                    .prepare(
                        context,
                        PrepareMount {
                            workspace,
                            branch: None,
                            revision: None,
                            path: mount_path,
                            access: AccessMode::ReadWrite,
                            durability: Durability::Local,
                        },
                    )
                    .await?,
            )?;
            let Some(CommandResult::Mount { binding }) = record.result else {
                return Err(Error::new(ErrorCode::MountBusy, "mount is not ready").into());
            };
            json!({"mount_path": binding.path, "binding": binding})
        }
        LifecycleEvent::Check {
            workspace,
            mount_path,
        } => {
            let status = services
                .workspace
                .status(&context.principal, workspace)
                .await?;
            let binding = status
                .mounts
                .into_iter()
                .find(|mount| {
                    mount.path == mount_path
                        && mount.state == MountState::Ready
                        && mount.access == AccessMode::ReadWrite
                })
                .ok_or_else(|| Error::new(ErrorCode::StaleBinding, "mount is not ready"))?;
            json!({"binding": binding})
        }
        LifecycleEvent::Save {
            mount,
            expected_binding_generation,
        } => {
            json!(checked(
                services
                    .mounts
                    .save(
                        context,
                        SaveMount {
                            mount,
                            expected_binding_generation,
                            durability: Durability::Local,
                            wait_for_remote_ms: None,
                        }
                    )
                    .await?
            )?)
        }
    };
    Ok(Json(value))
}
