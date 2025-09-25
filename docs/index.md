# <span class="xandeum-gradient">Xandeum Pod</span> Documentation <span class="version-badge">v1.0.0</span>

<div class="hero-section">
  <h1>🚀 High-Performance Blockchain Node</h1>
  <p>Complete documentation for Xandeum Pod - a cutting-edge blockchain node implementation featuring JSON-RPC API, peer-to-peer communication, and real-time monitoring.</p>
</div>

## Quick Start

### Installation
```bash
# Install via apt package manager
sudo apt update
sudo apt install pod
```

### Basic Usage
```bash
# Start with default settings (private RPC)
pod

# Start with public RPC access
pod --rpc-ip 0.0.0.0

# Check version
pod --version

# Get help
pod --help
```

### Test Your Setup
```bash
curl -X POST http://127.0.0.1:6000/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "get-version",
    "id": 1
  }'
```

## What's Included

### 🔌 **RPC API** <span class="xandeum-badge">JSON-RPC 2.0</span>
Complete JSON-RPC 2.0 API for interacting with your pod:

- **get-version**: Get pod software version
- **get-stats**: Retrieve comprehensive pod statistics  
- **get-pods**: List known peer pods in the network

[View RPC API Documentation](rpc-api.md){ .md-button .md-button--primary }

### ⚙️ **CLI Usage** <span class="xandeum-badge">Command Line</span>
Comprehensive command-line reference:

- **--rpc-ip**: Configure RPC server IP binding
- **--entrypoint**: Set bootstrap node for peer discovery
- **--atlas-ip**: Configure Atlas server connection
- And more...

[View CLI Documentation](cli.md){ .md-button .md-button--primary }

## Architecture Overview

The **Xandeum Pod** consists of several key components working in harmony:

| Component | Description | Status |
|-----------|-------------|--------|
| **RPC Server** | JSON-RPC API endpoint | <span class="status-online">●</span> Active |
| **Stats Dashboard** | Real-time monitoring interface | <span class="status-online">●</span> Active |
| **Gossip Protocol** | Peer-to-peer communication | <span class="status-online">●</span> Active |
| **Atlas Client** | Data streaming connection | <span class="status-online">●</span> Active |

## Network Configuration

| Service | Port | Protocol | Access | Configurable |
|---------|------|----------|--------|-------------|
| **RPC API** | `6000` | HTTP/TCP | Private (127.0.0.1) | IP binding only |
| **Stats Dashboard** | `80` | HTTP/TCP | Private (127.0.0.1) | No |
| **Gossip Protocol** | `9001` | UDP | Public (0.0.0.0) | No |
| **Atlas Connection** | `5000` | QUIC/UDP | Public (outbound) | Atlas IP only |

!!! tip "Security by Default"
    The pod is configured to be secure by default - RPC API is private unless explicitly configured otherwise. Use `--rpc-ip 0.0.0.0` for public access.

!!! info "Performance Optimized"
    Xandeum Pod utilizes modern protocols like QUIC for high-performance data streaming and UDP for efficient peer discovery.

## Key Features

### 🔒 **Security First**
- Private RPC by default
- Certificate-based QUIC connections
- Secure peer authentication

### ⚡ **High Performance**
- QUIC/UDP for Atlas streaming
- Efficient UDP gossip protocol
- Real-time statistics monitoring

### 🌐 **Network Ready**
- Automatic peer discovery
- Bootstrap node support
- Configurable network binding

### 📊 **Monitoring Built-in**
- Live stats dashboard
- Performance metrics
- Network topology visibility

---

<div style="text-align: center; margin-top: 2rem; opacity: 0.8;">
  <strong>Powered by <span class="xandeum-gradient">Xandeum</span></strong><br>
  <small>Building the future of blockchain infrastructure</small>
</div>