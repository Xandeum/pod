# Xandeum Pod Documentation

Welcome to the complete documentation for Xandeum Pod - a high-performance blockchain node implementation.

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

### 🔌 RPC API
Complete JSON-RPC 2.0 API for interacting with your pod:
- **get-version**: Get pod software version
- **get-stats**: Retrieve comprehensive pod statistics
- **get-pods**: List known peer pods in the network

[View RPC API Documentation](rpc-api.md){ .md-button .md-button--primary }

### ⚙️ CLI Usage
Comprehensive command-line reference:
- **--rpc-ip**: Configure RPC server IP binding
- **--entrypoint**: Set bootstrap node for peer discovery
- **--atlas-ip**: Configure Atlas server connection
- And more...

[View CLI Documentation](cli.md){ .md-button .md-button--primary }

## Architecture Overview

The Xandeum Pod consists of several key components:

- **RPC Server**: JSON-RPC API on port 6000 (configurable IP)
- **Stats Dashboard**: Web interface on port 80 (localhost only)
- **Gossip Protocol**: Peer-to-peer communication on port 9001
- **Atlas Client**: Data streaming connection on port 5000

## Default Configuration

| Service | Port | Access | Configurable |
|---------|------|--------|-------------|
| RPC API | 6000 | Private (127.0.0.1) | IP only |
| Stats Dashboard | 80 | Private (127.0.0.1) | No |
| Gossip Protocol | 9001 | All interfaces | No |
| Atlas Connection | 5000 | Fixed endpoint | No |

!!! tip "Security by Default"
    The pod is configured to be secure by default - RPC API is private unless explicitly configured otherwise.