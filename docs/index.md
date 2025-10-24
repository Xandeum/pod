# ![Xandeum Logo](assets/images/XandeumLogoStandard.png "Xandeum Logo"){ width="150" } Xandeum pNode Documentation

Welcome to the documentation for Xandeum pNode - a high-performance blockchain node implementation.

## Quick Start

### Installation

!!! info "Prerequisites"
    - A Linux server or VPS running Ubuntu/Debian
    - SSH access to your server
    - Basic command line knowledge

**Step 1: Access Your Server**

If you're using a VPS (Virtual Private Server), connect via SSH:
```bash
ssh username@your-server-ip
```

Once connected to your server, open a terminal window.

**Step 2: Add the Xandeum Repository**

First, install the required packages and add the Xandeum repository:
```bash
# Install repository prerequisites
sudo apt-get install -y apt-transport-https ca-certificates

# Add the Xandeum repository
echo "deb [trusted=yes] https://xandeum.github.io/pod-apt-package/ stable main" | sudo tee /etc/apt/sources.list.d/xandeum-pod.list

# Update package list
sudo apt-get update
```

**Step 3: Install the pNode**
```bash
# Install the pod package
sudo apt-get install pod
```

!!! tip "Verify Installation"
    After installation completes, you can verify it was successful by checking the version:
    ```bash
    pod --version
    ```

### Basic Usage
```bash
# Start with default settings (private pRPC)
pod

# Start with public pRPC access
pod --rpc-ip 0.0.0.0

# Check version
pod --version

# Get help
pod --help
```

### Test Your Setup

Verify your pNode is running correctly:

```bash
curl -X POST http://127.0.0.1:6000/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "get-version",
    "id": 1
  }'
```

!!! success "Expected Response"
    If everything is working, you should see a JSON response with the version number:
    ```json
    {
      "jsonrpc": "2.0",
      "result": {
        "version": "0.4.2"
      },
      "id": 1
    }
    ```

## What's Included

### 🔌 pRPC API
Complete JSON-RPC 2.0 API for interacting with your pnode:
- **get-version**: Get pnode software version
- **get-stats**: Retrieve comprehensive pnode statistics
- **get-pods**: List known peer pnodes in the network

[View pRPC API Documentation](rpc-api.md){ .md-button .md-button--primary }

### ⚙️ CLI Usage
Comprehensive command-line reference:
- **--rpc-ip**: Configure pRPC server IP binding
- **--entrypoint**: Set bootstrap node for peer discovery
- **--atlas-ip**: Configure Atlas server connection
- And more...

[View CLI Documentation](cli.md){ .md-button .md-button--primary }

## Architecture Overview

The Xandeum pNode consists of several key components:

- **pRPC Server**: JSON-RPC API on port 6000 (configurable IP)
- **Stats Dashboard**: Web interface on port 80 (localhost only)
- **Gossip Protocol**: Peer-to-peer communication on port 9001
- **Atlas Client**: Data streaming connection on port 5000

## Default Configuration

| Service | Port | Access | Configurable |
|---------|------|--------|-------------|
| pRPC API | 6000 | Private (127.0.0.1) | IP only |
| Stats Dashboard | 80 | Private (127.0.0.1) | No |
| Gossip Protocol | 9001 | All interfaces | No |
| Atlas Connection | 5000 | Fixed endpoint | No |

!!! tip "Security by Default"
    The pnode is configured to be secure by default - pRPC API is private unless explicitly configured otherwise.