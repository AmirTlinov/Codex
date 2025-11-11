use crate::index::IndexCoordinator;
use crate::metadata::DaemonMetadata;
use crate::project::ProjectProfile;
use crate::proto::OpenRequest;
use crate::proto::OpenResponse;
use crate::proto::PROTOCOL_VERSION;
use crate::proto::SearchRequest;
use crate::proto::SearchResponse;
use crate::proto::SnippetRequest;
use crate::proto::SnippetResponse;
use anyhow::Result;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use base64::Engine as _;
use rand::RngCore;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Clone)]
pub struct DaemonOptions {
    pub project_root: PathBuf,
    pub codex_home: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    coordinator: IndexCoordinator,
    secret: String,
}

pub async fn run_daemon(opts: DaemonOptions) -> Result<()> {
    let profile = ProjectProfile::detect(Some(&opts.project_root), opts.codex_home.as_deref())?;
    let coordinator = IndexCoordinator::new(profile.clone()).await?;
    let secret = random_secret();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let metadata = DaemonMetadata::new(
        profile.hash().to_string(),
        profile.project_root().to_string_lossy().into_owned(),
        addr.port(),
        secret.clone(),
        std::process::id(),
    );
    metadata.write_atomic(&profile.metadata_path())?;
    info!("code-finder daemon listening on {addr}");
    let state = AppState {
        coordinator,
        secret,
    };
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/v1/nav/search", post(search_handler))
        .route("/v1/nav/open", post(open_handler))
        .route("/v1/nav/snippet", post(snippet_handler))
        .route("/v1/nav/reindex", post(reindex_handler))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<crate::proto::IndexStatus>, AppError> {
    ensure_authorized(&state, &headers)?;
    Ok(Json(state.coordinator.current_status().await))
}

async fn search_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let response = state
        .coordinator
        .handle_search(request)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(response))
}

async fn open_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OpenRequest>,
) -> Result<Json<OpenResponse>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let response = state
        .coordinator
        .handle_open(request)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(response))
}

async fn snippet_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SnippetRequest>,
) -> Result<Json<SnippetResponse>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let response = state
        .coordinator
        .handle_snippet(request)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(response))
}

async fn reindex_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<crate::proto::IndexStatus>, AppError> {
    ensure_authorized(&state, &headers)?;
    let status = state
        .coordinator
        .rebuild_index()
        .await
        .map_err(AppError::internal)?;
    Ok(Json(status))
}

fn ensure_authorized(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let Some(header) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(AppError::unauthorized());
    };
    let value = header.to_str().map_err(|_| AppError::unauthorized())?;
    if value != format!("Bearer {}", state.secret) {
        return Err(AppError::unauthorized());
    }
    Ok(())
}

fn ensure_protocol_version(client_version: u32) -> Result<(), AppError> {
    if client_version != PROTOCOL_VERSION {
        return Err(AppError::version_mismatch(client_version));
    }
    Ok(())
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "missing or invalid token".to_string(),
        }
    }

    fn internal(err: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }

    fn version_mismatch(client_version: u32) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!(
                "code-finder requires protocol v{PROTOCOL_VERSION}, but client sent v{client_version}"
            ),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}
