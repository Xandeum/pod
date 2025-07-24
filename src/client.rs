use anyhow::{anyhow, Error, Result};
use bincode::deserialize;
use log::{debug, error, info, trace, warn}; // Ensure all levels are imported
use quinn::{
    ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig,
    VarInt,
};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::RootCertStore;
use rustls::ServerConfig as RustlsServerConfig;
use rustls_pemfile::certs;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tokio::time::sleep;

use crate::cert::AcceptAllVerifier;
use crate::packet::{reassemble_packets, split_packet, AtlasOperation, Packet, MAX_PACKET_SIZE};
use crate::protos::{
    ArmageddonData, BigBangData, CreateFilePayload, MkDirPayload, PeekPayload, PokePayload,
    RenamePayload, RmDirPayload,
};
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
    info!("Starting stream loop to connect to main server at {}", addr);

    let mut rx2 = shutdown_rx.resubscribe();
    loop {
        tokio::select! {
            Ok(_) = rx2.recv() => {
                info!("Shutdown signal received, terminating stream loop.");
                return;
            }
            result = connect_and_handle_stream(endpoint.clone(), addr, storage_state.clone(), shutdown_rx, stats.clone()) => {
                match result {
                    Ok(()) => {
                        info!("Stream loop completed successfully.");
                        break;
                    }
                    Err(e) => {
                        error!("Connection to server failed: {}. Retrying in {}s...", e, RETRY_INTERVAL);
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
    info!("Successfully connected to server at {}, waiting for incoming streams...", connection.remote_address());

    loop {
        let stats_clone = stats.clone();
        match connection.accept_bi().await {
            Ok((mut send, mut recv)) => {
                let stream_id = send.id();
                info!("[STREAM-{}] Accepted new bidirectional stream.", stream_id);
                let storage_clone = storage_state.clone();

                tokio::spawn(async move {
                    if let Err(e) =
                        handle_stream(storage_clone, &mut send, &mut recv, stats_clone).await
                    {
                        error!("[STREAM-{}] Error handling stream: {:?}", stream_id, e);
                    } else {
                        info!("[STREAM-{}] Finished handling stream.", stream_id);
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept a bidirectional stream: {}. Connection may be closing.", e);
                return Err(e.into());
            }
        }
    }
}

async fn handle_stream(
    storage_state: StorageState,
    sender: &mut SendStream,
    receiver: &mut RecvStream,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let packet = receive_packets(receiver, stats.clone()).await?;

    match AtlasOperation::try_from(packet.meta.unwrap().op) {
        Ok(operation) => {
            debug!("[STREAM-{}] Handling operation: {:?}", sender.id(), operation);
            match operation {
                AtlasOperation::Handshake => {
                    let pkt = Packet::new_handshake();
                    send_packets(sender, pkt, stats.clone()).await?;
                }
                AtlasOperation::PBigbang => {
                    let big_bang_data: BigBangData = deserialize(&packet.data)?;
                    storage_state.handle_bigbang(big_bang_data).await?;
                }
                AtlasOperation::PArmageddon => {
                    let armageddon_data: ArmageddonData = deserialize(&packet.data)?;
                    storage_state.handle_armageddon(armageddon_data).await?;
                }
                AtlasOperation::PMkdir => {
                    let mkdir_data: MkDirPayload = deserialize(&packet.data)?;
                    storage_state.handle_mkdir(mkdir_data).await?;
                }
                AtlasOperation::PRmdir => {
                    let rmdir_data: RmDirPayload = deserialize(&packet.data)?;
                    storage_state.handle_rmdir(rmdir_data).await?;
                }
                AtlasOperation::POpenrw => {
                    let create_file_data: CreateFilePayload = deserialize(&packet.data)?;
                    storage_state.handle_create_file(create_file_data).await?;
                }
                AtlasOperation::PPeek => {
                    let peek_data: PeekPayload = deserialize(&packet.data)?;
                    storage_state.handle_peek(sender, peek_data, stats.clone()).await?;
                }
                AtlasOperation::PPoke => {
                    let poke_data: PokePayload = deserialize(&packet.data)?;
                    storage_state.handle_poke(poke_data, stats.clone()).await?;
                }
                AtlasOperation::PRename => {
                    let rename_data: RenamePayload = deserialize(&packet.data)?;
                    storage_state.handle_rename(rename_data).await?;
                }
                AtlasOperation::Cache => {
                    storage_state.handle_cache(packet, sender, stats.clone()).await?;
                }
                AtlasOperation::Quorum => {
                    storage_state.handle_quorum(sender, stats.clone()).await?;
                }
                _ => {
                    warn!("[STREAM-{}] Operation {:?} is not yet implemented.", sender.id(), operation);
                }
            }
        }
        Err(_) => {
            warn!(
                "[STREAM-{}] Received packet with unknown operation code: {:?}",
                sender.id(),
                packet.meta
            );
        }
    }
    Ok(())
}

pub async fn receive_packets(
    receiver: &mut RecvStream,
    stats: Arc<Mutex<Stats>>,
) -> Result<Packet> {

    let id = receiver.id();
    trace!("[STREAM-{}] Waiting to receive packet(s)...", id);

    let mut packets_chunks = Vec::new();
    let mut expected_total_chunks = 1;

    let recv = receiver;
    loop {
        let mut buffer = vec![0u8; MAX_PACKET_SIZE];
        if let Err(e) = recv.read_exact(&mut buffer).await {
            error!("[STREAM-{}] Failed to read full packet from stream: {}", recv.id(), e);
            return Err(e.into());
        }

        let packet: Packet = bincode::deserialize(&buffer)
            .map_err(|e| anyhow!("[STREAM-{}] Packet deserialization failed: {:?}", recv.id(), e))?;

        if packets_chunks.is_empty() {
            expected_total_chunks = packet.meta.unwrap().total_chunks;
            trace!("[STREAM-{}] Expecting {} total chunk(s).", recv.id(), expected_total_chunks);
        }

        packets_chunks.push(packet);

        if packets_chunks.len() as u32 == expected_total_chunks {
            break;
        }
    }

    let mut stat = stats.lock().await;
    stat.packets_received += packets_chunks.len() as u64;
    debug!("[STREAM-{}] Received {} packet chunk(s) successfully.", id, packets_chunks.len());

    reassemble_packets(packets_chunks)
        .await
        .ok_or_else(|| anyhow!("[STREAM-{}] Failed to reassemble packet chunks", id))
}

pub async fn send_packets(
    sender_stream: &mut SendStream,
    packet: Packet,
    stats: Arc<Mutex<Stats>>,
) -> Result<(), Error> {
    let sender = sender_stream;
    let packet_chunks = split_packet(packet);
    let chunks_len = packet_chunks.len();
    debug!("[STREAM-{}] Sending data as {} packet chunk(s).", sender.id(), chunks_len);

    for (i, pkt) in packet_chunks.iter().enumerate() {
        let s = bincode::serialize(pkt)
            .map_err(|e| anyhow!("[STREAM-{}] Packet serialization failed: {:?}", sender.id(), e))?;
        
        trace!("[STREAM-{}] Writing chunk {}/{} ({} bytes).", sender.id(), i + 1, chunks_len, s.len());
        if let Err(e) = sender.write_all(&s).await {
            error!("[STREAM-{}] Failed to write packet to stream: {}", sender.id(), e);
            return Err(e.into());
        }
    }
    
    sleep(Duration::from_secs(5)).await;
    // sender.finish();

    let mut stat = stats.lock().await;
    stat.packets_sent += chunks_len as u64;
    trace!("[STREAM-{}] Finished writing all packet chunks to stream buffer.", sender.id());
    Ok(())
}

pub async fn send_and_receive_packets(
    connection: Connection,
    data: Packet,
    stats: Arc<Mutex<Stats>>,
) -> Result<Packet, Error> {
    let peer_addr = connection.remote_address();
    trace!("Opening new stream to {}", peer_addr);

    let (mut sender, mut receiver) = connection.open_bi().await.map_err(|e| {
        error!("Failed to open bidirectional stream to {}: {}", peer_addr, e);
        e
    })?;

    let stream_id = sender.id();
    info!("[STREAM-{}] Opened new stream to {}", stream_id, peer_addr);
    
    send_packets(&mut sender, data, stats.clone()).await?;
    
    // sender.finish().await.map_err(|e| {
    //     warn!("[STREAM-{}] Error finishing send stream to {}: {}", stream_id, peer_addr, e);
    //     e
    // })?;
    trace!("[STREAM-{}] Finished sending, now waiting for response.", stream_id);
    
    receive_packets(&mut receiver, stats).await
}

pub async fn try_connect(endpoint: Endpoint, addr: SocketAddr) -> Result<Connection> {
    const RETRY_INTERVAL: Duration = Duration::from_secs(5);
    info!("Attempting to connect to {}...", addr);
    loop {
        match endpoint.connect(addr, "atlas")?.await {
            Ok(connection) => {
                info!("Connection established with {}", connection.remote_address());
                return Ok(connection);
            }
            Err(e) => {
                warn!("Connection attempt to {} failed: {}. Retrying...", addr, e);
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    }
}

pub async fn try_connect_with_retries(
    endpoint: Endpoint,
    addr: SocketAddr,
    max_retries: u8,
) -> Result<Connection> {
    const RETRY_INTERVAL: Duration = Duration::from_secs(5);
    let mut last_error = None;

    for attempt in 0..=max_retries {
        trace!("Connection attempt {}/{} to {}...", attempt + 1, max_retries + 1, addr);
        match endpoint.connect(addr, "atlas")?.await {
            Ok(connection) => {
                info!("Successfully connected to {}", addr);
                return Ok(connection);
            }
            Err(e) => {
                last_error = Some(e);
                if attempt < max_retries {
                    debug!(
                        "Connection attempt to {} failed. Retrying in {}s...",
                        addr,
                        RETRY_INTERVAL.as_secs()
                    );
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    }
    error!("Failed to connect to {} after {} retries.", addr, max_retries);
    Err(anyhow!("Failed to connect: {:?}", last_error))
}

pub fn configure_client() -> Result<ClientConfig> {
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

    let mut client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    ));

    let mut transport_config = TransportConfig::default();
    transport_config
        .max_idle_timeout(Some(Duration::from_secs(24 * 60 * 60).try_into().unwrap()))
        .stream_receive_window(VarInt::from_u64(1_000_000).unwrap()) 
        .max_concurrent_bidi_streams(VarInt::from_u64(100).unwrap());

    client_config.transport_config(Arc::new(transport_config));
    Ok(client_config)
}

pub fn configure_gossip_client() -> Result<ClientConfig> {
    let mut client_crypto = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();

    client_crypto
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptAllVerifier));

    let mut client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
    ));

    let mut transport_config = TransportConfig::default();
    transport_config
        .max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()))
        .stream_receive_window(VarInt::from_u64(1_000_000).unwrap())
        .max_concurrent_bidi_streams(VarInt::from_u64(10).unwrap());

    client_config.transport_config(Arc::new(transport_config));
    Ok(client_config)
}

pub fn configure_server() -> Result<ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["atlas".to_string()])?;
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    let server_crypto = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));

    let mut transport_config = TransportConfig::default();
    transport_config
        .max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()))
        .stream_receive_window(VarInt::from_u64(1_000_000).unwrap())
        .max_concurrent_bidi_streams(VarInt::from_u64(10).unwrap());

    server_config.transport_config(Arc::new(transport_config));
    Ok(server_config)
}

pub fn set_default_client() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|e| anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    Ok(())
}