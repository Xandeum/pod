use anyhow::Result;
use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;

use crate::storage::Metadata;

#[derive(Serialize)]
struct ApiResponse {
    message: String,
}

async fn root() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "Welcome".to_string(),
    })
}

async fn get_stats(meta: State<Arc<Mutex<Metadata>>>) -> Json<Metadata> {
    let stats = meta.lock().await;
    Json(stats.clone())
}

pub async fn start_server(meta: Arc<Mutex<Metadata>>) -> Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/stats", get(get_stats))
        .with_state(meta);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3500));
    log::info!("Starting web server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}
