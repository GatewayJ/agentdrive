use crate::McpServer;
use agentfs_model::{Error, Principal, Result};
use agentfs_ports::Services;
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

type HttpService = StreamableHttpService<McpServer, LocalSessionManager>;

pub struct Credential {
    pub principal: Principal,
    pub token: String,
}
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("principal", &self.principal)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub cancellation: CancellationToken,
}

pub fn router(
    services: Services,
    credentials: Vec<Credential>,
    config: GatewayConfig,
) -> Result<Router> {
    if credentials.is_empty() || config.allowed_hosts.is_empty() {
        return Err(Error::invalid(
            "MCP requires credentials and an explicit host allowlist",
        ));
    }
    let mut routes = BTreeMap::new();
    let mut principals = BTreeMap::new();
    for credential in credentials {
        if credential.token.len() < 32 || credential.token.chars().any(char::is_whitespace) {
            return Err(Error::invalid(
                "authentication tokens must contain at least 32 non-whitespace bytes",
            ));
        }
        let digest: [u8; 32] = Sha256::digest(credential.token.as_bytes()).into();
        principals.insert(digest, credential.principal.clone());
        let server = McpServer::new(services.clone(), credential.principal);
        let transport = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .with_allowed_hosts(config.allowed_hosts.clone())
                .with_allowed_origins(config.allowed_origins.clone())
                .enforce_origin_validation()
                .with_max_request_body_bytes(8 * 1024 * 1024)
                .with_json_response(true)
                .with_cancellation_token(config.cancellation.clone()),
        );
        if routes.insert(digest, transport).is_some() {
            return Err(Error::invalid("authentication tokens must be unique"));
        }
    }
    let api = crate::web::api(services)
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::new(WebAuth { principals, config }),
            authenticate_web,
        ));
    Ok(Router::new()
        .route("/mcp", any(handle))
        .with_state(Arc::new(routes))
        .merge(api)
        .merge(crate::web::assets()))
}
async fn handle(
    State(routes): State<Arc<BTreeMap<[u8; 32], HttpService>>>,
    request: Request,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(service) =
        token.and_then(|token| routes.get(&<[u8; 32]>::from(Sha256::digest(token.as_bytes()))))
    else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "authentication required",
        )
            .into_response();
    };
    service.handle(request).await.map(Body::new)
}

struct WebAuth {
    principals: BTreeMap<[u8; 32], Principal>,
    config: GatewayConfig,
}

async fn authenticate_web(
    State(auth): State<Arc<WebAuth>>,
    mut request: Request,
    next: axum::middleware::Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !auth
        .config
        .allowed_hosts
        .iter()
        .any(|allowed| allowed == host)
    {
        return (StatusCode::FORBIDDEN, "host is not allowed").into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let origin = origin.to_str().unwrap_or("");
        if origin != format!("http://{host}")
            && !auth
                .config
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        {
            return (StatusCode::FORBIDDEN, "origin is not allowed").into_response();
        }
    }
    let actor = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| {
            auth.principals
                .get(&<[u8; 32]>::from(Sha256::digest(token.as_bytes())))
        });
    let Some(actor) = actor else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "authentication required",
        )
            .into_response();
    };
    request.extensions_mut().insert(actor.clone());
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
