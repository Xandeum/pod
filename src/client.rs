use anyhow::{Error, Result};
use log::{debug, error, info, warn};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use rustls_pemfile::certs;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

use crate::cert::AcceptAllVerifier;
use crate::packet::{reassemble_packets, Meta, Operation, Packet, MAX_PACKET_SIZE};
use crate::storage::{handle_poke, Metadata, StorageState, PAGE_SIZE};

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

    let (send, recv) = connection.accept_bi().await?;

    let receiver = Arc::new(Mutex::new(recv));
    let sender = Arc::new(Mutex::new(send));

    loop {
        match receive_packets(receiver.clone()).await {
            Ok(pack) => {
                if pack.meta.op == Operation::Peek {
                    info!("received Peek op ");
                    let _ = handle_peek(storage_state.clone(), sender.clone(), pack).await;
                } else {
                    handle_poke(storage_state.clone(), pack).await
                }
            }
            Err(e) => {
                error!("Failed to Receive packets : {:?}", e);
                continue;
            }
        }
    }
}


pub async fn handle_peek(
    storage_state: StorageState,
    sender: Arc<Mutex<SendStream>>,
    packet: Packet,
) {
    let page_id = packet.meta.page_no;
    let offset = packet.meta.offset;

    let indexes = storage_state.index.lock().await;
}

async fn receive_packets(receiver: Arc<Mutex<RecvStream>>) -> Result<Packet, Error> {
    let mut buffer = vec![0u8; MAX_PACKET_SIZE * 2];

    let mut packets_chunks = Vec::new();
    let mut expected_total_chunks = 1;
    // let mut expected_page_no = request_packet.meta.page_no;
    // let mut expected_file_id = request_packet.meta.file_id;
    let mut recv = receiver.lock().await;
    loop {
        match recv.read(&mut buffer).await {
            Ok(Some(n)) => {
                buffer.truncate(n);

                let packet: Packet = bincode::deserialize(&buffer)?;

                if packets_chunks.is_empty() {
                    expected_total_chunks = packet.meta.total_chunks;
                }

                packets_chunks.push(packet);

                if packets_chunks.len() as u32 == expected_total_chunks {
                    break;
                }
            }
            Ok(None) => {
                warn!("Stream closed by sender.");
            }
            Err(e) => {
                error!("Failed to Receive from client Due to : {} ", e);
                return Err(e.into());
            }
        };
    }

    let reassembled_packet = reassemble_packets(packets_chunks).await;

    Ok(reassembled_packet)
}

pub async fn try_connect(endpoint: Endpoint, addr: SocketAddr) -> Result<Connection> {
    const RETRY_INTERVAL: Duration = Duration::from_secs(10);

    loop {
        info!("addr : {:?}",addr);
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
