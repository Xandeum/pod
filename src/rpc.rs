use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use chrono::{DateTime, Utc};
use anyhow::Result;
use std::net::SocketAddr;

use crate::stats::{AppState, CombinedStats};
use crate::storage::FILE_PATH;
use tokio::fs::OpenOptions;

const RPC_PORT: u16 = 6000;

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

#[derive(Serialize)]
struct PodInfo {
    address: String,
    version: String,
    last_seen: String,
    last_seen_timestamp: u64,
}

#[derive(Serialize)]
struct PodsResponse {
    pods: Vec<PodInfo>,
    total_count: usize,
}

async fn rpc_handler(state: State<AppState>, Json(req): Json<RpcRequest>) -> Json<serde_json::Value> {
    let id = req.id.clone();
    match req.method.as_str() {
        "get-version" => {
            let version = VersionInfo { version: env!("CARGO_PKG_VERSION") };
            match serde_json::to_value(RpcResponse {
                jsonrpc: "2.0",
                result: Some(version),
                error: None,
                id: id.clone(),
            }) {
                Ok(json) => Json(json),
                Err(_) => Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32603, "message": "Internal error"},
                    "id": id
                }))
            }
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
            match serde_json::to_value(RpcResponse {
                jsonrpc: "2.0",
                result: Some(combined),
                error: None,
                id: id.clone(),
            }) {
                Ok(json) => Json(json),
                Err(_) => Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32603, "message": "Internal error"},
                    "id": id
                }))
            }
        }
        "get-pods" => {
            let peer_list = state.peer_list.read().await;
            let pods: Vec<PodInfo> = peer_list.list.iter().map(|peer| {
                let last_seen_dt = DateTime::<Utc>::from_timestamp(peer.last_seen as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                    .unwrap_or_else(|| "Invalid timestamp".to_string());
                
                PodInfo {
                    address: peer.addr.to_string(),
                    version: peer.version.clone().unwrap_or_else(|| "unknown".to_string()),
                    last_seen: last_seen_dt,
                    last_seen_timestamp: peer.last_seen,
                }
            }).collect();
            
            let pods_response = PodsResponse {
                total_count: pods.len(),
                pods,
            };
            
            match serde_json::to_value(RpcResponse {
                jsonrpc: "2.0",
                result: Some(pods_response),
                error: None,
                id: id.clone(),
            }) {
                Ok(json) => Json(json),
                Err(_) => Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32603, "message": "Internal error"},
                    "id": id
                }))
            }
        }
        _ => {
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32601,
                    "message": "Method not found"
                },
                "id": id
            }))
        }
    }
}

pub async fn start_rpc_server(
    meta: Arc<Mutex<crate::storage::Metadata>>, 
    stats: Arc<Mutex<crate::stats::Stats>>,
    peer_list: Arc<tokio::sync::RwLock<crate::gossip::PeerList>>,
    rpc_ip: String
) -> Result<()> {
    let app_state = AppState { meta, stats, peer_list };

    let app = Router::new()
        .route("/rpc", post(rpc_handler))
        .with_state(app_state);

    // Parse the IP address
    let ip_addr: std::net::IpAddr = rpc_ip.parse()
        .map_err(|e| anyhow::anyhow!("Invalid IP address '{}': {}", rpc_ip, e))?;
    
    let addr = SocketAddr::new(ip_addr, RPC_PORT);
    
    // Validate that we can actually bind to this IP address
    match tokio::net::TcpListener::bind(addr).await {
        Ok(test_listener) => {
            drop(test_listener); // Close the test listener
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Cannot bind to IP address {} on port {}: {}. \
                Make sure the IP address is available on this system.", 
                rpc_ip, RPC_PORT, e
            ));
        }
    }
    
    let visibility = if rpc_ip == "127.0.0.1" || rpc_ip == "localhost" {
        "private"
    } else if rpc_ip == "0.0.0.0" {
        "public"
    } else {
        "custom"
    };
    
    log::info!("Starting {} RPC server on {} ({}:{})", visibility, addr, rpc_ip, RPC_PORT);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
} 