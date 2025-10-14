use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
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

#[derive(Serialize, Deserialize)]
struct PodInfo {
    address: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen: Option<String>,  // For backward compatibility with older servers
    last_seen_timestamp: u64,
    #[serde(default)]
    pubkey: Option<String>,
}

#[derive(Serialize, Deserialize)]
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
                PodInfo {
                    address: peer.addr.to_string(),
                    version: peer.version.clone().unwrap_or_else(|| "unknown".to_string()),
                    last_seen: None,  // Don't include formatted time in response (backward compat field)
                    last_seen_timestamp: peer.last_seen,
                    pubkey: peer.pubkey.clone(),
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

/// Query a remote pod's RPC endpoint and display gossip network information
pub async fn display_gossip_network(rpc_host: &str) -> Result<()> {
    let rpc_url = format!("http://{}:{}/rpc", rpc_host, RPC_PORT);
    
    println!("\n🔍 Querying gossip network from {}...\n", rpc_host);
    
    // Create RPC request
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "get-pods",
        "params": null,
        "id": 1
    });
    
    // Send HTTP request
    let client = reqwest::Client::new();
    let response = client
        .post(&rpc_url)
        .json(&request)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!(
            "Failed to connect to pod RPC at {}\nError: {}\n\n💡 Make sure the pod service is running at that address", 
            rpc_url, e
        ))?;
    
    let rpc_response: serde_json::Value = response.json().await?;
    
    // Debug: print the raw response (optional, can be removed later)
    if std::env::var("POD_DEBUG").is_ok() {
        eprintln!("DEBUG: RPC Response: {}", serde_json::to_string_pretty(&rpc_response).unwrap_or_default());
    }
    
    // Check for RPC error (only if error is not null)
    if let Some(error) = rpc_response.get("error") {
        if !error.is_null() {
            return Err(anyhow::anyhow!(
                "RPC endpoint returned an error: {}\n\n💡 This could mean:\n   • The pod service is starting up\n   • The RPC endpoint is not fully initialized\n   • The gossip network hasn't populated yet\n   • Try again in a few seconds\n\nDebug tip: Run with POD_DEBUG=1 for more details", 
                error
            ));
        }
    }
    
    // Parse the result
    let result = rpc_response.get("result")
        .ok_or_else(|| anyhow::anyhow!(
            "No result in RPC response\n\n💡 The RPC endpoint responded but without data.\n   Make sure the pod service is fully running."
        ))?;
    
    let pods_response: PodsResponse = serde_json::from_value(result.clone())?;
    
    // Display the gossip information
    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║                      XANDEUM POD GOSSIP NETWORK                              ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝\n");
    
    println!("📊 Total Peers: {}\n", pods_response.total_count);
    
    if pods_response.pods.is_empty() {
        println!("📭 No peers found in the network.\n");
        return Ok(());
    }
    
    // Sort peers by last_seen_timestamp (most recent first)
    let mut sorted_pods = pods_response.pods;
    sorted_pods.sort_by(|a, b| b.last_seen_timestamp.cmp(&a.last_seen_timestamp));
    
    // Print header
    println!("{:<22} {:<10} {:<15} {:<44}", "ADDRESS", "VERSION", "LAST SEEN", "PUBKEY");
    println!("{}", "─".repeat(105));
    
    let now = chrono::Utc::now().timestamp() as u64;
    
    // Print each peer
    for pod in sorted_pods {
        let last_seen = format_time_ago(now, pod.last_seen_timestamp);
        let pubkey_display = pod.pubkey.as_deref().unwrap_or("unknown");
        let pubkey_short = if pubkey_display.len() > 44 {
            format!("{}...{}", &pubkey_display[..20], &pubkey_display[pubkey_display.len()-20..])
        } else {
            pubkey_display.to_string()
        };
        
        println!("{:<22} {:<10} {:<15} {:<44}", 
                 pod.address, 
                 pod.version, 
                 last_seen,
                 pubkey_short);
    }
    
    println!("\n");
    
    Ok(())
}

/// Format the time difference as a human-readable string
fn format_time_ago(now: u64, timestamp: u64) -> String {
    let diff = now.saturating_sub(timestamp);
    
    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
} 