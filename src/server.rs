use anyhow::Result;
use axum::{extract::State, response::Html, routing::get, Json, Router};
use chrono::{DateTime, Utc};
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

async fn get_stats_page(meta: State<Arc<Mutex<Metadata>>>) -> Html<String> {
    let stats = meta.lock().await;

    let total_bytes_formatted = format!(
        "{} ({})",
        format_with_thousands(stats.total_bytes),
        bytes_to_mib(stats.total_bytes)
    );


    let html = STATS_TEMPLATE
        .replace("{{current_index}}", &format_with_thousands(stats.current_index))
        .replace("{{total_pages}}", &format_with_thousands(stats.total_pages))
        .replace("{{last_updated}}", &format_timestamp(stats.last_updated))
        .replace("{{total_bytes}}", &total_bytes_formatted);
    
    Html(html)
}


pub async fn start_server(meta: Arc<Mutex<Metadata>>) -> Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/stats", get(get_stats))
        .route("/stats-page", get(get_stats_page))
        .with_state(meta);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3500));
    log::info!("Starting web server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}


fn format_with_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().rev().collect();
    
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(*c);
    }
    
    result.chars().rev().collect()
}

fn bytes_to_mib(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    format!("{:.2}MiB", mib)
}

fn format_timestamp(timestamp: u64) -> String {
    let datetime = DateTime::<Utc>::from_timestamp(timestamp as i64, 0);
    match datetime {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => "Invalid timestamp".to_string(),
    }
}

const STATS_TEMPLATE: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Pod Stats</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, sans-serif;
            max-width: 800px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
            color: #333;
        }
        h1 {
            color: #2c3e50;
            text-align: center;
            margin-bottom: 30px;
        }
        .stats-table {
            width: 100%;
            border-collapse: collapse;
            background: white;
            box-shadow: 0 4px 6px rgba(0,0,0,0.1);
            border-radius: 8px;
            overflow: hidden;
        }
        th, td {
            padding: 15px;
            text-align: left;
            border-bottom: 1px solid #eee;
        }
        th {
            background-color: #2c3e50;
            color: white;
            font-weight: 600;
        }
        tr:last-child td {
            border-bottom: none;
        }
        tr:nth-child(even) {
            background-color: #f8f9fa;
        }
        .value {
            font-family: monospace;
            color: #e74c3c;
        }
        @media (max-width: 600px) {
            th, td {
                padding: 10px;
                font-size: 14px;
            }
        }
    </style>
</head>
<body>
    <h1>Pod Stats</h1>
    <table class="stats-table">
        <tr>
            <th>Metric</th>
            <th>Value</th>
        </tr>
        <tr>
            <td>Current Index</td>
            <td class="value">{{current_index}}</td>
        </tr>
        <tr>
            <td>Total Pages</td>
            <td class="value">{{total_pages}}</td>
        </tr>
        <tr>
            <td>Last Updated</td>
            <td class="value">{{last_updated}}</td>
        </tr>
        <tr>
            <td>Total Bytes</td>
            <td class="value">{{total_bytes}}</td>
        </tr>
    </table>
</body>
</html>
"#;