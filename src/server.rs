use anyhow::Result;
use axum::{extract::State, response::Html, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use sysinfo::System;
use tokio::sync::Mutex;

use crate::{
    stats::{update_system_stats, AppState, CombinedStats, Stats},
    storage::Metadata,
};

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
    let metadata = state.meta.lock().await;
    let stats = state.stats.lock().await;
    Json(CombinedStats {
        metadata: metadata.clone(),
        stats: stats.clone(),
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

    let packets_received_per_min = stats.packets_received * 60; // Initial rate
    let packets_sent_per_min = stats.packets_sent * 60; // Initial rate
    let uptime_formatted = format_uptime(stats.uptime);

    let html = STATS_TEMPLATE
        .replace("{{current_index}}", &format_with_thousands(metadata.current_index))
        .replace("{{total_pages}}", &format_with_thousands(metadata.total_pages))
        .replace("{{last_updated}}", &format_timestamp(metadata.last_updated))
        .replace("{{total_bytes}}", &total_bytes_formatted)
        .replace("{{packets_received}}", &format_with_thousands(packets_received_per_min))
        .replace("{{packets_sent}}", &format_with_thousands(packets_sent_per_min))
        .replace("{{uptime}}", &uptime_formatted);
    
    Html(html)
}

pub async fn start_server(meta: Arc<Mutex<Metadata>>, stats: Arc<Mutex<Stats>>) -> Result<()> {
    let stats_clone = stats.clone();
    tokio::spawn(async move {
        update_system_stats(stats_clone).await;
    });

    let app_state = AppState { meta, stats };

    let app = Router::new()
        .route("/", get(root))
        .route("/stats", get(get_stats))
        .route("/stats-page", get(get_stats_page))
        .with_state(app_state);

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
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Pod Stats</title>
    <!-- Tailwind CSS CDN -->
    <script src="https://cdn.tailwindcss.com"></script>
    <style>
        html, body {
            margin: 0;
            padding: 0;
            height: 100%;
            width: 100%;
            overflow-x: hidden;
            overflow-y: auto;
            background: linear-gradient(135deg, #1a1a1a 0%, #2a2a2a 100%);
            color: #e0e0e0;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, sans-serif;
        }
        * {
            box-sizing: border-box;
        }
        .container {
            display: flex;
            flex-direction: column;
            min-height: 100vh;
            width: 100%;
            max-width: 100vw;
            padding: 0.25rem;
        }
        .header {
            background: #2a2a2a;
            padding: 0.25rem 0.5rem;
            border-bottom: 1px solid #3a3a3a;
            width: 100%;
        }
        .header h1 {
            font-size: 1.5rem;
            color: #ffffff;
        }
        .header .status {
            font-size: 0.875rem;
        }
        .status.connected {
            color: #55BA83;
        }
        .status.disconnected {
            color: #D94343;
        }
        .main-content {
            flex: 1;
            width: 100%;
        }
        .stats-row {
            display: grid;
            grid-template-columns: repeat(4, 1fr);
            gap: 0.25rem;
            margin-bottom: 0.25rem;
            width: 100%;
        }
        .stat-tile {
            background: #2a2a2a;
            border-radius: 0.5rem;
            padding: 0.5rem;
            box-shadow: 0 4px 6px rgba(0,0,0,0.3), 0 0 10px rgba(255,255,255,0.05);
            text-align: center;
            transition: transform 0.2s ease;
            width: 100%;
        }
        .stat-tile:hover {
            transform: translateY(-2px);
            box-shadow: 0 6px 8px rgba(0,0,0,0.4), 0 0 15px rgba(255,255,255,0.1);
        }
        .stat-tile h3 {
            margin: 0 0 0.3rem 0;
            font-size: 0.875rem;
            color: #a0a0a0;
        }
        .stat-tile .value {
            font-size: 1rem;
            color: #ffffff;
            font-family: monospace;
        }
        .charts-container {
            margin-bottom: 0.25rem;
            width: 100%;
        }
        .chart-container {
            background: #2a2a2a;
            box-shadow: 0 4px 6px rgba(0,0,0,0.3);
            border-radius: 0.5rem;
            padding: 0.5rem;
            width: 100%;
        }
        .chart-container h3 {
            margin: 0 0 0.5rem 0;
            font-size: 0.875rem;
            color: #a0a0a0;
        }
        .last-updated {
            text-align: center;
            margin: 0.25rem 0;
            font-size: 0.875rem;
            color: #a0a0a0;
            width: 100%;
        }
        .footer {
            display: grid;
            grid-template-columns: repeat(4, 1fr);
            gap: 0.25rem;
            padding: 0.25rem 0;
            border-top: 1px solid #3a3a3a;
            width: 100%;
        }
        .footer-item {
            background: #2a2a2a;
            border-radius: 0.5rem;
            box-shadow: 0 4px 6px rgba(0,0,0,0.3), 0 0 10px rgba(255,255,255,0.05);
            padding: 0.5rem;
            text-align: center;
            transition: transform 0.2s ease;
            width: 100%;
        }
        .footer-item:hover {
            transform: translateY(-2px);
            box-shadow: 0 6px 8px rgba(0,0,0,0.4), 0 0 15px rgba(255,255,255,0.1);
        }
        .footer-item h3 {
            margin: 0 0 0.5rem 0;
            font-size: 0.875rem;
            color: #a0a0a0;
        }
        .footer-item .value {
            font-size: 1rem;
            color: #ffffff;
            font-family: monospace;
        }
        svg.chart-svg {
            width: 100%;
            height: 120px;
            background: transparent;
        }
        .x-axis, .y-axis {
            stroke: #3a3a3a;
            stroke-width: 1;
        }
        .x-axis-labels, .y-axis-labels {
            fill: #a0a0a0;
            font-size: 8px;
        }
        .data-line {
            fill: none;
            stroke-width: 2;
            transition: d 0.5s ease;
        }
        .data-line.cpu {
            stroke: #D94343;
        }
        .data-line.ram {
            stroke: #FFD700;
        }
        .data-line.totalBytes {
            stroke: url(#gradientTotalBytes);
        }
        @media (max-width: 900px) {
            .stats-row, .footer {
                grid-template-columns: 1fr;
            }
            .stat-tile, .footer-item {
                width: 100%;
            }
        }
    </style>
</head>
<body>
    <div class="container">
        <div class="header flex justify-between items-center">
            <h1>Pod Stats</h1>
            <div class="status connected" id="serverStatus">Running</div>
        </div>
        <div class="main-content">
            <div class="stats-row">
                <div class="stat-tile">
                    <h3>Uptime</h3>
                    <div class="value" id="uptime">{{uptime}}</div>
                </div>
                <div class="stat-tile">
                    <h3>Current Index</h3>
                    <div class="value" id="currentIndex">{{current_index}}</div>
                </div>
                <div class="stat-tile">
                    <h3>Total Pages</h3>
                    <div class="value" id="totalPages">{{total_pages}}</div>
                </div>
                <div class="stat-tile">
                    <h3>Total Bytes</h3>
                    <div class="value" id="totalBytes">{{total_bytes}}</div>
                </div>
            </div>
            <div class="charts-container">
                <div class="chart-container">
                    <h3>Total Bytes</h3>
                    <svg id="totalBytesChart" class="chart-svg">
                        <defs>
                            <linearGradient id="gradientTotalBytes" x1="0%" y1="0%" x2="100%" y2="0%">
                                <stop offset="0%" style="stop-color:#55BA83;stop-opacity:1" />
                                <stop offset="100%" style="stop-color:#D94343;stop-opacity:1" />
                            </linearGradient>
                        </defs>
                        <g class="x-axis"></g>
                        <g class="y-axis"></g>
                        <polyline class="data-line totalBytes" points="" />
                    </svg>
                </div>
            </div>
        </div>
        <div class="last-updated">Last Updated: <span id="lastUpdated">{{last_updated}}</span></div>
        <div class="footer">
            <div class="footer-item chart-container">
                <h3>CPU Usage (%)</h3>
                <svg id="cpuChart" class="chart-svg">
                    <g class="x-axis"></g>
                    <g class="y-axis"></g>
                    <polyline class="data-line cpu" points="" />
                </svg>
            </div>
            <div class="footer-item chart-container">
                <h3>RAM Usage (%)</h3>
                <svg id="ramChart" class="chart-svg">
                    <g class="x-axis"></g>
                    <g class="y-axis"></g>
                    <polyline class="data-line ram" points="" />
                </svg>
            </div>
            <div class="footer-item">
                <h3>Packets Received/Min</h3>
                <div class="value" id="packetsReceived">{{packets_received}}</div>
            </div>
            <div class="footer-item">
                <h3>Packets Sent/Min</h3>
                <div class="value" id="packetsSent">{{packets_sent}}</div>
            </div>
        </div>
    </div>

    <script>
        // Formatting functions
        function formatWithThousands(n) {
            return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
        }
        function bytesToMiB(bytes) {
            const mib = bytes / (1024 * 1024);
            return `${mib.toFixed(2)} MiB`;
        }
        function formatTimestamp(timestamp) {
            const date = new Date(timestamp * 1000);
            return date.toUTCString().replace(' GMT', ' UTC');
        }
        function formatUptime(seconds) {
            const days = Math.floor(seconds / (24 * 3600));
            seconds %= (24 * 3600);
            const hours = Math.floor(seconds / 3600);
            seconds %= 3600;
            const minutes = Math.floor(seconds / 60);
            seconds = seconds % 60;

            let result = '';
            if (days > 0) result += `${days}d `;
            if (hours > 0 || days > 0) result += `${hours}h `;
            if (minutes > 0 || hours > 0 || days > 0) result += `${minutes}m `;
            result += `${seconds}s`;
            return result.trim();
        }

        // Parse initial formatted strings
        const initialCurrentIndex = parseInt("{{current_index}}".replace(/,/g, ''), 10);
        const initialTotalPages = parseInt("{{total_pages}}".replace(/,/g, ''), 10);
        const initialTotalBytesStr = "{{total_bytes}}".split(' ')[0];
        const initialTotalBytes = parseInt(initialTotalBytesStr.replace(/,/g, ''), 10);

        // Initialize chart data
        let chartData = [
            {
                time: new Date(),
                currentIndex: initialCurrentIndex,
                totalPages: initialTotalPages,
                totalBytes: initialTotalBytes,
                cpuPercent: 0,
                ramPercent: 0
            }
        ];

        // Cache for the latest stats
        let latestStats = {
            uptime: 0,
            current_index: initialCurrentIndex,
            total_pages: initialTotalPages,
            total_bytes: initialTotalBytes,
            cpu_percent: 0,
            ram_used: 0,
            ram_total: 1,
            packets_sent: 0,
            packets_received: 0
        };

        // SVG Chart Setup
        let svgWidth = 0;
        const svgHeight = 120;
        const padding = { top: 5, right: 5, bottom: 20, left: 40 };
        let chartWidth = 0;
        const chartHeight = svgHeight - padding.top - padding.bottom;

        function initializeSvgWidth() {
            const container = document.querySelector('.chart-container');
            if (container) {
                svgWidth = container.clientWidth;
                chartWidth = svgWidth - padding.left - padding.right;
            } else {
                svgWidth = 600;
                chartWidth = svgWidth - padding.left - padding.right;
            }
        }

        function setupAxes(svgId) {
            const svgElement = document.getElementById(svgId);
            svgElement.setAttribute('width', svgWidth);
            svgElement.setAttribute('height', svgHeight);

            const xAxisGroup = svgElement.querySelector('.x-axis');
            const yAxisGroup = svgElement.querySelector('.y-axis');

            while (xAxisGroup.firstChild) xAxisGroup.removeChild(xAxisGroup.firstChild);
            while (yAxisGroup.firstChild) yAxisGroup.removeChild(yAxisGroup.firstChild);

            const xAxisLine = document.createElementNS('http://www.w3.org/2000/svg', 'line');
            xAxisLine.setAttribute('x1', padding.left);
            xAxisLine.setAttribute('y1', svgHeight - padding.bottom);
            xAxisLine.setAttribute('x2', svgWidth - padding.right);
            xAxisLine.setAttribute('y2', svgHeight - padding.bottom);
            xAxisLine.classList.add('x-axis');
            xAxisGroup.appendChild(xAxisLine);

            const yAxisLine = document.createElementNS('http://www.w3.org/2000/svg', 'line');
            yAxisLine.setAttribute('x1', padding.left);
            yAxisLine.setAttribute('y1', padding.top);
            yAxisLine.setAttribute('x2', padding.left);
            yAxisLine.setAttribute('y2', svgHeight - padding.bottom);
            yAxisLine.classList.add('y-axis');
            yAxisGroup.appendChild(yAxisLine);
        }

        function updateChart(svgId, dataKey, formatTooltip, isBytes = false, isPercent = false) {
            const svgElement = document.getElementById(svgId);
            const data = chartData.map(d => d[dataKey]).filter(v => !isNaN(v));
            const times = chartData.map(d => d.time.getTime()).filter(t => !isNaN(t));

            if (data.length < 1 || times.length < 1) {
                console.warn(`No valid data to render for ${svgId}`);
                return;
            }

            const minValue = isPercent ? 0 : Math.min(...data);
            const maxValue = isPercent ? 100 : Math.max(...data);
            const range = maxValue - minValue || 1;
            const yMin = isPercent ? 0 : Math.max(0, minValue - range * 0.1);
            const yMax = isPercent ? 100 : maxValue + range * 0.1;

            const timeMin = Math.min(...times);
            const timeMax = Math.max(...times) || timeMin + 1;

            const xScale = (time) => {
                if (timeMax === timeMin) return padding.left + chartWidth / 2;
                return padding.left + ((time - timeMin) / (timeMax - timeMin)) * chartWidth;
            };
            const yScale = (value) => {
                if (yMax === yMin) return svgHeight - padding.bottom - chartHeight / 2;
                return svgHeight - padding.bottom - ((value - yMin) / (yMax - yMin)) * chartHeight;
            };

            // Create ECG-style points (P wave, QRS complex, T wave)
            const points = [];
            for (let i = 0; i < data.length; i++) {
                const value = Math.max(0, data[i]); // Ensure no negative values
                const time = times[i];
                const x = xScale(time);
                const baseline = yScale(value);

                if (i < data.length - 1) {
                    const nextTime = times[i + 1];
                    const nextX = xScale(nextTime);
                    const timeDiff = (nextTime - time) / 6; // Divide into segments for ECG pattern
                    const pWaveHeight = value + (isBytes ? value * 0.05 : 5); // Small P wave
                    const qrsHeight = value + (isBytes ? value * 0.2 : 20); // Tall QRS peak
                    const tWaveHeight = value + (isBytes ? value * 0.1 : 10); // Smaller T wave

                    const pWaveX = x + (timeDiff * chartWidth / (timeMax - timeMin));
                    const qrsX = x + (timeDiff * 3 * chartWidth / (timeMax - timeMin));
                    const tWaveX = x + (timeDiff * 5 * chartWidth / (timeMax - timeMin));

                    // ECG waveform: baseline -> P -> baseline -> QRS -> baseline -> T -> baseline
                    points.push(`${x},${baseline}`); // Start at baseline
                    points.push(`${pWaveX},${yScale(pWaveHeight)}`); // P wave
                    points.push(`${pWaveX + (timeDiff * chartWidth / (timeMax - timeMin))},${baseline}`); // Back to baseline
                    points.push(`${qrsX},${yScale(qrsHeight)}`); // QRS peak
                    points.push(`${qrsX + (timeDiff * chartWidth / (timeMax - timeMin))},${baseline}`); // Back to baseline
                    points.push(`${tWaveX},${yScale(tWaveHeight)}`); // T wave
                    points.push(`${nextX},${baseline}`); // End at baseline
                } else {
                    points.push(`${x},${baseline}`);
                }
            }

            const polyline = svgElement.querySelector('.data-line');
            if (points.length) {
                polyline.setAttribute('points', points.join(' '));
            } else {
                console.warn(`No points to render for ${svgId}`);
            }

            const xAxisGroup = svgElement.querySelector('.x-axis');
            const yAxisGroup = svgElement.querySelector('.y-axis');

            while (xAxisGroup.childElementCount > 1) xAxisGroup.removeChild(xAxisGroup.lastChild);
            while (yAxisGroup.childElementCount > 1) yAxisGroup.removeChild(yAxisGroup.lastChild);

            const numTicks = 5;
            for (let i = 0; i <= numTicks; i++) {
                const timeValue = timeMin + (i / numTicks) * (timeMax - timeMin);
                const time = new Date(timeValue);
                const x = padding.left + (i / numTicks) * chartWidth;
                const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                text.setAttribute('x', x);
                text.setAttribute('y', svgHeight - padding.bottom + 12);
                text.setAttribute('text-anchor', 'middle');
                text.classList.add('x-axis-labels');
                text.textContent = time.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
                xAxisGroup.appendChild(text);
            }

            for (let i = 0; i <= 3; i++) {
                const value = yMin + (i / 3) * (yMax - yMin);
                const y = svgHeight - padding.bottom - (i / 3) * chartHeight;
                const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
                text.setAttribute('x', padding.left - 5);
                text.setAttribute('y', y + 3);
                text.setAttribute('text-anchor', 'end');
                text.classList.add('y-axis-labels');
                text.textContent = isBytes ? `${formatWithThousands(Math.round(value))} (${bytesToMiB(Math.round(value))})` :
                                    isPercent ? `${Math.round(value)}%` :
                                    formatWithThousands(Math.round(value));
                yAxisGroup.appendChild(text);
            }
        }

        let isServerConnected = true;

        // Initialize charts
        initializeSvgWidth();
        setupAxes('totalBytesChart');
        setupAxes('cpuChart');
        setupAxes('ramChart');
        updateChart('totalBytesChart', 'totalBytes', (value) => `${formatWithThousands(value)} (${bytesToMiB(value)})`, true);
        updateChart('cpuChart', 'cpuPercent', (value) => `${Math.round(value)}%`, false, true);
        updateChart('ramChart', 'ramPercent', (value) => `${Math.round(value)}%`, false, true);

        async function updateCharts() {
            try {
                console.log('Fetching stats from /stats...');
                const response = await fetch('/stats');
                console.log('Response status:', response.status);
                if (!response.ok) {
                    throw new Error(`Server responded with status ${response.status}: ${response.statusText}`);
                }
                const stats = await response.json();
                console.log('Received stats:', stats);

                // Validate stats object
                if (!stats || typeof stats !== 'object') {
                    throw new Error('Invalid stats data received: not an object');
                }

                // Update the cached stats
                latestStats = stats;

                isServerConnected = true;
                const statusElement = document.getElementById('serverStatus');
                statusElement.textContent = 'Running';
                statusElement.classList.remove('disconnected');
                statusElement.classList.add('connected');

                const ramPercent = (stats.ram_used || 0) / (stats.ram_total || 1) * 100;
                const newData = {
                    time: new Date(),
                    currentIndex: stats.current_index,
                    totalPages: stats.total_pages,
                    totalBytes: stats.total_bytes,
                    cpuPercent: stats.cpu_percent || 0,
                    ramPercent: ramPercent
                };
                chartData.push(newData);
                chartData = chartData.filter(d => new Date() - d.time < 300000);

                updateChart('totalBytesChart', 'totalBytes', (value) => `${formatWithThousands(value)} (${bytesToMiB(value)})`, true);
                updateChart('cpuChart', 'cpuPercent', (value) => `${Math.round(value)}%`, false, true);
                updateChart('ramChart', 'ramPercent', (value) => `${Math.round(value)}%`, false, true);

                document.getElementById('currentIndex').textContent = formatWithThousands(stats.current_index);
                document.getElementById('totalPages').textContent = formatWithThousands(stats.total_pages);
                document.getElementById('totalBytes').textContent = `${formatWithThousands(stats.total_bytes)} (${bytesToMiB(stats.total_bytes)})`;
                document.getElementById('lastUpdated').textContent = formatTimestamp(stats.last_updated);

                const packetsReceivedPerMin = (stats.packets_received || 0) * 60;
                const packetsSentPerMin = (stats.packets_sent || 0) * 60;
                document.getElementById('packetsReceived').textContent = formatWithThousands(packetsReceivedPerMin);
                document.getElementById('packetsSent').textContent = formatWithThousands(packetsSentPerMin);
            } catch (error) {
                console.error('Error fetching stats:', error.message);
                console.error('Error stack:', error.stack);
                isServerConnected = false;
                const statusElement = document.getElementById('serverStatus');
                statusElement.textContent = 'Disconnected';
                statusElement.classList.remove('connected');
                statusElement.classList.add('disconnected');
            }
        }

        function updateUptime() {
            const uptime = (latestStats.uptime || 0) + 1;
            latestStats.uptime = uptime;
            document.getElementById('uptime').textContent = formatUptime(uptime);
        }

        setInterval(updateCharts, 3000);
        setInterval(updateUptime, 1000);

        window.addEventListener('resize', () => {
            initializeSvgWidth();
            setupAxes('totalBytesChart');
            setupAxes('cpuChart');
            setupAxes('ramChart');
            updateChart('totalBytesChart', 'totalBytes', (value) => `${formatWithThousands(value)} (${bytesToMiB(value)})`, true);
            updateChart('cpuChart', 'cpuPercent', (value) => `${Math.round(value)}%`, false, true);
            updateChart('ramChart', 'ramPercent', (value) => `${Math.round(value)}%`, false, true);
        });
    </script>
</body>
</html>
"###;