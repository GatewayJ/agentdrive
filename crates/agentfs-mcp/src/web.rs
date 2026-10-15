use agentfs_model::*;
use agentfs_ports::*;
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) fn api(services: Services) -> Router {
    Router::new()
        .route("/api/workspaces", get(workspaces))
        .route("/api/workspaces/{workspace}", get(workspace))
        .route("/api/lifecycle", post(lifecycle))
        .with_state(services)
}

pub(crate) fn assets() -> Router {
    Router::new()
        .route("/favicon.svg", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], include_str!("../dashboard/favicon.svg")) }))
        .route("/", get(|| async { Html(include_str!("../dashboard/index.html")) }))
        .route("/dashboard", get(|| async { Html(include_str!("../dashboard/index.html")) }))
        .route("/dashboard.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../dashboard/dashboard.js")) }))
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
) -> std::result::Result<Json<Value>, ApiError> {
    let status = services.workspace.status(&actor, workspace).await?;
    let revisions = services.revisions.list(&actor, workspace, None).await?;
    Ok(Json(json!({"status": status, "revisions": revisions})))
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
        key: SessionKey,
        mount_path: PathBuf,
    },
    Begin {
        key: SessionKey,
        turn_id: String,
    },
    End {
        key: SessionKey,
        turn_id: String,
        status: TurnStatus,
    },
    Detach {
        key: SessionKey,
    },
}

fn checked(record: OperationRecord) -> Result<OperationRecord> {
    match record.error.clone() {
        Some(error) => Err(error),
        None => Ok(record),
    }
}

fn step(context: &RequestContext, suffix: &str) -> RequestContext {
    let fingerprint = object_ref(
        ObjectKind::RecordIndex,
        format!("{}:{suffix}", context.request_id).as_bytes(),
    );
    RequestContext {
        principal: context.principal.clone(),
        request_id: format!("lifecycle-{}", fingerprint.id),
    }
}

async fn finish_mount(
    services: &Services,
    context: &RequestContext,
    record: OperationRecord,
) -> Result<OperationRecord> {
    let record = checked(record)?;
    if record.phase != OperationPhase::WaitingForQuiesce {
        return Ok(record);
    }
    let generation = match &record.pending {
        Some(PendingAction::MountChange { old: Some(old), .. }) => old.generation,
        _ => {
            return Err(Error::new(
                ErrorCode::MountBusy,
                "mount change requires explicit release",
            ));
        }
    };
    checked(
        services
            .mounts
            .release(
                step(context, "release"),
                ReleaseMount {
                    operation: record.id,
                    expected_binding_generation: generation,
                },
            )
            .await?,
    )?;
    checked(
        services
            .operations
            .get(&context.principal, record.id)
            .await?,
    )
}

