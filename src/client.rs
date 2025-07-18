use anyhow::{anyhow, Error, Result};
use bincode::deserialize;
use log::{error, info, warn};
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

    // Handle based on operation
    match AtlasOperation::try_from(packet.meta.unwrap().op) {
        Ok(operation) => {
            match operation {
                AtlasOperation::Handshake => {
                    info!("Handshake");
                    let pkt = Packet::new_handshake();
                    let _ = send_packets(sender.clone(), pkt, stats.clone()).await?;
                }
                AtlasOperation::PBigbang => {
                    info!("Big bang");

                    let len = packet.meta.unwrap().length;

                    let big_bang_data: BigBangData = deserialize(&packet.data)?;

                    // let big_bang_data: BigBangData = deserialize(&packet.data[0..len as usize])?;
                    let _ = storage_state.handle_bigbang(big_bang_data).await?;
                }
                AtlasOperation::PArmageddon => {
                    info!("Armageddon");

                    let armageddon_data: ArmageddonData = deserialize(&packet.data)?;
                    let _ = storage_state.handle_armageddon(armageddon_data).await?;
                }

                AtlasOperation::PMkdir => {
                    info!("mkdfdir");

                    let mkdir_data: MkDirPayload = deserialize(&packet.data)?;
                    let _ = storage_state.handle_mkdir(mkdir_data).await?;
                }
                AtlasOperation::PRmdir => {
                    info!("rmdir");

                    let rmdir_data: RmDirPayload = deserialize(&packet.data)?;
                    let _ = storage_state.handle_rmdir(rmdir_data).await?;
                }

                AtlasOperation::POpenrw => {
                    info!("opernrw");

                    let rmdir_data: CreateFilePayload = deserialize(&packet.data)?;
                    let _ = storage_state.handle_create_file(rmdir_data).await?;
                }

                AtlasOperation::PPeek => {
                    info!("peek");

                    // Handle peek and send response
                    let peek_data: PeekPayload = deserialize(&packet.data)?;

                    let _ = storage_state
                        .handle_peek(sender.clone(), peek_data, stats.clone())
                        .await;
                }
                AtlasOperation::PPoke => {
                    info!("poke");

                    let poke_data: PokePayload = deserialize(&packet.data)?;

                    let _ = storage_state.handle_poke(poke_data, stats.clone()).await;
                }
                AtlasOperation::PRename => {
                    info!("rename");

                    let rename_data: RenamePayload = deserialize(&packet.data)?;

                    let _ = storage_state.handle_rename(rename_data).await?;
                }
                AtlasOperation::Cache => {
                    info!("cache");
                    let _ = storage_state
                        .handle_cache(packet, sender.clone(), stats.clone())
                        .await;
                }
                AtlasOperation::Quorum => {
                    info!("quorum");

                    let _ = storage_state
                        .handle_quorum(sender.clone(), stats.clone())
                        .await;
                }
                _ => {
                    info!("not implemented yet");
                }
            }
        }
        Err(_) => {
            error!(
                "Received packet with unknown operation code: {:?}",
                packet.meta
            );
        }
    }

    Ok(())
}

