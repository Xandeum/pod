# Xandeum Pod Documentation

**Version:** 0.4.2

## 🚀 High-Performance Blockchain Node

Complete documentation for Xandeum Pod - a high-performance blockchain node with QUIC-based data streaming, UDP gossip protocol for peer discovery, JSON-RPC API, and real-time monitoring dashboard.

## Quick Start

### Installation
```bash
# Build from source
cargo build --release

# Install to system
cargo install --path .
```

### Prerequisites
You need a Solana keypair file to run the pod. Generate one if you don't have it:

```bash
# Generate a new keypair
solana-keygen new -o key.json
```

### Basic Usage
```bash
# Start with required keypair (private RPC)
pod --keypair key.json

# Start with public RPC access
pod --keypair key.json --rpc-ip 0.0.0.0

# Check version
pod --version

# Get help
pod --help

# View gossip network from a running pod
pod --gossip --rpc-ip <pod-ip>
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

### 🔌 RPC API (JSON-RPC 2.0)
Complete JSON-RPC 2.0 API for interacting with your pod:

- **get-version**: Get pod software version
- **get-stats**: Retrieve comprehensive pod statistics including CPU, RAM, uptime, and packet metrics
- **get-pods**: List known peer pods in the network with version and public keys

[View RPC API Documentation](rpc-api.md)

### ⚙️ CLI Usage
Comprehensive command-line reference:

- **--keypair**: Path to Solana keypair file (required)
- **--rpc-ip**: Configure RPC server IP binding (default: 127.0.0.1)
- **--entrypoint**: Set bootstrap node for peer discovery
- **--atlas-ip**: Configure Atlas server connection
- **--gossip**: Display network gossip peers and exit
- **--no-entrypoint**: Run in isolation without peer discovery

[View CLI Documentation](cli.md)

## Architecture Overview

The **Xandeum Pod** consists of several key components:

| Component | Description | Protocol |
|-----------|-------------|----------|
| **Atlas Client** | Persistent QUIC streams for data operations with heartbeat mechanism | QUIC/UDP |
| **RPC Server** | JSON-RPC 2.0 API endpoint on port 6000 | HTTP/TCP |
| **Gossip Service** | UDP-based peer discovery and network gossip | UDP |
| **Stats Server** | Real-time monitoring dashboard on port 80 | HTTP/TCP |
| **Storage Layer** | Page-based storage with global catalog and filesystem management | Local |
| **Keypair Auth** | Solana keypair-based signing for secure operations | Ed25519 |

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

### 🔒 **Security & Authentication**
- Solana keypair-based authentication
- Signed heartbeat packets for identity verification
- Private RPC by default (localhost only)
- TLS certificate verification for QUIC connections

### ⚡ **High Performance**
- Persistent QUIC streams for low-latency data operations
- Server-initiated heartbeat mechanism keeps connections alive
- UDP gossip protocol for efficient peer discovery
- 2 managed streams: heartbeat + data operations
- Page-based storage (1 MiB pages)

### 🌐 **Peer-to-Peer Network**
- Automatic peer discovery via UDP gossip (port 9001)
- Bootstrap node support with fallback to default
- Message deduplication to prevent gossip loops
- Periodic peer pruning (inactive > 1 hour)
- Version tracking and public key sharing

### 📊 **Monitoring & Observability**
- Web-based stats dashboard (localhost:80)
- Real-time CPU, RAM, uptime metrics
- Packet send/receive counters
- Active stream monitoring
- File storage size tracking

---

**Powered by Xandeum** - Building the future of blockchain infrastructure