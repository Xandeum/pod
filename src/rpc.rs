use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::stats::{AppState, CombinedStats};
use crate::storage::FILE_PATH;
use tokio::fs::OpenOptions;

#[derive(Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    method: String,
    params: Option<serde_json::Value>,
    id: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct RpcResponse<T> {
    jsonrpc: &'static str,
    result: Option<T>,
    error: Option<RpcError>,
    id: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Serialize)]
struct VersionInfo {
    version: &'static str,
}

async fn rpc_handler(state: State<AppState>, Json(req): Json<RpcRequest>) -> Json<serde_json::Value> {
    let id = req.id.clone();
    match req.method.as_str() {
        "get-version" => {
            let version = VersionInfo { version: env!("CARGO_PKG_VERSION") };
            Json(serde_json::to_value(RpcResponse {
                jsonrpc: "2.0",
                result: Some(version),
                error: None,
                id,
            }).unwrap())
        }
        "get-stats" => {
            let metadata = state.meta.lock().await;
            let stats = state.stats.lock().await;
            let mut file_size = 0u64;
            if let Ok(mut file) = OpenOptions::new().read(true).write(true).open(FILE_PATH).await {
                if let Ok(meta) = file.metadata().await {
                    file_size = meta.len();
                }
            }
            let combined = CombinedStats {
                metadata: metadata.clone(),
                stats: stats.clone(),
                file_size,
            };
            Json(serde_json::to_value(RpcResponse {
                jsonrpc: "2.0",
                result: Some(combined),
                error: None,
                id,
            }).unwrap())
        }
        _ => {
            Json(serde_json::to_value(RpcResponse::<()> {
                jsonrpc: "2.0",
                result: None,
                error: Some(RpcError {
                    code: -32601,
                    message: "Method not found".to_string(),
                }),
                id,
            }).unwrap())
        }
    }
}

pub fn rpc_router() -> Router<AppState> {
    Router::new()
        .route("/rpc", post(rpc_handler))
} 