# RPC API Reference

Complete reference for all JSON-RPC 2.0 methods available in Xandeum Pod.

## Overview

The Xandeum Pod RPC API uses JSON-RPC 2.0 protocol over HTTP POST requests. All requests should be sent to the `/rpc` endpoint.

!!! info "Base URL"
    `http://<pod-ip>:6000/rpc`
    
    **Default**: `http://127.0.0.1:6000/rpc` (private)

## Network Architecture

The pod uses several network ports for different services:

- **Port 6000**: RPC API server (configurable IP binding)
- **Port 80**: Statistics dashboard (localhost only) 
- **Port 9001**: Gossip protocol for peer discovery and communication
- **Port 5000**: Atlas server connection for data streaming (fixed endpoint)

## Available Methods

=== "get-version"

    Returns the current version of the pod software.

    ### Request
    ```json
    {
      "jsonrpc": "2.0",
      "method": "get-version",
      "id": 1
    }
    ```

    ### Response
    ```json
    {
      "jsonrpc": "2.0",
      "result": {
        "version": "0.4.2"
      },
      "id": 1
    }
    ```

    ### cURL Example
    ```bash
    curl -X POST http://127.0.0.1:6000/rpc \
      -H "Content-Type: application/json" \
      -d '{
        "jsonrpc": "2.0",
        "method": "get-version",
        "id": 1
      }'
    ```

=== "get-stats"

    Returns comprehensive statistics about the pod including system metrics, storage info, and network activity.

    ### Request
    ```json
    {
      "jsonrpc": "2.0",
      "method": "get-stats",
      "id": 1
    }
    ```

    ### Response
    ```json
    {
      "jsonrpc": "2.0",
      "result": {
        "metadata": {
          "total_bytes": 1048576000,
          "total_pages": 1000,
          "last_updated": 1672531200
        },
        "stats": {
          "cpu_percent": 15.5,
          "ram_used": 536870912,
          "ram_total": 8589934592,
          "uptime": 86400,
          "packets_received": 1250,
          "packets_sent": 980,
          "active_streams": 5
        },
        "file_size": 1048576000
      },
      "id": 1
    }
    ```

    ### Response Fields

    | Field | Type | Description |
    |-------|------|-------------|
    | `metadata.total_bytes` | number | Total bytes processed by the pod |
    | `metadata.total_pages` | number | Total pages in storage (each page is 1 MiB) |
    | `metadata.current_index` | number | Current storage index position |
    | `metadata.last_updated` | number | Unix timestamp of last metadata update |
    | `stats.cpu_percent` | number | Current CPU usage percentage (0-100) |
    | `stats.ram_used` | number | RAM used in bytes |
    | `stats.ram_total` | number | Total RAM available in bytes |
    | `stats.uptime` | number | Pod uptime in seconds |
    | `stats.packets_received` | number | Total packets received (QUIC + gossip) |
    | `stats.packets_sent` | number | Total packets sent (QUIC + gossip) |
    | `stats.active_streams` | number | Number of active QUIC streams (should be 2: heartbeat + data) |
    | `file_size` | number | Storage file size in bytes (xandeum-pod file) |

=== "get-pods"

    Returns a list of all known peer pods in the network with their status information.

    ### Request
    ```json
    {
      "jsonrpc": "2.0",
      "method": "get-pods",
      "id": 1
    }
    ```

    ### Response
    ```json
    {
      "jsonrpc": "2.0",
      "result": {
        "pods": [
          {
            "address": "192.168.1.100:9001",
            "version": "1.0.0",
            "last_seen": "2023-12-01 14:30:00 UTC",
            "last_seen_timestamp": 1672574200
          },
          {
            "address": "10.0.0.5:9001",
            "version": "1.0.1",
            "last_seen": "2023-12-01 14:25:00 UTC",
            "last_seen_timestamp": 1672573900
          }
        ],
        "total_count": 2
      },
      "id": 1
    }
    ```

    ### Pod Fields

    | Field | Type | Description |
    |-------|------|-------------|
    | `address` | string | IP address and port of the peer pod (gossip port 9001) |
    | `version` | string | Software version of the peer pod (e.g., "0.4.2") |
    | `last_seen_timestamp` | number | Unix timestamp of last gossip message received |
    | `pubkey` | string (optional) | Solana public key of the peer pod for identity verification |
    | `total_count` | number | Total number of known pods in the gossip network |

    !!! note "Peer Discovery"
        Peers are discovered via UDP gossip protocol on port 9001. Inactive peers (not seen for > 1 hour) are automatically pruned.

