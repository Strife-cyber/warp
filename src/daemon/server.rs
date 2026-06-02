//! `warp serve` — REST API over the shared SQLite registry.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::download_registry::Registry;
use crate::metrics::list_host_metrics;
use crate::pipeline;

#[derive(Clone)]
struct AppState {
    registry: Registry,
    running: Arc<Mutex<bool>>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct DownloadSummary {
    id: String,
    url: String,
    status: String,
    category: String,
}

#[derive(Deserialize)]
struct AddDownloadRequest {
    url: String,
    output: Option<std::path::PathBuf>,
}

pub async fn serve(registry: Registry, port: u16) -> anyhow::Result<()> {
    let state = AppState {
        registry,
        running: Arc::new(Mutex::new(false)),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/downloads", get(list_downloads).post(add_download))
        .route("/downloads/{id}/pause", post(pause_download))
        .route("/run", post(run_downloads))
        .route("/metrics", get(metrics))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    println!("Warp daemon listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn list_downloads(State(state): State<AppState>) -> Result<Json<Vec<DownloadSummary>>, StatusCode> {
    let entries = state.registry.list().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        entries
            .into_iter()
            .map(|e| DownloadSummary {
                id: e.id,
                url: e.url,
                status: format!("{:?}", e.status),
                category: e.category.label().to_string(),
            })
            .collect(),
    ))
}

async fn add_download(
    State(state): State<AppState>,
    Json(body): Json<AddDownloadRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = body.output.unwrap_or_else(|| std::path::PathBuf::from("download.bin"));
    let id = state
        .registry
        .add(body.url, path)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn pause_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .registry
        .update_status(&id, crate::core::DownloadStatus::Paused)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(StatusCode::OK)
}

async fn run_downloads(State(state): State<AppState>) -> Result<StatusCode, StatusCode> {
    let mut running = state.running.lock().await;
    if *running {
        return Err(StatusCode::CONFLICT);
    }
    *running = true;
    let registry = state.registry.clone();
    let flag = Arc::clone(&state.running);
    tokio::spawn(async move {
        let _ = pipeline::run_all(&registry).await;
        *flag.lock().await = false;
    });
    Ok(StatusCode::ACCEPTED)
}

async fn metrics(State(state): State<AppState>) -> Result<Json<Vec<crate::metrics::HostMetrics>>, StatusCode> {
    let rows = list_host_metrics(&state.registry.pool())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows))
}
