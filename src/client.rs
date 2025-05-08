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
use tokio::sync::{broadcast, Mutex};

use crate::cert::AcceptAllVerifier;
use crate::packet::{reassemble_packets, split_packet, Operation, Packet, MAX_PACKET_SIZE};
use crate::stats::Stats;
use crate::storage::StorageState;

pub async fn start_stream_loop(
    endpoint: Endpoint,
    addr: SocketAddr,
    storage_state: StorageState,
    shutdown_rx: &mut broadcast::Receiver<()>,
    stats: Arc<Mutex<Stats>>,
) {
    const RETRY_INTERVAL: u64 = 5;

    let mut rx2 = shutdown_rx.resubscribe();
    loop {
        tokio::select! {
            Ok(_) = rx2.recv() => {
                info!("Shutdown received in start_stream_loop");
                return ();
            }
            result = connect_and_handle_stream(endpoint.clone(), addr, storage_state.clone(),shutdown_rx,stats.clone()) => {
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
    shutdown_rx: &mut broadcast::Receiver<()>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let connection = try_connect(endpoint, addr).await?;

    info!("Connected to server, waiting for streams");

    loop {
        let stats_clone = stats.clone();
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                info!("Accepted A Bi Stream");
                let sender = Arc::new(tokio::sync::Mutex::new(send));
                let receiver = Arc::new(tokio::sync::Mutex::new(recv));
                let storage_clone = storage_state.clone();

                tokio::spawn(async move {
                    if let Err(e) =
                        handle_stream(storage_clone, sender, receiver, stats_clone).await
                    {
                        error!("Error handling stream: {:?}", e);
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept bidirectional stream: {:?}", e);
                return Err(e.into());
            }
        }
    }
}

async fn handle_stream(
    storage_state: StorageState,
    sender: Arc<tokio::sync::Mutex<SendStream>>,
    receiver: Arc<tokio::sync::Mutex<RecvStream>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    // Receive packets
    let packet = receive_packets(receiver, stats.clone()).await?;
    info!("Received packet: {:?}", packet);

    // Handle based on operation
    match packet.meta.op {
        Operation::Peek => {
            // Handle peek and send response
            let _ = storage_state
                .handle_peek(sender.clone(), packet, stats.clone())
                .await;
        }
        Operation::Poke => {
            storage_state.handle_poke(packet).await;
        }
    }

    Ok(())
}

async fn receive_packets(
    receiver: Arc<Mutex<RecvStream>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<Packet, Error> {
    let mut buffer = vec![0u8; MAX_PACKET_SIZE];

    let mut packets_chunks = Vec::new();
    let mut expected_total_chunks = 1;

    // let mut expected_page_no = request_packet.meta.page_no;
    // let mut expected_file_id = request_packet.meta.file_id;
    let mut recv = receiver.lock().await;
    loop {
        let mut buffer = vec![0u8; MAX_PACKET_SIZE];

        match recv.read_exact(&mut buffer).await {
            Ok(()) => {
                let packet: Packet = bincode::deserialize(&buffer).unwrap();

                if packets_chunks.is_empty() {
                    expected_total_chunks = packet.meta.total_chunks;
                }

                packets_chunks.push(packet);

                if packets_chunks.len() as u32 == expected_total_chunks {
                    break;
                }
            }
            Err(e) => {
                error!("Failed to Receive from client Due to : {} ", e);
                return Err(e.into());
            }
        };
    }

    let mut stat = stats.lock().await;
    stat.packets_received += packets_chunks.len() as u64;

    drop(stat);

    let reassembled_packet = reassemble_packets(packets_chunks).await;
    debug!("reassembled packet : {:?}", reassembled_packet);

    Ok(reassembled_packet)
}

pub async fn send_packets(
    sender_stream: Arc<tokio::sync::Mutex<SendStream>>,
    packet: Packet,
    stats: Arc<Mutex<Stats>>,
) -> Result<(), Error> {
    let mut sender = sender_stream.lock().await;
    let packet_chunks = split_packet(packet);
    let chunks_len = packet_chunks.len();
    info!("Sending  : {} Packets", chunks_len);

    for pkt in packet_chunks {
        let s = bincode::serialize(&pkt).unwrap();
        if let Err(e) = sender.write(&s).await {
            error!("Failed to send data to client  due to : {:?}", e);
        }
    }
    sender.finish()?;

    let mut stat = stats.lock().await;
    stat.packets_sent += chunks_len as u64;
    drop(stat);
    Ok(())
}

pub async fn try_connect(endpoint: Endpoint, addr: SocketAddr) -> Result<Connection> {
    const RETRY_INTERVAL: Duration = Duration::from_secs(5);

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
