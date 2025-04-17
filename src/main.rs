use anyhow::Result;
use pod::{
    client::{configure_client, start_stream_loop},
    logger::init_logger,
    server::start_server,
};
use quinn::Endpoint;
use std::{net::SocketAddr, str::FromStr};
use tokio::signal;

const ATLAS_IP: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() -> Result<()> {
    let _ = init_logger();

    let client_config = configure_client()?;

    let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    endpoint.set_default_client_config(client_config);
    let addr = SocketAddr::from_str(ATLAS_IP)?;

    let client_handle = tokio::spawn(async move {
        start_stream_loop(endpoint, addr).await;
    });

    let server_handle = tokio::spawn(async move {
        let _ = start_server().await;
    });

    signal::ctrl_c().await?;

    client_handle.abort();
    server_handle.abort();

    Ok(())
}
