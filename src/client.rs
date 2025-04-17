use anyhow::Result;
use log::{error, info, warn};
use quinn::{ClientConfig, Connection, Endpoint};
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use rustls_pemfile::certs;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use crate::cert::AcceptAllVerifier;

pub async fn start_stream_loop(endpoint: Endpoint, addr: SocketAddr) {
    info!("Starting stream ");

    loop {
        if let Err(e) = connect_and_handle_stream(endpoint.clone(), addr).await {
            error!("Stream error : {:?} , retrying in 5 seconds", e);
            sleep(Duration::from_secs(5));
        }
    }
}

pub async fn connect_and_handle_stream(endpoint: Endpoint, addr: SocketAddr) -> Result<()> {
    let connection = try_connect(endpoint, addr).await?;

    let (mut _send, mut recv) = connection.accept_bi().await?;

    loop {
        let mut buf = vec![0u8; 4096];
        match recv.read(&mut buf).await {
            Ok(None) => {
                warn!("Stream closed by sender.");
                return Ok(());
            }
            Ok(Some(n)) => {
                let message = String::from_utf8_lossy(&buf[..n]);
                info!("Received: {}", message);
            }
            Err(e) => {
                error!("Error reading from stream: {}", e);
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