## Error Handling

All errors follow the JSON-RPC 2.0 specification and include standard error codes.

### Method Not Found
```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32601,
    "message": "Method not found"
  },
  "id": 1
}
```

### Internal Error
```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32603,
    "message": "Internal error"
  },
  "id": 1
}
```

### Standard Error Codes

| Code | Message | Description |
|------|---------|-------------|
| -32601 | Method not found | The requested method does not exist |
| -32603 | Internal error | Server encountered an internal error |

## Integration Examples

### Python Example
```python
import requests
import json

def call_rpc(method, params=None):
    payload = {
        "jsonrpc": "2.0",
        "method": method,
        "id": 1
    }
    if params:
        payload["params"] = params
    
    response = requests.post(
        "http://127.0.0.1:6000/rpc",
        json=payload,
        headers={"Content-Type": "application/json"}
    )
    return response.json()

# Get version
version = call_rpc("get-version")
print(f"Pod version: {version['result']['version']}")

# Get stats
stats = call_rpc("get-stats")
print(f"CPU usage: {stats['result']['cpu_percent']}%")
print(f"Active streams: {stats['result']['active_streams']}")
print(f"Packets sent: {stats['result']['packets_sent']}")

# Get known peers
peers = call_rpc("get-pods")
print(f"Total peers: {peers['result']['total_count']}")
for pod in peers['result']['pods']:
    print(f"  - {pod['address']} (version: {pod['version']})")
```

### JavaScript/Node.js Example
```javascript
const fetch = require('node-fetch');

async function callRPC(method, params = null) {
  const payload = {
    jsonrpc: "2.0",
    method: method,
    id: 1
  };
  if (params) payload.params = params;

  const response = await fetch('http://127.0.0.1:6000/rpc', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload)
  });
  
  return await response.json();
}

// Usage
(async () => {
  const version = await callRPC('get-version');
  console.log(`Pod version: ${version.result.version}`);
  
  const stats = await callRPC('get-stats');
  console.log(`Uptime: ${stats.result.uptime} seconds`);
  console.log(`Active QUIC streams: ${stats.result.active_streams}`);
  
  const peers = await callRPC('get-pods');
  console.log(`Known peers: ${peers.result.total_count}`);
  peers.result.pods.forEach(pod => {
    console.log(`  - ${pod.address} (v${pod.version})`);
  });
})();
```

### Bash/curl Example
```bash
#!/bin/bash

RPC_URL="http://127.0.0.1:6000/rpc"

# Function to call RPC
call_rpc() {
  local method=$1
  curl -s -X POST "$RPC_URL" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"id\":1}"
}

# Get version
echo "Getting version..."
call_rpc "get-version" | jq '.result.version'

# Get stats
echo "Getting stats..."
call_rpc "get-stats" | jq '.result.stats.cpu_percent'
```

!!! tip "Build from Source"
    Build the pod from source: `cargo build --release`

!!! info "Keypair Required"
    The pod requires a Solana keypair to run: `pod --keypair key.json`

!!! tip "Rate Limiting"
    There are currently no rate limits on the RPC API, but be mindful of resource usage when making frequent requests.

!!! warning "Security"
    When using `--rpc-ip 0.0.0.0`, your RPC API will be accessible from any network interface. Ensure proper firewall rules are in place.

!!! note "Monitoring Dashboard"
    In addition to the RPC API, a web-based stats dashboard is available at `http://localhost:80` with real-time charts and metrics. 