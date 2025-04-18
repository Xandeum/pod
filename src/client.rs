use anyhow::Result;
use log::{debug, error, info, warn};
use quinn::{ClientConfig, Connection, Endpoint};
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use rustls_pemfile::certs;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::cert::AcceptAllVerifier;
use crate::storage::StorageState;

pub async fn start_stream_loop(
    endpoint: Endpoint,
    addr: SocketAddr,
    storage_state: StorageState,
    shutdown_rx: &mut mpsc::Receiver<()>,
) {
    const RETRY_INTERVAL: u64 = 5;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::info!("Received shutdown signal, stopping stream loop");
                return ;
            }
            result = connect_and_handle_stream(endpoint.clone(), addr, storage_state.clone()) => {
                match result {
                    Ok(()) => {
                        log::info!("Stream loop completed successfully");
                        break;
                    }
                    Err(e) => {
                        log::error!("Stream error: {}, retrying in {} seconds", e, RETRY_INTERVAL);
                        tokio::time::sleep(Duration::from_secs(RETRY_INTERVAL)).await;
                    }
                }
            }
        }
    }
}

pub async fn connect_and_handle_stream(
    endpoint: Endpoint,
    addr: SocketAddr,
    storage_state: StorageState,
) -> Result<()> {
    let connection = try_connect(endpoint, addr).await?;

    let (mut _send, mut recv) = connection.accept_bi().await?;

    let mut buffer = Vec::new();

    loop {
        let mut buf = vec![0u8; 1048576];
        match recv.read(&mut buf).await {
            Ok(None) => {
                warn!("Stream closed by sender.");
                return Ok(());
            }
            Ok(Some(n)) => {
                buffer.extend_from_slice(&buf[..n]);
                info!("Received {} bytes, buffer size: {}", n, buffer.len());

                while buffer.len() >= 1048576 {
                    let page = buffer.drain(..1048576).collect::<Vec<u8>>();
                    storage_state.write_page(&page).await?;
                }
            }
            Err(e) => {
                error!("Error reading from stream: {}", e);
                return Err(e.into());
            }
        }
    }
}

pub async fn try_connect(endpoint: Endpoint, addr: SocketAddr) -> Result<Connection> {
    const RETRY_INTERVAL: Duration = Duration::from_secs(10);

    loop {
        match endpoint.connect(addr, "atlas")?.await {
            Ok(connection) => {
                info!("Connected to {}", addr);
                return Ok(connection);
            }
            Err(e) => {
                warn!("Connection Attempt failed : {}", e);
            }
        }

        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

pub fn configure_client() -> Result<ClientConfig> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    let cert_file = File::open("server.crt")?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'_>> =
        certs(&mut cert_reader).collect::<Result<_, std::io::Error>>()?;

    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots.add(cert)?;
    }

    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_config
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptAllVerifier));

    Ok(ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    )))
}