async fn attach(
    services: &Services,
    context: &RequestContext,
    key: SessionKey,
    mount_path: PathBuf,
) -> Result<Value> {
    if !mount_path.is_absolute() {
        return Err(Error::invalid("mount_path must be absolute"));
    }
    let existing = services.sessions.get(&context.principal, &key).await?;
    for suffix in ["open", "resume"] {
        match services
            .operations
            .by_request(&context.principal, &step(context, suffix).request_id)
            .await
        {
            Ok(record) => {
                let record = checked(record)?;
                let Some(CommandResult::Session { binding: original }) = &record.result else {
                    return Err(Error::new(
                        ErrorCode::RecoveryRequired,
                        "session attachment has no saved result",
                    ));
                };
                if original.key != key {
                    return Err(Error::new(
                        ErrorCode::RequestIdConflict,
                        "attachment request id belongs to another session",
                    ));
                }
                let status = services
                    .workspace
                    .status(&context.principal, key.workspace)
                    .await?;
                let original_path = status
                    .mounts
                    .iter()
                    .find(|mount| Some(mount.id) == original.mount)
                    .map(|mount| &mount.path)
                    .or(match &record.pending {
                        Some(PendingAction::MountChange { new: Some(new), .. }) => Some(&new.path),
                        _ => None,
                    });
                let requested_path = tokio::fs::canonicalize(&mount_path)
                    .await
                    .unwrap_or_else(|_| mount_path.clone());
                if original_path != Some(&requested_path) {
                    return Err(Error::new(
                        ErrorCode::RequestIdConflict,
                        "attachment request id has a different mount path",
                    ));
                }
                finish_mount(services, context, record).await?;
                let binding = services
                    .sessions
                    .get(&context.principal, &key)
                    .await?
                    .ok_or_else(|| Error::integrity("session attachment is missing"))?;
                let status = services
                    .workspace
                    .status(&context.principal, key.workspace)
                    .await?;
                let mount = status
                    .mounts
                    .iter()
                    .find(|mount| {
                        Some(mount.id) == binding.mount && mount.state == MountState::Ready
                    })
                    .ok_or_else(|| {
                        Error::new(ErrorCode::MountBusy, "session mount is not ready")
                    })?;
                return Ok(json!({"binding": binding, "mount_path": mount.path}));
            }
            Err(error) if error.code == ErrorCode::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(binding) = &existing
        && binding.state == SessionState::Active
        && let Some(turn_id) = &binding.active_turn
    {
        checked(
            services
                .sessions
                .turn_end(
                    step(context, "interrupt"),
                    EndTurn {
                        key: key.clone(),
                        turn_id: turn_id.clone(),
                        status: TurnStatus::Interrupted,
                        durability: Durability::Local,
                    },
                )
                .await?,
        )?;
    }
    let operation = if let Some(binding) = existing {
        services
            .sessions
            .resume(
                step(context, "resume"),
                ResumeSession {
                    key: key.clone(),
                    mount_path: Some(mount_path.clone()),
                    recovery_action: Some(RecoveryAction::Interrupt),
                    turn_id: binding.active_turn,
                    durability: Durability::Local,
                },
            )
            .await?
    } else {
        services
            .sessions
            .open(
                step(context, "open"),
                OpenSession {
                    key: key.clone(),
                    branch: None,
                    mount_path: Some(mount_path.clone()),
                    durability: Durability::Local,
                },
            )
            .await?
    };
    finish_mount(services, context, operation).await?;
    let binding = services
        .sessions
        .get(&context.principal, &key)
        .await?
        .ok_or_else(|| Error::integrity("session attachment is missing"))?;
    let status = services
        .workspace
        .status(&context.principal, key.workspace)
        .await?;
    let mount = status
        .mounts
        .iter()
        .find(|mount| Some(mount.id) == binding.mount && mount.state == MountState::Ready)
        .ok_or_else(|| Error::new(ErrorCode::MountBusy, "session mount is not ready"))?;
    Ok(json!({"binding": binding, "mount_path": mount.path}))
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
        LifecycleEvent::Attach { key, mount_path } => {
            attach(&services, &context, key, mount_path).await?
        }
        LifecycleEvent::Begin { key, turn_id } => json!(checked(
            services
                .sessions
                .turn_begin(context, BeginTurn { key, turn_id })
                .await?
        )?),
        LifecycleEvent::End {
            key,
            turn_id,
            status,
        } => json!(checked(
            services
                .sessions
                .turn_end(
                    context,
                    EndTurn {
                        key,
                        turn_id,
                        status,
                        durability: Durability::Local
                    }
                )
                .await?
        )?),
        LifecycleEvent::Detach { key } => {
            let paused = match services
                .operations
                .by_request(&context.principal, &step(&context, "pause").request_id)
                .await
            {
                Ok(record) => Some(checked(record)?),
                Err(error) if error.code == ErrorCode::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let paused = if let Some(record) = paused {
                Some(record)
            } else if let Some(binding) = services.sessions.get(&context.principal, &key).await? {
                if let Some(turn_id) = binding.active_turn {
                    checked(
                        services
                            .sessions
                            .turn_end(
                                step(&context, "interrupt"),
                                EndTurn {
                                    key: key.clone(),
                                    turn_id,
                                    status: TurnStatus::Interrupted,
                                    durability: Durability::Local,
                                },
                            )
                            .await?,
                    )?;
                }
                Some(checked(
                    services
                        .sessions
                        .pause(
                            step(&context, "pause"),
                            SessionAction {
                                key: key.clone(),
                                durability: Durability::Local,
                            },
                        )
                        .await?,
                )?)
            } else {
                None
            };
            if let Some(record) = paused {
                let Some(CommandResult::Session { binding }) = record.result else {
                    return Err(Error::integrity("session detachment has no saved result").into());
                };
                if binding.key != key {
                    return Err(Error::new(
                        ErrorCode::RequestIdConflict,
                        "detachment request id belongs to another session",
                    )
                    .into());
                }
                let status = services
                    .workspace
                    .status(&context.principal, key.workspace)
                    .await?;
                if let Some(mount) = status.mounts.iter().find(|mount| {
                    Some(mount.id) == binding.mount && mount.state != MountState::Released
                }) {
                    checked(
                        services
                            .mounts
                            .unmount(step(&context, "unmount"), mount.id, mount.generation)
                            .await?,
                    )?;
                }
            }
            json!({"saved": true})
        }
    };
    Ok(Json(value))
}
