use anyhow::Result;
use pod::{
    client::{configure_client, start_persistent_stream_loop},
    logger::init_logger,
    server::start_server,
    stats::Stats,
    storage::StorageState,
};
use quinn::Endpoint;
use std::{net::SocketAddr, str::FromStr, sync::Arc};
use tokio::{
    signal,
    sync::{broadcast, Mutex},
};

const ATLAS_IP: &str = "95.217.229.171:5000"; //Devnet
// const ATLAS_IP: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && args[1] == "--version" {
        println!("pod {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let _ = init_logger();
    log::info!("Pod v{} starting - target: {}", env!("CARGO_PKG_VERSION"), ATLAS_IP);

    let client_config = configure_client()?;

    let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 8400)))?;

    endpoint.set_default_client_config(client_config);
    let addr = SocketAddr::from_str(ATLAS_IP)?;

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
        active_streams: 0,
    }));

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
        let _ = start_server(metadata, stats.clone()).await;
    });
    signal::ctrl_c().await?;

    let _ = shutdown_tx.send(());

    client_handle.abort();
    server_handle.abort();

    Ok(())
}
