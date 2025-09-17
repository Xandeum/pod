use anyhow::Result;
use axum::{extract::State, response::Html, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::Arc,
};
use tokio::{fs::OpenOptions, sync::{Mutex, RwLock}};

use crate::{
    stats::{update_system_stats, AppState, CombinedStats, Stats},
    storage::{Metadata, FILE_PATH},
    gossip::PeerList,
};

const STATS_PORT: u16 = 80;

#[derive(Serialize)]
struct ApiResponse {
    message: String,
}

async fn root() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "Welcome".to_string(),
    })
}

async fn get_stats(state: State<AppState>) -> Json<CombinedStats> {

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(FILE_PATH)
        .await.unwrap();
    let file_size = file.metadata().await.unwrap().len();

    let metadata = state.meta.lock().await;
    let stats = state.stats.lock().await;
    Json(CombinedStats {
        metadata: metadata.clone(),
        stats: stats.clone(),
        file_size
    })
}

async fn get_stats_page(state: State<AppState>) -> Html<String> {
    
    let metadata = state.meta.lock().await;
    let stats = state.stats.lock().await;

    let total_bytes_formatted = format!(
        "{} ({})",
        format_with_thousands(metadata.total_bytes),
        bytes_to_mib(metadata.total_bytes)
    );

    let packets_received_per_min = stats.packets_received * 60;
    let packets_sent_per_min = stats.packets_sent * 60;
    let uptime_formatted = format_uptime(stats.uptime);

    let html = STATS_TEMPLATE
        .replace("{{last_updated}}", &format_timestamp(metadata.last_updated))
        .replace("{{total_bytes}}", &total_bytes_formatted)
        .replace(
            "{{packets_received}}",
            &format_with_thousands(packets_received_per_min),
        )
        .replace(
            "{{packets_sent}}",
            &format_with_thousands(packets_sent_per_min),
        )
        .replace("{{uptime}}", &uptime_formatted);

    Html(html)
}

pub async fn start_server(
    meta: Arc<Mutex<Metadata>>, 
    stats: Arc<Mutex<Stats>>,
    peer_list: Arc<RwLock<PeerList>>
) -> Result<()> {
    let stats_clone = stats.clone();
    tokio::spawn(async move {
        update_system_stats(stats_clone).await;
    });

    let app_state = AppState { meta, stats, peer_list };

    let app = Router::new()
        .route("/", get(root))
        .route("/stats", get(get_stats))
        .route("/stats-page", get(get_stats_page))
        .with_state(app_state);

    let addr = SocketAddr::from(([127, 0, 0, 1], STATS_PORT));
    log::info!("Starting private stats server on {}", addr);
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

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        let mib = bytes as f64 / (1024.0 * 1024.0);
        format!("{:.2} MiB/s", mib)
    } else if bytes >= 1024 {
        let kib = bytes as f64 / 1024.0;
        format!("{:.2} KiB/s", kib)
    } else {
        format!("{} B/s", bytes)
    }
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / (24 * 3600);
    let seconds = seconds % (24 * 3600);
    let hours = seconds / 3600;
    let seconds = seconds % 3600;
    let minutes = seconds / 60;
    let seconds = seconds % 60;

    let mut result = String::new();
    if days > 0 {
        result.push_str(&format!("{}d ", days));
    }
    if hours > 0 || days > 0 {
        result.push_str(&format!("{}h ", hours));
    }
    if minutes > 0 || hours > 0 || days > 0 {
        result.push_str(&format!("{}m ", minutes));
    }
    result.push_str(&format!("{}s", seconds));
    result.trim().to_string()
}

