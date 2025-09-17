use anyhow::Result;
use clap::Parser;
use log::{info, warn};
use pod::{
    client::{configure_client, set_default_client, start_persistent_stream_loop},
    gossip::{bootstrap_from_entrypoint_udp, start_udp_gossip, PeerList},
    logger::init_logger,
    server::start_server,
    rpc::start_rpc_server,
    stats::Stats,
    storage::StorageState,
};
use quinn::Endpoint;
use std::{net::SocketAddr, str::FromStr, sync::Arc};
use tokio::{
    signal,
    sync::{broadcast, Mutex, RwLock},
};

const ATLAS_IP: &str = "95.217.229.171:5000"; //Devnet
// const ATLAS_IP: &str = "65.108.233.175:5000"; // trynet
// const ATLAS_IP: &str = "127.0.0.1:5000";
const DEFAULT_BOOTSTRAP: &str = "161.97.97.41:9001"; // Default bootstrap node
const QUIC_PORT: u16 = 5000;

/// Xandeum Pod - High-performance blockchain node implementation
#[derive(Parser)]
#[command(name = "pod")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "High-performance blockchain node implementation")]
#[command(long_about = "Xandeum Pod is a high-performance blockchain node that provides JSON-RPC API, 
peer-to-peer communication via gossip protocol, and real-time statistics monitoring.

PORTS:
    6000    RPC API (configurable IP binding)
    80      Stats dashboard (localhost only)  
    9001    Gossip protocol (peer communication)
    5000    Atlas connection (outbound)

DOCUMENTATION:
    For complete documentation, visit /usr/share/doc/pod/ after installation")]
struct Args {
    /// Set RPC server IP binding [default: 127.0.0.1 for private]
    /// 
    /// Use 127.0.0.1 for private (localhost only)
    /// Use 0.0.0.0 for public (all interfaces)  
    /// Or specify custom IP address
    #[arg(long, default_value = "127.0.0.1", value_name = "IP_ADDRESS")]
    rpc_ip: String,

    /// Bootstrap node for peer discovery [default: 173.212.207.32:9001]
    #[arg(long, value_name = "IP:PORT")]
    entrypoint: Option<SocketAddr>,

    /// Disable peer discovery (run in isolation)
    #[arg(long)]
    no_entrypoint: bool,

    /// Atlas server address for data streaming [default: 95.217.229.171:5000]
    #[arg(long, value_name = "IP:PORT")]
    atlas_ip: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut entrypoint = args.entrypoint;
    let no_entrypoint = args.no_entrypoint;
    let rpc_ip = args.rpc_ip;
    let atlas_ip = args.atlas_ip;

    if no_entrypoint {
        println!("Running without entrypoint.");
    } else if let Some(addr) = entrypoint {
        println!("Using entrypoint: {}", addr);
    } else {
        // Use default bootstrap node when no entrypoint is specified
        match DEFAULT_BOOTSTRAP.parse() {
            Ok(addr) => {
                entrypoint = Some(addr);
                println!("Using default bootstrap node: {}", DEFAULT_BOOTSTRAP);
            }
            Err(e) => {
                eprintln!("Invalid default bootstrap address {}: {}", DEFAULT_BOOTSTRAP, e);
                std::process::exit(1);
            }
        }
    }

    let atlas_ip = atlas_ip.unwrap_or_else(|| ATLAS_IP.to_string());

    let _ = init_logger();

    // Setting Default client , No need to set it anywhere
    set_default_client()?;

    let client_config = configure_client()?;

    let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], QUIC_PORT)))?;
    endpoint.set_default_client_config(client_config);
    let addr = SocketAddr::from_str(&atlas_ip)?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

    let mut client_shutdown_rx = shutdown_tx.subscribe();

    let storage_state = StorageState::get_or_create_state().await?;

    let metadata = storage_state.metadata.clone();

    let stats = Arc::new(Mutex::new(Stats {
        cpu_percent: 0.0,
        ram_used: 0,
        ram_total: 0,
        uptime: 0,
        packets_received: 0,
        packets_sent: 0,
        active_streams:0
    }));

    let peer_list = Arc::new(RwLock::new(PeerList::new()));

    // Bootstrap from entrypoint using UDP (gracefully handle failures)
    match bootstrap_from_entrypoint_udp(entrypoint, peer_list.clone()).await {
        Ok(_) => info!("Bootstrap completed successfully"),
        Err(e) => {
            if entrypoint.is_some() {
                warn!("Bootstrap failed, continuing without initial peers: {}", e);
            }
        }
    }
    
    // Start UDP gossip service
    let _ = start_udp_gossip(peer_list.clone(), stats.clone()).await?;

    let stats_clone = stats.clone();
    let client_handle = tokio::spawn(async move {
        start_persistent_stream_loop(
            endpoint,
            addr,
            storage_state.clone(),
            &mut client_shutdown_rx,
            stats_clone,
        )
        .await;
    });

    let metadata_clone = metadata.clone();
    let stats_clone = stats.clone();
    let peer_list_clone = peer_list.clone();
    let server_handle = tokio::spawn(async move {
        let _ = start_server(metadata.clone(), stats.clone(), peer_list.clone()).await;
    });

    let rpc_handle = tokio::spawn(async move {
        let _ = start_rpc_server(metadata_clone, stats_clone, peer_list_clone, rpc_ip).await;
    });

    signal::ctrl_c().await?;

    let _ = shutdown_tx.send(());

    client_handle.abort();
    server_handle.abort();
    rpc_handle.abort();

    Ok(())
}
