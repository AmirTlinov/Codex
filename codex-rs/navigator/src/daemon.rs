use crate::index::IndexCoordinator;
use crate::metadata::DaemonMetadata;
use crate::project::ProjectProfile;
use crate::proto::AtlasRequest;
use crate::proto::AtlasResponse;
use crate::proto::DoctorReport;
use crate::proto::IndexStatus;
use crate::proto::OpenRequest;
use crate::proto::OpenResponse;
use crate::proto::PROTOCOL_VERSION;
use crate::proto::ProfileRequest;
use crate::proto::ProfileResponse;
use crate::proto::ReindexRequest;
use crate::proto::SearchRequest;
use crate::proto::SearchStreamEvent;
use crate::proto::SnippetRequest;
use crate::proto::SnippetResponse;
use crate::proto::UpdateSettingsRequest;
use crate::workspace::WorkspaceHandle;
use crate::workspace::WorkspaceRegistry;
use anyhow::Result;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::body::Bytes;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use base64::Engine as _;
use futures::StreamExt;
use rand::RngCore;
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::info;

#[derive(Clone)]
pub struct DaemonOptions {
    pub project_root: PathBuf,
    pub codex_home: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    registry: Arc<WorkspaceRegistry>,
    default_root: String,
    secret: String,
    pid: u32,
}

impl AppState {
    async fn workspace(
        &self,
        override_root: Option<String>,
    ) -> Result<Arc<WorkspaceHandle>, AppError> {
        let root = override_root
            .and_then(|value| {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
            .unwrap_or_else(|| self.default_root.clone());
        self.registry
            .checkout(&root)
            .await
            .map_err(AppError::internal)
    }
}

pub async fn run_daemon(opts: DaemonOptions) -> Result<()> {
    let profile = ProjectProfile::detect(Some(&opts.project_root), opts.codex_home.as_deref())?;
    let auto_indexing = std::env::var("NAVIGATOR_AUTO_INDEXING")
        .ok()
        .and_then(|value| {
            let trimmed = value.trim().to_ascii_lowercase();
            match trimmed.as_str() {
                "0" | "false" | "off" => Some(false),
                "1" | "true" | "on" => Some(true),
                _ => None,
            }
        })
        .unwrap_or(true);
    let max_workspaces = std::env::var("NAVIGATOR_MAX_WORKSPACES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4);
    let codex_home = Some(profile.codex_home().to_path_buf());
    let registry = Arc::new(WorkspaceRegistry::new(
        max_workspaces,
        auto_indexing,
        codex_home,
    ));
    let default_root = profile.project_root().to_string_lossy().into_owned();
    // Prime default workspace so the first request is warm.
    let _ = registry.checkout(&default_root).await?;

    let secret = random_secret();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let pid = std::process::id();
    let metadata = DaemonMetadata::new(
        profile.hash().to_string(),
        default_root.clone(),
        addr.port(),
        secret.clone(),
        pid,
    );
    metadata.write_atomic(&profile.shared_metadata_path())?;
    info!("navigator daemon listening on {addr}");
    let state = AppState {
        registry,
        default_root,
        secret,
        pid,
    };
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/v1/nav/search", post(search_handler))
        .route("/v1/nav/open", post(open_handler))
        .route("/v1/nav/snippet", post(snippet_handler))
        .route("/v1/nav/atlas", post(atlas_handler))
        .route("/v1/nav/profile", post(profile_handler))
        .route("/v1/nav/reindex", post(reindex_handler))
        .route("/v1/nav/settings", post(settings_handler))
        .route("/v1/nav/doctor", get(doctor_handler))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<IndexStatus>, AppError> {
    ensure_authorized(&state, &headers)?;
    let workspace = state.workspace(query.project_root).await?;
    Ok(Json(workspace.coordinator().current_status().await))
}

async fn search_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SearchRequest>,
) -> Result<Response, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let workspace = state.workspace(request.project_root.clone()).await?;
    let coordinator = workspace.coordinator().clone();
    let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
    let stream = UnboundedReceiverStream::new(rx).map(Ok::<Bytes, Infallible>);
    tokio::spawn(async move {
        if let Err(err) = stream_search_response(coordinator, request, tx.clone()).await {
            let _ = send_stream_event(
                &tx,
                &SearchStreamEvent::Error {
                    message: err.message,
                },
            );
        }
    });
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .map_err(AppError::internal)?;
    Ok(response)
}

async fn open_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OpenRequest>,
) -> Result<Json<OpenResponse>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let workspace = state.workspace(request.project_root.clone()).await?;
    let response = workspace
        .coordinator()
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
    let workspace = state.workspace(request.project_root.clone()).await?;
    let response = workspace
        .coordinator()
        .handle_snippet(request)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(response))
}