async fn receive_packets(
    receiver: Arc<Mutex<RecvStream>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<Packet> {
    // let mut buffer = vec![0u8; MAX_PACKET_SIZE];

    let mut packets_chunks = Vec::new();
    let mut expected_total_chunks = 1;

    let mut recv = receiver.lock().await;
    loop {
        let mut buffer = vec![0u8; MAX_PACKET_SIZE];

        match recv.read_exact(&mut buffer).await {
            Ok(()) => {
                info!("buffer len : {:?}", buffer.len());
                let packet: Packet = bincode::deserialize(&buffer)
                    .map_err(|e| anyhow!("Failed to deserialize packet : {:?}", e))?;

                if packets_chunks.is_empty() {
                    expected_total_chunks = packet.meta.unwrap().total_chunks;
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

    info!("packet received : {:?}", packets_chunks.len());

    // drop(stat);

    let reassembled_packet = reassemble_packets(packets_chunks)
        .await
        .ok_or_else(|| anyhow!("Failed To reassemble packet"))?;

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
        let s = bincode::serialize(&pkt)
            .map_err(|e| anyhow!("Failed to serialize packet : {:?}", e))?;
        if let Err(e) = sender.write(&s).await {
            error!("Failed to send data to client  due to : {:?}", e);
        }
    }

    // sleep(Duration::from_secs(5)).await;
    sender.finish()?;

    let mut stat = stats.lock().await;
    stat.packets_sent += chunks_len as u64;
    info!("Packets sent : {}", chunks_len);
    Ok(())
}

pub async fn send_and_receive_packets(
    connection: Connection,
    data: Packet,
    stats: Arc<Mutex<Stats>>,
) -> Result<Packet, Error> {
    // info!("connected {:?}",connection.);
    let client = connection.remote_address();
    // if connection.closed().now_or_never().is_some() {
    //     info!("Connection to {:?} is already closed", client);
    //     return Err(anyhow!("Connection already closed").into());
    // }

    match connection.open_bi().await {
        Ok((sender, receiver)) => {
            info!("Connection established, Sending data to {:?}", client);

            match send_packets(Arc::new(Mutex::new(sender)), data, stats.clone()).await {
                Ok(()) => info!("Send Packets Finished"),
                Err(e) => {
                    info!("****Send Packets Failed : {:?}", e);
                    return Err(e.into());
                }
            }
            let packet = receive_packets(Arc::new(Mutex::new(receiver)), stats).await?;

            return Ok(packet);
        }
        Err(e) => {
            info!(
                "***Failed to open Bi directional stream to client {:?} due to : {:?}",
                client, e
            );
            return Err(e.into());
        }
    }
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
                info!("Connection Attempt failed : {}", e);
            }
        }

        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

pub async fn try_connect_with_retries(
    endpoint: Endpoint,
    addr: SocketAddr,
    max_retries: u8,
) -> Result<Connection> {
    const RETRY_INTERVAL: Duration = Duration::from_secs(5);
    let mut last_error = None;

    // Total attempts = 1 initial + max_retries
    for attempt in 0..=max_retries {
        match endpoint.connect(addr, "atlas")?.await {
            Ok(connection) => {
                info!("Successfully connected to {}", addr);
                return Ok(connection);
            }
            Err(e) => {
                last_error = Some(e);
                info!(
                    "Connection attempt {}/{} to {} failed. Retrying in {}s...",
                    attempt + 1,
                    max_retries + 1,
                    addr,
                    RETRY_INTERVAL.as_secs()
                );

                if attempt < max_retries {
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    }
    return Err(anyhow!("Failed to connect  : {:?}", last_error));
}

pub fn configure_client() -> Result<ClientConfig> {
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

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
        .stream_receive_window(VarInt::from_u64(1_000_000).unwrap()) // 1MB per stream
        .max_concurrent_bidi_streams(VarInt::from_u64(100).unwrap());

    client_config.transport_config(Arc::new(transport_config));

    // Ok(ClientConfig::new(Arc::new(
    //     quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    // )))
    Ok(client_config)
}

pub fn configure_gossip_client() -> Result<ClientConfig> {
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

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

    // // Use the default crypto provider
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    // // Create a rustls client config builder that uses our custom verifier.
    // // We don't provide any root certificates because the verifier accepts everything anyway.
    // let mut client_crypto = rustls::ClientConfig::builder()
    //     .with_root_certificates(rustls::RootCertStore::empty()) // No root CAs needed
    //     .with_no_client_auth();

    // // This is the crucial part: setting the custom verifier.
    // client_crypto
    //     .dangerous()
    //     .set_certificate_verifier(Arc::new(AcceptAllVerifier));

    // // Wrap the rustls config into a Quinn client config.
    // let mut client_config = ClientConfig::new(Arc::new(
    //     quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
    // ));

    // // Apply your desired transport settings.
    // let mut transport_config = TransportConfig::default();
    // transport_config
    //     .max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap())) // Shorter timeout for gossip
    //     .stream_receive_window(VarInt::from_u64(1_000_000).unwrap())
    //     .max_concurrent_bidi_streams(VarInt::from_u64(10).unwrap()); // Fewer streams needed for gossip

    // client_config.transport_config(Arc::new(transport_config));

    // Ok(client_config)
}

pub fn configure_server() -> Result<ServerConfig> {
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    let mut server_crypto = RustlsServerConfig::builder()
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
        .map_err(|e| anyhow::anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    Ok(())
}
