# CLI Usage

Complete reference for all command-line arguments and configuration options for Xandeum Pod.

## Basic Usage

### Start with Required Keypair
```bash
pod --keypair key.json
```

Starts the pod with default configuration:
- RPC server on `127.0.0.1:6000` (private)
- Stats server on `127.0.0.1:80` (private) 
- Uses default bootstrap node `173.212.207.32:9001` for peer discovery
- Connects to Atlas server at `65.108.233.175:5000`

### Check Version
```bash
pod --version
# Output: pod 0.4.2
```

### Get Help
```bash
pod --help
# Shows complete usage information with all options and examples
```

!!! example "Help Output"
    ```
    Xandeum Pod is a high-performance blockchain node that provides JSON-RPC API, 
    peer-to-peer communication via gossip protocol, and real-time statistics monitoring.

    PORTS:
        6000    RPC API (HTTP/TCP, configurable IP binding)
        80      Stats dashboard (HTTP/TCP, localhost only)  
        9001    Gossip protocol (UDP, public, peer communication)
        5000    Atlas connection (QUIC/UDP, public, outbound)

    DOCUMENTATION:
        For complete documentation, visit https://xandeum.github.io/pod-docs/ after installation
    ```

## Command-Line Arguments

### --keypair `<PATH>`
*Required (unless using --gossip)* - Path to Solana keypair file in JSON format.

This keypair is used for:
- Signing heartbeat packets sent to Atlas server
- Authenticating the pod in the network
- Identity verification with other nodes

=== "Example"

    ```bash
    # Use existing keypair
    pod --keypair ./key.json
    
    # Generate new keypair first
    solana-keygen new -o my-pod-key.json
    pod --keypair my-pod-key.json
    ```

!!! warning "Keypair Security"
    Keep your keypair file secure. It identifies your pod in the network. Never share it publicly.

### --rpc-ip `<IP_ADDRESS>`
*Optional* - Specifies the IP address for the RPC server to bind to.

**Default**: `127.0.0.1` (private, localhost only)

=== "Examples"

    ```bash
    # Private access (default)
    pod --rpc-ip 127.0.0.1

    # Public access from any interface
    pod --rpc-ip 0.0.0.0

    # Bind to specific network interface
    pod --rpc-ip 192.168.1.100

    # IPv6 localhost
    pod --rpc-ip ::1
    ```

!!! warning "Security Note"
    Using `0.0.0.0` makes your RPC API accessible from any network interface. Only use this if you understand the security implications.

### --entrypoint `<IP:PORT>`
*Optional* - Specifies a bootstrap node to connect to for initial peer discovery.

**Default**: `173.212.207.32:9001` (default bootstrap node)

=== "Examples"

    ```bash
    # Connect to specific bootstrap node
    pod --entrypoint 192.168.1.50:9001

    # Connect to custom port
    pod --entrypoint 10.0.0.5:9002
    ```

### --no-entrypoint
*Optional* - Disables bootstrap peer discovery. The pod will start without attempting to connect to any initial peers.

=== "Example"

    ```bash
    # Start without peer discovery
    pod --no-entrypoint
    ```

!!! info "Isolated Mode"
    When using `--no-entrypoint`, your pod will operate in isolation until other pods connect to it directly.

### --atlas-ip `<IP:PORT>`
*Optional* - Specifies the Atlas server address for data streaming and synchronization.

**Default**: `65.108.233.175:5000` (Trynet)

=== "Examples"

    ```bash
    # Connect to local Atlas server
    pod --keypair key.json --atlas-ip 127.0.0.1:5000

    # Connect to custom Atlas server
    pod --keypair key.json --atlas-ip 10.0.0.10:5000
    ```

!!! info "Atlas Connection"
    The pod maintains 2 persistent QUIC streams with Atlas:
    - **Heartbeat stream**: For keep-alive and identity verification
    - **Data stream**: For blockchain data operations (peek/poke/bigbang/etc.)

### --gossip
*Standalone* - Queries a running pod's RPC endpoint to display the gossip network and exits.

This is useful for monitoring the peer-to-peer network without running a full pod.

=== "Example"

    ```bash
    # View gossip network from default local pod
    pod --gossip
    
    # View gossip network from remote pod
    pod --gossip --rpc-ip 161.97.97.41
    ```

**Output includes:**
- Total peer count
- Each peer's address, version, last seen time, and public key

### --version
*Standalone* - Displays the pod software version and exits immediately.

```bash
pod --version
# Output: pod 0.4.2
```

### --help
*Standalone* - Displays complete usage information including all options, examples, and port information.

```bash
pod --help
# Shows complete usage information with all options and examples
```

## Common Usage Patterns

### 🔒 Private Development Setup
```bash
pod --keypair key.json --no-entrypoint
```
Perfect for local development and testing. No external connections, RPC only accessible locally.

### 🌐 Public Node
```bash
pod --keypair key.json --rpc-ip 0.0.0.0
```
Runs a public node with RPC API accessible from any network interface. Uses default bootstrap for peer discovery.

