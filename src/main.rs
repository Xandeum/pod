use anyhow::Result;
use pod::{
    client::{configure_client, start_stream_loop},
    logger::init_logger,
    server::start_server, storage::StorageState,
};
use quinn::Endpoint;
use std::{net::SocketAddr, str::FromStr};
use tokio::{signal, sync::mpsc};

const ATLAS_IP: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() -> Result<()> {
    let _ = init_logger();

    let client_config = configure_client()?;

    let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    endpoint.set_default_client_config(client_config);
    let addr = SocketAddr::from_str(ATLAS_IP)?;


    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);


    let storage_state = StorageState::get_or_create_state().await?;
    let metadata = storage_state.metadata.clone();

    let client_handle = tokio::spawn(async move {
        start_stream_loop(endpoint, addr,storage_state.clone(),&mut shutdown_rx).await;
    });

    let server_handle = tokio::spawn(async move {
        let _ = start_server(metadata).await;
    });

    signal::ctrl_c().await?;

    shutdown_tx.send(()).await.unwrap_or(());

    client_handle.abort();
    server_handle.abort();

    Ok(())
}
