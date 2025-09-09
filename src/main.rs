use anyhow::Result;
use log::{info, warn};
use pod::{
    client::{configure_client, set_default_client, start_persistent_stream_loop},
    gossip::{bootstrap_from_entrypoint_udp, start_udp_gossip, PeerList, GOSSIP_PORT},
    logger::init_logger,
    server::start_server,
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
const DEFAULT_BOOTSTRAP: &str = "173.212.207.32:9001"; // Default bootstrap node

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--version".to_string()) {
        println!("pod {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let mut entrypoint: Option<SocketAddr> = None;
    let mut no_entrypoint = false;
    let mut atlas_ip: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--entrypoint" => {
                if i + 1 >= args.len() {
                    eprintln!("Expected argument after --entrypoint");
                    std::process::exit(1);
                }
                entrypoint = Some(args[i + 1].parse()?);
                i += 1;
            }
            "--no-entrypoint" => {
                no_entrypoint = true;
            }
            "--atlas-ip" => {
                if i + 1 >= args.len() {
                    eprintln!("Expected argument after --atlas-ip");
                    std::process::exit(1);
                }
                atlas_ip = Some(args[i + 1].clone());
                i += 1;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

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

    let atlas_ip = match atlas_ip {
        Some(ip) => ip,
        None => ATLAS_IP.to_string(),
    };

    let _ = init_logger();

    // Setting Default client , No need to set it anywhere
    set_default_client()?;

    let client_config = configure_client()?;

    let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 5000)))?;
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

    let server_handle = tokio::spawn(async move {
        let _ = start_server(metadata, stats.clone(), peer_list.clone()).await;
    });
    signal::ctrl_c().await?;

    let _ = shutdown_tx.send(());

    client_handle.abort();
    server_handle.abort();

    Ok(())
}