### 🏢 Enterprise/Private Network
```bash
pod --keypair key.json --rpc-ip 192.168.1.100 --entrypoint 192.168.1.50:9001 --atlas-ip 192.168.1.10:5000
```
Configured for private corporate networks with custom Atlas server and internal bootstrap node.

### 🧪 Local Testing with Custom Atlas
```bash
pod --keypair key.json --atlas-ip 127.0.0.1:5000 --no-entrypoint
```
For testing with a local Atlas server without peer discovery.

### 👀 Monitor Gossip Network
```bash
pod --gossip --rpc-ip 161.97.97.41
```
View the peer-to-peer network topology from any running pod without starting your own.

## Port Information

| Service | Default Port | Configurable | Description |
|---------|-------------|-------------|-------------|
| RPC API | 6000 | IP Only | JSON-RPC API endpoint |
| Stats Dashboard | 80 | ❌ Fixed | Web-based statistics dashboard (localhost only) |
| Gossip Protocol | 9001 | ❌ Fixed | Peer-to-peer communication and bootstrap discovery |
| Atlas Connection | 5000 | ❌ Fixed | Connection to Atlas server for data streaming |

### Firewall Configuration

For public nodes, ensure these ports are accessible:

- **Port 6000**: RPC API (if using `--rpc-ip 0.0.0.0`)
- **Port 9001**: Gossip protocol (always required for peer communication and discovery)
- **Port 5000**: Atlas connection (outbound to Atlas server)

## Error Handling

### Invalid IP Address
```bash
pod --rpc-ip invalid-ip
# Error: Invalid IP address 'invalid-ip': invalid IP address syntax
```

### IP Address Not Available
```bash
pod --rpc-ip 192.168.1.200
# Error: Cannot bind to IP address 192.168.1.200 on port 6000: Address not available.
# Make sure the IP address is available on this system.
```

### Missing Argument Value
```bash
pod --rpc-ip
# error: a value is required for '--rpc-ip <IP_ADDRESS>' but none was supplied
# 
# For more information, try '--help'.
```

### Unknown Argument
```bash
pod --invalid-option
# error: unexpected argument '--invalid-option' found
# 
# Usage: pod [OPTIONS]
# 
# For more information, try '--help'.
```

## Configuration Examples

### Development Environment
```bash
# Start isolated pod for development
pod --keypair key.json --no-entrypoint

# Test RPC locally
curl -X POST http://127.0.0.1:6000/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"get-version","id":1}'
```

### Production Public Node
```bash
# Start public node with proper logging
RUST_LOG=info pod --keypair key.json --rpc-ip 0.0.0.0

# Verify RPC is accessible
curl -X POST http://YOUR_PUBLIC_IP:6000/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"get-version","id":1}'
```

### Private Network Setup
```bash
# Configure for private network
pod \
  --keypair key.json \
  --rpc-ip 10.0.1.100 \
  --entrypoint 10.0.1.50:9001 \
  --atlas-ip 10.0.1.10:5000
```

### Monitor Network Gossip
```bash
# View gossip peers from a running pod
pod --gossip --rpc-ip 161.97.97.41

# Example output:
# 🔍 Querying gossip network from 161.97.97.41...
# 
# ╔══════════════════════════════════════════════════════════════════╗
# ║              XANDEUM POD GOSSIP NETWORK                          ║
# ╚══════════════════════════════════════════════════════════════════╝
# 
# 📊 Total Peers: 5
# 
# ADDRESS              VERSION    LAST SEEN       PUBKEY
# ────────────────────────────────────────────────────────────────────
# 192.168.1.10:9001    0.4.2      2m ago         5QJ7K...Gx2p9
# 10.0.0.5:9001        0.4.1      5m ago         8HmN...k5L1a
```

## Environment Variables

You can also configure logging and other runtime behavior:

```bash
# Set log level
export RUST_LOG=debug
pod

# Enable specific module logging
export RUST_LOG=pod::rpc=debug,pod::gossip=info
pod
```

## Systemd Service

For production deployments, the pod can be managed as a systemd service. When installed via apt, the service file is automatically configured.

```ini
[Unit]
Description=Xandeum Pod System service
After=network.target

[Service]
ExecStart=/usr/bin/pod --rpc-ip 0.0.0.0
Restart=always
User=pod
Environment=NODE_ENV=production
Environment=RUST_LOG=info
StandardOutput=syslog
StandardError=syslog
SyslogIdentifier=xandeum-pod

[Install]
WantedBy=multi-user.target
```

```bash
# Enable and start the service
sudo systemctl enable pod
sudo systemctl start pod

# Check status
sudo systemctl status pod

# View logs
sudo journalctl -u pod -f
```

## Troubleshooting

### Port Already in Use
```bash
# Check what's using port 6000
sudo lsof -i :6000

# Kill process using the port
sudo kill -9 <PID>
```

### Network Interface Issues
```bash
# List available network interfaces
ip addr show

# Test if IP is accessible
ping <your-ip>
```

### Peer Discovery Problems
```bash
# Test connectivity to bootstrap node
nc -u 173.212.207.32 9001

# Check local gossip port
sudo netstat -ulnp | grep 9001
```

!!! tip "Logging"
    Use `RUST_LOG=debug` to get detailed logs for troubleshooting network and configuration issues. 