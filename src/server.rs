use anyhow::Result;
use axum::{routing::get, Json, Router};
use serde::Serialize;
use std::net::SocketAddr;

#[derive(Serialize)]
struct ApiResponse {
    message: String,
}

async fn root() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "Welcome".to_string(),
    })
}

pub async fn start_server() -> Result<()> {
    let app = Router::new().route("/", get(root));

    let addr = SocketAddr::from(([127, 0, 0, 1], 3500));
    log::info!("Starting web server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}
