use anyhow::Result;
use log::info;
use pod::{
    client::{
        configure_client, configure_gossip_client, configure_server, set_default_client,
        start_stream_loop,
    },
    gossip::{self, bootstrap_from_entrypoint, start_gossip, PeerList},
    logger::init_logger,
    protos::{DirectoryEntry, Inode, MkDirPayload},
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

// const ATLAS_IP: &str = "65.108.233.175:5000";
const ATLAS_IP: &str = "65.108.233.175:6000"; // trynet
                                              // const ATLAS_IP: &str = "127.0.0.1:1800";

#[tokio::main]
async fn main() -> Result<()> {
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--version".to_string()) {
        println!("pod {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let mut entrypoint: Option<SocketAddr> = None;
    let mut no_entrypoint = false;

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
        eprintln!("--entrypoint <ip:port> must be provided.");
        std::process::exit(1);
    }

    let _ = init_logger();

    // Setting Default client , No need to set it anywhere
    set_default_client()?;

    let client_config = configure_client()?;

    let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 1825)))?;

    endpoint.set_default_client_config(client_config);
    let addr = SocketAddr::from_str(ATLAS_IP)?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

    let mut client_shutdown_rx = shutdown_tx.subscribe();

    let storage_state = StorageState::get_or_create_state().await?;
    // let _ = storage_state
    //     .bootstrap_dummy_filesystem(0, "127.0.0.1:18268")
    //     .await?;
    let metadata = storage_state.metadata.clone();

    let stats = Arc::new(Mutex::new(Stats {
        cpu_percent: 0.0,
        ram_used: 0,
        ram_total: 0,
        uptime: 0,
        packets_received: 0,
        packets_sent: 0,
    }));

    let peer_list = Arc::new(RwLock::new(PeerList { list: vec![] }));

    let _ = bootstrap_from_entrypoint(entrypoint, peer_list.clone(), endpoint.clone()).await?;
    let _ = start_gossip(entrypoint, peer_list, stats.clone()).await?;

    let stats_clone = stats.clone();
    let client_handle = tokio::spawn(async move {
        start_stream_loop(
            endpoint,
            addr,
            storage_state.clone(),
            &mut client_shutdown_rx,
            stats_clone,
        )
        .await;
    });

    // let payload = MkDirPayload {
    //     new_inode: Some(Inode::new(1, "system".to_string(), true, false)),
    //     directory_entery: Some(DirectoryEntry {
    //         name: "test_dir".to_string(),
    //         inode_no: 1,
    //     }),
    //     parent_inode: Some(Inode::new(1, "system".to_string(), true, false)),
    //     directory_entery_parent: Some(DirectoryEntry {
    //         name: "test_dir".to_string(),
    //         inode_no: 1,
    //     }),
    //     xentires_inode: None,
    //     xentry_mapping: None,
    //     pod_mapping_inode: None,
    //     pods_mapping: None,
    // };
    // st.handle_mkdir(payload).await.unwrap();

    let server_handle = tokio::spawn(async move {
        let _ = start_server(metadata, stats.clone()).await;
    });
    signal::ctrl_c().await?;

    let _ = shutdown_tx.send(());

    client_handle.abort();
    server_handle.abort();

    Ok(())
}