const STATS_TEMPLATE: &str = r###"
<!DOCTYPE html>
<html lang="en" class="dark">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Pod Stats Monitor</title>
    <script src="https://cdn.tailwindcss.com"></script>
    <style>
        ::-webkit-scrollbar {
            width: 8px;
            height: 8px;
        }
        ::-webkit-scrollbar-track {
            background: #1e293b;
        }
        ::-webkit-scrollbar-thumb {
            background: #475569;
            border-radius: 4px;
        }
        ::-webkit-scrollbar-thumb:hover {
            background: #64748b;
        }

        html, body {
            margin: 0;
            padding: 0;
            width: 100%;
            overflow-x: hidden;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, 'Open Sans', 'Helvetica Neue', sans-serif;
        }
        body {
            background: linear-gradient(135deg, #111827 0%, #1f2937 100%);
            color: #d1d5db;
        }
        .status.connected {
            color: #22c55e;
        }
        .status.disconnected {
            color: #ef4444;
        }
        svg.chart-svg {
            width: 100%;
            height: 120px;
            background: #1e293b;
        }
        svg.pie-chart-svg {
            width: 100%;
            height: 120px;
        }
        .x-axis, .y-axis {
            stroke: #4b5563;
            stroke-width: 1;
        }
        .grid-line {
            stroke: #374151;
            stroke-width: 0.5;
            stroke-opacity: 0.3;
        }
        .x-axis-labels, .y-axis-labels, .pie-label {
            fill: #9ca3af;
            font-size: 9px;
            font-family: monospace;
        }
        .data-line {
            fill: none;
            stroke-width: 2;
        }
        .data-line.cpu {
            stroke: #ef4444;
        }
        .data-line.ram {
            stroke: #eab308;
        }
        .data-line.totalBytes {
            stroke: url(#gradientTotalBytes);
        }
        .data-line.packetsReceived {
            stroke: #22c55e;
        }
        .data-line.packetsSent {
            stroke: #f97316;
        }
        .pie-used {
            fill: #22c55e;
            opacity: 0.7;
            transition: opacity 0.2s, stroke 0.2s;
        }
        .pie-idle {
            fill: #3b82f6;
            opacity: 0.7;
            transition: opacity 0.2s, stroke 0.2s;
        }
        .pie-used:hover, .pie-idle:hover {
            opacity: 1;
            stroke: #ffffff;
            stroke-width: 2;
        }
        .legend-item {
            cursor: pointer;
        }
        .legend-item:hover .legend-label {
            color: #ffffff;
        }
    </style>
    <script>
        tailwind.config = {
            darkMode: 'class',
            theme: {
                extend: {
                    colors: {
                        'custom-dark-start': '#111827',
                        'custom-dark-end': '#1f2937',
                        'card-bg': '#1f2937',
                        'card-border': '#374151',
                        'text-primary': '#d1d5db',
                        'text-secondary': '#9ca3af',
                        'accent-green': '#22c55e',
                        'accent-red': '#ef4444',
                        'accent-yellow': '#eab308',
                        'accent-orange': '#f97316',
                        'accent-blue': '#3b82f6',
                    }
                }
            }
        }
    </script>
</head>
<body class="bg-gradient-to-br from-custom-dark-start to-custom-dark-end text-text-primary">
    <div class="container mx-auto p-2 sm:p-3">
        <header class="mb-3 flex justify-between items-center py-3 px-4 bg-card-bg rounded-lg shadow-xl border border-card-border">
            <h1 class="text-xl sm:text-2xl font-bold text-white">Pod Monitor</h1>
            <div class="status connected text-sm" id="serverStatus">Running</div>
        </header>

        <main class="space-y-3">
            <section class="grid grid-cols-1 sm:grid-cols-2 gap-3">
                <div class="stat-card bg-card-bg p-3 rounded-lg shadow-xl border border-card-border text-center">
                    <h3 class="text-xs font-medium text-text-secondary mb-1">Uptime</h3>
                    <div class="text-lg font-semibold text-white" id="uptime">{{uptime}}</div>
                </div>
                <div class="stat-card bg-card-bg p-3 rounded-lg shadow-xl border border-card-border text-center">
                    <h3 class="text-xs font-medium text-text-secondary mb-1">Total Bytes</h3>
                    <div class="text-lg font-semibold text-white" id="totalBytes">{{total_bytes}}</div>
                </div>
            </section>

            <section class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-2 gap-3">
                <!-- Row 1 -->
                <div class="chart-card bg-card-bg p-4 rounded-lg shadow-xl border border-card-border">
                    <h3 class="text-md font-semibold text-text-secondary mb-3">Page Usage</h3>
                    <svg id="pageUsageChart" class="pie-chart-svg" viewBox="0 0 200 140">
                        <g class="pie-group"></g>
                    </svg>
                    <div class="flex justify-center mt-2 space-x-4 text-sm text-text-secondary">
                        <div class="legend-item flex items-center" data-segment="pie-used">
                            <span class="inline-block w-3 h-3 bg-accent-green mr-1"></span>
                            <span class="legend-label">Pages Used</span>
                        </div>
                        <div class="legend-item flex items-center" data-segment="pie-idle">
                            <span class="inline-block w-3 h-3 bg-accent-blue mr-1"></span>
                            <span class="legend-label">Pages Idle</span>
                        </div>
                    </div>
                </div>
                <div class="chart-card bg-card-bg p-4 rounded-lg shadow-xl border border-card-border">
                    <h3 class="text-md font-semibold text-text-secondary mb-1">Total Bytes Transferred</h3>
                    <div class="text-xl font-semibold text-white mb-3" id="totalBytesValue">{{total_bytes}}</div>
                    <svg id="totalBytesChart" class="chart-svg h-40">
                        <defs>
                            <linearGradient id="gradientTotalBytes" x1="0%" y1="0%" x2="100%" y2="0%">
                                <stop offset="0%" style="stop-color:#22c55e;stop-opacity:1" />
                                <stop offset="100%" style="stop-color:#3b82f6;stop-opacity:1" />
                            </linearGradient>
                        </defs>
                        <g class="grid-lines"></g>
                        <g class="x-axis"></g>
                        <g class="y-axis"></g>
                        <path class="data-line totalBytes" d="" />
                    </svg>
                </div>
                <!-- Row 2 -->
                <div class="chart-card bg-card-bg p-4 rounded-lg shadow-xl border border-card-border">
                    <h3 class="text-md font-semibold text-text-secondary mb-1">CPU Usage (%)</h3>
                    <div class="text-xl font-semibold text-white mb-3" id="cpuPercent">0%</div>
                    <svg id="cpuChart" class="chart-svg h-40">
                        <g class="grid-lines"></g>
                        <g class="x-axis"></g>
                        <g class="y-axis"></g>
                        <path class="data-line cpu" d="" />
                    </svg>
                </div>
                <div class="chart-card bg-card-bg p-4 rounded-lg shadow-xl border border-card-border">
                    <h3 class="text-md font-semibold text-text-secondary mb-1">RAM Usage (%)</h3>
                    <div class="text-xl font-semibold text-white mb-3" id="ramPercent">0%</div>
                    <svg id="ramChart" class="chart-svg h-40">
                        <g class="grid-lines"></g>
                        <g class="x-axis"></g>
                        <g class="y-axis"></g>
                        <path class="data-line ram" d="" />
                    </svg>
                </div>
                <!-- Row 3 -->
                <div class="chart-card bg-card-bg p-4 rounded-lg shadow-xl border border-card-border">
                    <h3 class="text-md font-semibold text-text-secondary mb-1">Packets Received/Min</h3>
                    <div class="text-xl font-semibold text-white mb-3" id="packetsReceived">{{packets_received}}</div>
                    <svg id="packetsReceivedChart" class="chart-svg h-40">
                        <g class="grid-lines"></g>
                        <g class="x-axis"></g>
                        <g class="y-axis"></g>
                        <path class="data-line packetsReceived" d="" />
                    </svg>
                </div>
                <div class="chart-card bg-card-bg p-4 rounded-lg shadow-xl border border-card-border">
                    <h3 class="text-md font-semibold text-text-secondary mb-1">Packets Sent/Min</h3>
                    <div class="text-xl font-semibold text-white mb-3" id="packetsSent">{{packets_sent}}</div>
                    <svg id="packetsSentChart" class="chart-svg h-40">
                        <g class="grid-lines"></g>
                        <g class="x-axis"></g>
                        <g class="y-axis"></g>
                        <path class="data-line packetsSent" d="" />
                    </svg>
                </div>
            </section>
        </main>

        <footer class="mt-3 text-center py-3">
            <p class="text-xs text-text-secondary">Last Updated: <span id="lastUpdated">{{last_updated}}</span></p>
        </footer>
    </div>

    <script>
        // Formatting functions
        function formatWithThousands(n) {
            if (typeof n !== 'number' && typeof n !== 'string') return 'N/A';
            return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
        }
        function bytesToMiB(bytes) {
            if (typeof bytes !== 'number') return 'N/A MiB';
            const mib = bytes / (1024 * 1024);
            return `${mib.toFixed(2)} MiB`;
        }
        function formatTimestamp(timestamp) {
            if (typeof timestamp !== 'number' || isNaN(timestamp) || timestamp === 0) return 'N/A';
            const date = new Date(timestamp * 1000);
            return date.toUTCString().replace(' GMT', ' UTC');
        }
        function formatUptime(seconds) {
            if (typeof seconds !== 'number' || isNaN(seconds)) return 'N/A';
            const days = Math.floor(seconds / (24 * 3600));
            let remainder = seconds % (24 * 3600);
            const hours = Math.floor(remainder / 3600);
            remainder %= 3600;
            const minutes = Math.floor(remainder / 60);
            const secs = remainder % 60;
            let result = '';
            if (days > 0) result += `${days}d `;
            if (hours > 0 || days > 0) result += `${hours}h `;
            if (minutes > 0 || hours > 0 || days > 0) result += `${minutes}m `;
            result += `${secs}s`;
            return result.trim() || '0s';
        }

        // Chart setup
        let chartData = [];
        let svgWidth = 0;
        const defaultSvgHeight = 120;
        const padding = { top: 10, right: 10, bottom: 25, left: 45 };
        let chartWidth = 0;
        const PAGE_SIZE = 1048576; // 1 MiB, from pod::storage

        // Cache for latest stats
        let latestStats = {
            uptime: 0,
            total_pages: 0,
            total_bytes: 0,
            cpu_percent: 0,
            ram_used: 0,
            ram_total: 1,
            packets_sent: 0,
            packets_received: 0,
            last_updated: Math.floor(Date.now() / 1000),
            file_size: 0
        };
        let isServerConnected = true;

        function initializeSvgWidth(svgElement) {
            const container = svgElement.parentElement;
            svgWidth = container ? container.clientWidth : 300;
            chartWidth = svgWidth - padding.left - padding.right;
            if (chartWidth < 0) chartWidth = 0;
        }

        function setupAxes(svgId) {
            const svgElement = document.getElementById(svgId);
            if (!svgElement) return;

            initializeSvgWidth(svgElement);
            const actualSvgHeight = svgElement.clientHeight || defaultSvgHeight;
            const actualChartHeight = actualSvgHeight - padding.top - padding.bottom;

            svgElement.setAttribute('width', svgWidth);

            const xAxisGroup = svgElement.querySelector('.x-axis');
            const yAxisGroup = svgElement.querySelector('.y-axis');
            const gridGroup = svgElement.querySelector('.grid-lines');
            if (!xAxisGroup || !yAxisGroup || !gridGroup) return;

            while (xAxisGroup.firstChild) xAxisGroup.removeChild(xAxisGroup.firstChild);
            while (yAxisGroup.firstChild) yAxisGroup.removeChild(yAxisGroup.lastChild);
            while (gridGroup.firstChild) gridGroup.removeChild(gridGroup.firstChild);

            const xAxisLine = document.createElementNS('http://www.w3.org/2000/svg', 'line');
            xAxisLine.setAttribute('x1', padding.left);
            xAxisLine.setAttribute('y1', actualSvgHeight - padding.bottom);
            xAxisLine.setAttribute('x2', svgWidth - padding.right);
            xAxisLine.setAttribute('y2', actualSvgHeight - padding.bottom);
            xAxisLine.classList.add('x-axis');
            xAxisGroup.appendChild(xAxisLine);

            const yAxisLine = document.createElementNS('http://www.w3.org/2000/svg', 'line');
            yAxisLine.setAttribute('x1', padding.left);
            yAxisLine.setAttribute('y1', padding.top);
            yAxisLine.setAttribute('x2', padding.left);
            yAxisLine.setAttribute('y2', actualSvgHeight - padding.bottom);
            yAxisLine.classList.add('y-axis');
            yAxisGroup.appendChild(yAxisLine);

            const numYTicks = 3;
            for (let i = 0; i <= numYTicks; i++) {
                const y = actualSvgHeight - padding.bottom - (i / numYTicks) * actualChartHeight;
                const gridLine = document.createElementNS('http://www.w3.org/2000/svg', 'line');
                gridLine.setAttribute('x1', padding.left);
                gridLine.setAttribute('y1', y);
                gridLine.setAttribute('x2', svgWidth - padding.right);
                gridLine.setAttribute('y2', y);
                gridLine.classList.add('grid-line');
                gridGroup.appendChild(gridLine);
            }
            const numTimeTicks = Math.min(Math.floor(chartWidth / 80), 5);
            for (let i = 0; i <= numTimeTicks; i++) {
                const x = padding.left + (i / numTimeTicks) * chartWidth;
                const gridLine = document.createElementNS('http://www.w3.org/2000/svg', 'line');
                gridLine.setAttribute('x1', x);
                gridLine.setAttribute('y1', padding.top);
                gridLine.setAttribute('x2', x);
                gridLine.setAttribute('y2', actualSvgHeight - padding.bottom);
                gridLine.classList.add('grid-line');
                gridGroup.appendChild(gridLine);
            }
        }

        function updatePieChart(pagesUsed, pagesIdle) {
            const svgElement = document.getElementById('pageUsageChart');
            if (!svgElement) {
                console.warn('Pie chart SVG not found.');
                return;
            }

            const pieGroup = svgElement.querySelector('.pie-group');
            if (!pieGroup) return;

            while (pieGroup.firstChild) pieGroup.removeChild(pieGroup.firstChild);

            const totalPages = pagesUsed + pagesIdle;
            console.log('Pie chart data:', { pagesUsed, pagesIdle, totalPages, file_size: latestStats.file_size, PAGE_SIZE });

            if (totalPages <= 0 || isNaN(totalPages) || latestStats.file_size < PAGE_SIZE) {
                const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                text.setAttribute('x', 100);
                text.setAttribute('y', 70);
                text.setAttribute('text-anchor', 'middle');
                text.classList.add('pie-label');
                text.textContent = 'N/A';
                pieGroup.appendChild(text);
                console.warn('Invalid pie chart data, displaying N/A:', { totalPages, file_size: latestStats.file_size });
                return;
            }

            const usedPercent = (pagesUsed / totalPages) * 100;
            const idlePercent = (pagesIdle / totalPages) * 100;
            console.log('Pie chart percentages:', { usedPercent, idlePercent });

            const radius = 50;
            pieGroup.setAttribute('transform', `translate(100, 70)`);
            let startAngle = 0;

            // Pages in Use segment
            const usedAngle = (usedPercent / 100) * 2 * Math.PI;
            const usedEndAngle = startAngle + usedAngle;
            const usedLargeArc = usedAngle > Math.PI ? 1 : 0;
            const usedPathData = [
                `M 0,0`,
                `L ${radius * Math.cos(startAngle)},${radius * Math.sin(startAngle)}`,
                `A ${radius},${radius} 0 ${usedLargeArc},1 ${radius * Math.cos(usedEndAngle)},${radius * Math.sin(usedEndAngle)}`,
                `Z`
            ].join(' ');
            console.log('Used path data:', usedPathData);
            const usedPath = document.createElementNS('http://www.w3.org/2000/svg', 'path');
            usedPath.setAttribute('id', 'pie-used-path');
            usedPath.setAttribute('d', usedPathData);
            usedPath.classList.add('pie-used');
            pieGroup.appendChild(usedPath);

            // Used percentage label
            if (usedPercent > 5) {
                const usedLabelAngle = startAngle + usedAngle / 2;
                const usedLabelX = (radius * 0.7) * Math.cos(usedLabelAngle);
                const usedLabelY = (radius * 0.7) * Math.sin(usedLabelAngle);
                const usedLabel = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                usedLabel.setAttribute('x', usedLabelX);
                usedLabel.setAttribute('y', usedLabelY);
                usedLabel.setAttribute('text-anchor', 'middle');
                usedLabel.classList.add('pie-label');
                usedLabel.textContent = `${usedPercent.toFixed(1)}%`;
                pieGroup.appendChild(usedLabel);
            }

            // Pages Idle segment
            startAngle = usedEndAngle;
            const idleAngle = (idlePercent / 100) * 2 * Math.PI;
            const idleEndAngle = startAngle + idleAngle;
            const idleLargeArc = idleAngle > Math.PI ? 1 : 0;
            const idlePathData = [
                `M 0,0`,
                `L ${radius * Math.cos(startAngle)},${radius * Math.sin(startAngle)}`,
                `A ${radius},${radius} 0 ${idleLargeArc},1 ${radius * Math.cos(idleEndAngle)},${radius * Math.sin(idleEndAngle)}`,
                `Z`
            ].join(' ');
            console.log('Idle path data:', idlePathData);
            const idlePath = document.createElementNS('http://www.w3.org/2000/svg', 'path');
            idlePath.setAttribute('id', 'pie-idle-path');
            idlePath.setAttribute('d', idlePathData);
            idlePath.classList.add('pie-idle');
            pieGroup.appendChild(idlePath);

            // Idle percentage label
            if (idlePercent > 5) {
                const idleLabelAngle = startAngle + idleAngle / 2;
                const idleLabelX = (radius * 0.7) * Math.cos(idleLabelAngle);
                const idleLabelY = (radius * 0.7) * Math.sin(idleLabelAngle);
                const idleLabel = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                idleLabel.setAttribute('x', idleLabelX);
                idleLabel.setAttribute('y', idleLabelY);
                idleLabel.setAttribute('text-anchor', 'middle');
                idleLabel.classList.add('pie-label');
                idleLabel.textContent = `${idlePercent.toFixed(1)}%`;
                pieGroup.appendChild(idleLabel);
            }
        }

        function updateChart(svgId, dataKey, formatTooltip, isBytes = false, isPercent = false) {
            const svgElement = document.getElementById(svgId);
            if (!svgElement) {
                console.warn(`SVG element ${svgId} not found.`);
                return;
            }

            initializeSvgWidth(svgElement);
            const actualSvgHeight = svgElement.clientHeight || defaultSvgHeight;
            const actualChartHeight = actualSvgHeight - padding.top - padding.bottom;
            if (actualChartHeight <= 0) {
                console.warn(`Chart height for ${svgId} is too small or negative.`);
                return;
            }

            const data = chartData.map(d => d[dataKey]).filter(v => typeof v === 'number' && !isNaN(v));
            const times = chartData.map(d => d.time instanceof Date ? d.time.getTime() : NaN).filter(t => !isNaN(t));

            if (data.length < 1 || times.length < 1) {
                console.warn(`No valid data for ${svgId}, data:`, data, 'times:', times);
                const path = svgElement.querySelector('.data-line');
                if (path) path.setAttribute('d', '');
                return;
            }

            const minValue = Math.min(...data);
            const maxValue = Math.max(...data, 0);
            const range = Math.max(maxValue - minValue, 1);
            let yMin, yMax, unit = 'bytes';

            if (isPercent) {
                yMin = Math.max(0, minValue - range * 0.5);
                yMax = Math.min(100, maxValue + range * 0.5);
                if (yMax - yMin < 10) {
                    const mid = (maxValue + minValue) / 2;
                    yMin = Math.max(0, mid - 5);
                    yMax = Math.min(100, mid + 5);
                }
            } else if (isBytes && maxValue >= 1024 * 1024) {
                unit = 'MiB';
                const minMiB = minValue / (1024 * 1024);
                const maxMiB = maxValue / (1024 * 1024);
                const rangeMiB = Math.max(maxMiB - minMiB, 0.1);
                yMin = Math.max(0, minMiB - rangeMiB * 0.2);
                yMax = maxMiB + rangeMiB * 0.2;
                const step = Math.pow(10, Math.floor(Math.log10(rangeMiB))) / 2;
                yMin = Math.floor(yMin / step) * step;
                yMax = Math.ceil(yMax / step) * step;
                if (yMax - yMin < step) yMax = yMin + step;
            } else {
                yMin = Math.max(0, minValue - range * 0.2);
                yMax = maxValue + range * 0.2;
                const step = Math.pow(10, Math.floor(Math.log10(range))) / 2;
                yMin = Math.floor(yMin / step) * step;
                yMax = Math.ceil(yMax / step) * step;
                if (yMax - yMin < step) yMax = yMin + step;
            }

            console.log(`Chart ${svgId} scaling: yMin=${yMin}, yMax=${yMax}, unit=${unit}, data=`, data);

            const timeMin = Math.min(...times);
            const timeMax = Math.max(...times, timeMin + 1000);

            const xScale = (time) => {
                if (timeMax === timeMin) return padding.left + chartWidth / 2;
                return padding.left + ((time - timeMin) / (timeMax - timeMin)) * chartWidth;
            };
            const yScale = (value) => {
                if (yMax === yMin) return actualSvgHeight - padding.bottom - actualChartHeight / 2;
                const scaledValue = unit === 'MiB' ? value / (1024 * 1024) : value;
                return actualSvgHeight - padding.bottom - ((scaledValue - yMin) / (yMax - yMin)) * actualChartHeight;
            };

            let pathData = '';
            for (let i = 0; i < data.length; i++) {
                const value = Math.max(0, data[i]);
                const time = times[i];
                const x = xScale(time);
                const y = yScale(value);
                if (i === 0) {
                    pathData += `M ${x.toFixed(2)},${y.toFixed(2)}`;
                } else {
                    const prevX = xScale(times[i - 1]);
                    const prevY = yScale(data[i - 1]);
                    pathData += ` H ${x.toFixed(2)} V ${y.toFixed(2)}`;
                }
            }

            const path = svgElement.querySelector('.data-line');
            if (path) {
                path.setAttribute('d', pathData);
            }

            const xAxisGroup = svgElement.querySelector('.x-axis');
            const yAxisGroup = svgElement.querySelector('.y-axis');
            if (!xAxisGroup || !yAxisGroup) return;

            while (xAxisGroup.childElementCount > 1) xAxisGroup.removeChild(xAxisGroup.lastChild);
            while (yAxisGroup.childElementCount > 1) yAxisGroup.removeChild(yAxisGroup.lastChild);

            const numTimeTicks = Math.min(Math.floor(chartWidth / 80), 5);
            if (chartWidth > 0 && numTimeTicks > 0) {
                for (let i = 0; i <= numTimeTicks; i++) {
                    const timeValue = timeMin + (i / numTimeTicks) * (timeMax - timeMin);
                    const time = new Date(timeValue);
                    const x = padding.left + (i / numTimeTicks) * chartWidth;
                    const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                    text.setAttribute('x', x);
                    text.setAttribute('y', actualSvgHeight - padding.bottom + 15);
                    text.setAttribute('text-anchor', 'middle');
                    text.classList.add('x-axis-labels');
                    text.textContent = time.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
                    xAxisGroup.appendChild(text);
                }
            }

            const numYTicks = 3;
            if (actualChartHeight > 0) {
                for (let i = 0; i <= numYTicks; i++) {
                    const value = yMin + (i / numYTicks) * (yMax - yMin);
                    const y = actualSvgHeight - padding.bottom - (i / numYTicks) * actualChartHeight;
                    const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                    text.setAttribute('x', padding.left - 8);
                    text.setAttribute('y', y + 3);
                    text.setAttribute('text-anchor', 'end');
                    text.classList.add('y-axis-labels');
                    text.textContent = isPercent ? `${Math.round(value)}%` :
                                       unit === 'MiB' ? `${value.toFixed(1)} MiB` :
                                       formatWithThousands(Math.round(value));
                    yAxisGroup.appendChild(text);
                }
            }
        }

        function initializeAllCharts() {
            setupAxes('totalBytesChart');
            setupAxes('cpuChart');
            setupAxes('ramChart');
            setupAxes('packetsReceivedChart');
            setupAxes('packetsSentChart');
            updateChart('totalBytesChart', 'totalBytes', (value) => `${formatWithThousands(value)} (${bytesToMiB(value)})`, true);
            updateChart('cpuChart', 'cpuPercent', (value) => `${Math.round(value)}%`, false, true);
            updateChart('ramChart', 'ramPercent', (value) => `${Math.round(value)}%`, false, true);
            updateChart('packetsReceivedChart', 'packetsReceivedPerMin', (value) => formatWithThousands(Math.round(value)), false);
            updateChart('packetsSentChart', 'packetsSentPerMin', (value) => formatWithThousands(Math.round(value)), false);
            const totalPages = Math.floor(latestStats.file_size / PAGE_SIZE);
            const pagesUsed = Math.min(latestStats.total_pages, totalPages);
            const pagesIdle = Math.max(0, totalPages - pagesUsed);
            updatePieChart(pagesUsed, pagesIdle);
            setupPieChartHover();
        }

        function setupPieChartHover() {
            const usedPath = document.getElementById('pie-used-path');
            const idlePath = document.getElementById('pie-idle-path');
            const legendItems = document.querySelectorAll('.legend-item');

            if (usedPath && idlePath) {
                usedPath.addEventListener('mouseover', () => {
                    usedPath.style.opacity = '1';
                    usedPath.style.stroke = '#ffffff';
                    usedPath.style.strokeWidth = '2';
                    idlePath.style.opacity = '0.7';
                    idlePath.style.stroke = 'none';
                });
                usedPath.addEventListener('mouseout', () => {
                    usedPath.style.opacity = '0.7';
                    usedPath.style.stroke = 'none';
                    idlePath.style.opacity = '0.7';
                    idlePath.style.stroke = 'none';
                });

                idlePath.addEventListener('mouseover', () => {
                    idlePath.style.opacity = '1';
                    idlePath.style.stroke = '#ffffff';
                    idlePath.style.strokeWidth = '2';
                    usedPath.style.opacity = '0.7';
                    usedPath.style.stroke = 'none';
                });
                idlePath.addEventListener('mouseout', () => {
                    idlePath.style.opacity = '0.7';
                    idlePath.style.stroke = 'none';
                    usedPath.style.opacity = '0.7';
                    usedPath.style.stroke = 'none';
                });
            }

            legendItems.forEach(item => {
                item.addEventListener('mouseover', () => {
                    const segmentId = item.getAttribute('data-segment') + '-path';
                    const segment = document.getElementById(segmentId);
                    const otherSegmentId = segmentId === 'pie-used-path' ? 'pie-idle-path' : 'pie-used-path';
                    const otherSegment = document.getElementById(otherSegmentId);
                    if (segment && otherSegment) {
                        segment.style.opacity = '1';
                        segment.style.stroke = '#ffffff';
                        segment.style.strokeWidth = '2';
                        otherSegment.style.opacity = '0.7';
                        otherSegment.style.stroke = 'none';
                    }
                });
                item.addEventListener('mouseout', () => {
                    const segmentId = item.getAttribute('data-segment') + '-path';
                    const segment = document.getElementById(segmentId);
                    const otherSegmentId = segmentId === 'pie-used-path' ? 'pie-idle-path' : 'pie-used-path';
                    const otherSegment = document.getElementById(otherSegmentId);
                    if (segment && otherSegment) {
                        segment.style.opacity = '0.7';
                        segment.style.stroke = 'none';
                        otherSegment.style.opacity = '0.7';
                        otherSegment.style.stroke = 'none';
                    }
                });
            });
        }

        async function updateDashboardData() {
            try {
                const response = await fetch('/stats');
                console.log('Fetch response status:', response.status);
                if (!response.ok) {
                    throw new Error(`Server responded with status ${response.status}`);
                }
                const stats = await response.json();
                console.log('Received stats:', JSON.stringify(stats, null, 2));

                if (!stats || typeof stats !== 'object') {
                    throw new Error('Invalid stats data received');
                }

                latestStats = {
                    uptime: Number(stats.uptime) || latestStats.uptime || 0,
                    total_pages: Number(stats.total_pages) || latestStats.total_pages || 0,
                    total_bytes: Number(stats.total_bytes) || latestStats.total_bytes || 0,
                    cpu_percent: Number(stats.cpu_percent) || latestStats.cpu_percent || 0,
                    ram_used: Number(stats.ram_used) || latestStats.ram_used || 0,
                    ram_total: Number(stats.ram_total) || latestStats.ram_total || 1,
                    packets_sent: Number(stats.packets_sent) || latestStats.packets_sent || 0,
                    packets_received: Number(stats.packets_received) || latestStats.packets_received || 0,
                    last_updated: Number(stats.last_updated) || latestStats.last_updated || Math.floor(Date.now() / 1000),
                    file_size: Number(stats.file_size) || latestStats.file_size || 0
                };
                console.log('Parsed latestStats:', latestStats);

                isServerConnected = true;
                const statusElement = document.getElementById('serverStatus');
                if (statusElement) {
                    statusElement.textContent = 'Running';
                    statusElement.classList.remove('disconnected', 'text-accent-red');
                    statusElement.classList.add('connected', 'text-accent-green');
                }

                const ramPercent = latestStats.ram_total > 0 ? (latestStats.ram_used / latestStats.ram_total) * 100 : 0;
                const packetsReceivedPerMin = latestStats.packets_received * 60;
                const packetsSentPerMin = latestStats.packets_sent * 60;
                const newDataPoint = {
                    time: new Date(),
                    totalBytes: latestStats.total_bytes,
                    cpuPercent: latestStats.cpu_percent,
                    ramPercent: ramPercent,
                    packetsReceivedPerMin: packetsReceivedPerMin,
                    packetsSentPerMin: packetsSentPerMin
                };
                console.log('New data point:', newDataPoint);
                chartData.push(newDataPoint);
                const fiveMinutesAgo = new Date().getTime() - 300000;
                chartData = chartData.filter(d => d.time.getTime() > fiveMinutesAgo);

                updateChart('totalBytesChart', 'totalBytes', (value) => `${formatWithThousands(value)} (${bytesToMiB(value)})`, true);
                updateChart('cpuChart', 'cpuPercent', (value) => `${Math.round(value)}%`, false, true);
                updateChart('ramChart', 'ramPercent', (value) => `${Math.round(value)}%`, false, true);
                updateChart('packetsReceivedChart', 'packetsReceivedPerMin', (value) => formatWithThousands(Math.round(value)), false);
                updateChart('packetsSentChart', 'packetsSentPerMin', (value) => formatWithThousands(Math.round(value)), false);

                const totalPages = Math.floor(latestStats.file_size / PAGE_SIZE);
                const pagesUsed = Math.min(latestStats.total_pages, totalPages);
                const pagesIdle = Math.max(0, totalPages - pagesUsed);
                updatePieChart(pagesUsed, pagesIdle);
                setupPieChartHover();

                const updates = {
                    uptime: formatUptime(latestStats.uptime),
                    totalBytes: `${formatWithThousands(latestStats.total_bytes)} (${bytesToMiB(latestStats.total_bytes)})`,
                    lastUpdated: formatTimestamp(latestStats.last_updated),
                    packetsReceived: formatWithThousands(packetsReceivedPerMin),
                    packetsSent: formatWithThousands(packetsSentPerMin),
                    cpuPercent: `${Math.round(latestStats.cpu_percent)}%`,
                    ramPercent: `${Math.round(ramPercent)}%`
                };
                console.log('DOM updates:', updates);

                document.getElementById('uptime').textContent = updates.uptime;
                document.getElementById('totalBytes').textContent = updates.totalBytes;
                document.getElementById('totalBytesValue').textContent = updates.totalBytes;
                document.getElementById('lastUpdated').textContent = updates.lastUpdated;
                document.getElementById('packetsReceived').textContent = updates.packetsReceived;
                document.getElementById('packetsSent').textContent = updates.packetsSent;
                document.getElementById('cpuPercent').textContent = updates.cpuPercent;
                document.getElementById('ramPercent').textContent = updates.ramPercent;
            } catch (error) {
                console.error('Error in updateDashboardData:', error);
                isServerConnected = false;
                const statusElement = document.getElementById('serverStatus');
                if (statusElement) {
                    statusElement.textContent = 'Disconnected';
                    statusElement.classList.remove('connected', 'text-accent-green');
                    statusElement.classList.add('disconnected', 'text-accent-red');
                }
            }
        }

        function updateUptimeDisplay() {
            if (isServerConnected && latestStats.uptime >= 0) {
                latestStats.uptime += 1;
                document.getElementById('uptime').textContent = formatUptime(latestStats.uptime);
            }
        }

        document.addEventListener('DOMContentLoaded', () => {
            console.log('DOM loaded, initializing charts');
            initializeAllCharts();
            updateDashboardData();
        });

        setInterval(updateDashboardData, 3000);
        setInterval(updateUptimeDisplay, 1000);

        window.addEventListener('resize', () => {
            initializeAllCharts();
        });
    </script>
</body>
</html>
"###;