async fn profile_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProfileRequest>,
) -> Result<Json<ProfileResponse>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let workspace = state.workspace(request.project_root.clone()).await?;
    let coordinator = workspace.coordinator();
    let (samples, hotspots) = tokio::join!(
        coordinator.profile_snapshot(request.limit),
        coordinator.stage_hotspots(),
    );
    Ok(Json(ProfileResponse { samples, hotspots }))
}

async fn atlas_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AtlasRequest>,
) -> Result<Json<AtlasResponse>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let workspace = state.workspace(request.project_root.clone()).await?;
    let snapshot = workspace.coordinator().atlas_snapshot().await;
    Ok(Json(AtlasResponse { snapshot }))
}

async fn reindex_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ReindexRequest>,
) -> Result<Json<IndexStatus>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let workspace = state.workspace(request.project_root.clone()).await?;
    let status = workspace
        .coordinator()
        .rebuild_index()
        .await
        .map_err(AppError::internal)?;
    Ok(Json(status))
}

async fn settings_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateSettingsRequest>,
) -> Result<Json<IndexStatus>, AppError> {
    ensure_authorized(&state, &headers)?;
    ensure_protocol_version(request.schema_version)?;
    let workspace = state.workspace(request.project_root.clone()).await?;
    if let Some(auto_indexing) = request.auto_indexing {
        workspace
            .coordinator()
            .set_auto_indexing(auto_indexing)
            .await;
    }
    Ok(Json(workspace.coordinator().current_status().await))
}

async fn doctor_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DoctorReport>, AppError> {
    ensure_authorized(&state, &headers)?;
    let report = state.registry.doctor_report(state.pid).await;
    Ok(Json(report))
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
                "navigator requires protocol v{PROTOCOL_VERSION}, but client sent v{client_version}"
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

#[derive(Deserialize, Default)]
struct WorkspaceQuery {
    project_root: Option<String>,
}

async fn stream_search_response(
    coordinator: IndexCoordinator,
    request: SearchRequest,
    sender: mpsc::UnboundedSender<Bytes>,
) -> Result<(), AppError> {
    let diagnostics = coordinator.diagnostics().await;
    send_stream_event(&sender, &SearchStreamEvent::Diagnostics { diagnostics })?;
    let response = coordinator
        .handle_search(request)
        .await
        .map_err(AppError::internal)?;
    let top_hits: Vec<_> = response.hits.iter().take(5).cloned().collect();
    if !top_hits.is_empty() {
        send_stream_event(&sender, &SearchStreamEvent::TopHits { hits: top_hits })?;
    }
    send_stream_event(&sender, &SearchStreamEvent::Final { response })?;
    Ok(())
}

fn send_stream_event(
    sender: &mpsc::UnboundedSender<Bytes>,
    event: &SearchStreamEvent,
) -> Result<(), AppError> {
    let mut payload = serde_json::to_vec(event).map_err(AppError::internal)?;
    payload.push(b'\n');
    sender
        .send(Bytes::from(payload))
        .map_err(|_| AppError::internal("stream closed"))
}
