use anyhow::{anyhow, Error, Result};
use log::{error, info, warn};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, TransportConfig, VarInt};
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use rustls_pemfile::certs;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};

use crate::cert::AcceptAllVerifier;
use crate::packet::{reassemble_packets, split_packet, Operation, Packet, MAX_PACKET_SIZE};
use crate::stats::Stats;
use crate::storage::StorageState;

/// Manages persistent QUIC streams for heartbeat and data operations
pub struct PersistentStreamManager {
    connection: Option<Connection>,
    heartbeat_stream: Option<(Arc<tokio::sync::Mutex<SendStream>>, Arc<tokio::sync::Mutex<RecvStream>>)>,
    data_stream: Option<(Arc<tokio::sync::Mutex<SendStream>>, Arc<tokio::sync::Mutex<RecvStream>>)>,
    endpoint: Endpoint,
    addr: SocketAddr,
    stats: Arc<Mutex<Stats>>,
}

impl PersistentStreamManager {
    pub fn new(endpoint: Endpoint, addr: SocketAddr, stats: Arc<Mutex<Stats>>) -> Self {
        Self {
            connection: None,
            heartbeat_stream: None,
            data_stream: None,
            endpoint,
            addr,
            stats,
        }
    }

    /// Establishes connection and creates the 2 persistent streams
        /// Establishes connection and creates the 2 persistent streams
        pub async fn connect(&mut self) -> Result<()> {
            info!("Establishing connection to Atlas at {}", self.addr);
            self.connection = Some(try_connect(self.endpoint.clone(), self.addr).await?);
            info!("QUIC connection established successfully");
            
            if let Some(ref connection) = self.connection {
                info!("Creating bidirectional streams");
                
                // Create heartbeat stream
                let (heartbeat_send, heartbeat_recv) = connection.open_bi().await?;
                self.heartbeat_stream = Some((
                    Arc::new(tokio::sync::Mutex::new(heartbeat_send)),
                    Arc::new(tokio::sync::Mutex::new(heartbeat_recv))
                ));
                info!("Heartbeat stream established (1/2)");

                
                // Send initial version packet on heartbeat stream so Atlas knows our version
                info!("Sending pod heartbeatt to Atlas...");
                if let Some((sender, _)) = &self.heartbeat_stream {
                    let heartbeat_packet = Packet::new_heartbeat();
                    send_packets(sender.clone(), heartbeat_packet, self.stats.clone()).await?;
                    info!("Pod heartbeat sent to Atlas");
                }
                
                // Send initial heartbeat packet to announce stream
                info!("Sending initial heartbeat packet to announce stream...");
                if let Some((sender, _)) = &self.heartbeat_stream {
                    let init_packet = Packet::new_heartbeat();
                    send_packets(sender.clone(), init_packet, self.stats.clone()).await?;
                    info!("Initial heartbeat packet sent successfully");
                }
                
                // Create data stream  
                let (data_send, data_recv) = connection.open_bi().await?;
                self.data_stream = Some((
                    Arc::new(tokio::sync::Mutex::new(data_send)),
                    Arc::new(tokio::sync::Mutex::new(data_recv))
                ));
                info!("Data stream established (2/2)");
                
                // Send initial packet on data stream so Atlas's accept_bi() sees it
                info!("Sending initial handshake packet to announce data stream...");
                if let Some((sender, _)) = &self.data_stream {
                    let init_packet = Packet::new_version(env!("CARGO_PKG_VERSION").to_string());
                    send_packets(sender.clone(), init_packet, self.stats.clone()).await?;
                    info!("Initial handshake packet sent successfully");
                }
                
                // Update active streams count in stats
                let stream_count = if self.heartbeat_stream.is_some() && self.data_stream.is_some() { 2 } else { 0 };
                {
                    let mut stats_guard = self.stats.lock().await;
                    stats_guard.active_streams = stream_count;
                }
                
                // Log final status with stream count
                let connection_stats = connection.stats();
                info!("Persistent connection ready: {} streams active, connection ID: {:?}, RTT: {}ms", 
                      stream_count, connection.stable_id(), connection_stats.path.rtt.as_millis());
            }
            
            Ok(())
        }

    /// Checks if streams are still valid
    pub fn is_connected(&self) -> bool {
        self.connection.is_some() && self.heartbeat_stream.is_some() && self.data_stream.is_some()
    }

    /// Checks connection health more thoroughly
    pub fn connection_status(&self) -> (bool, bool, bool) {
        let conn_alive = self.connection.as_ref().map_or(false, |c| !c.close_reason().is_some());
        let heartbeat_available = self.heartbeat_stream.is_some();
        let data_available = self.data_stream.is_some();
        
        (conn_alive, heartbeat_available, data_available)
    }

    /// Gets the actual number of open streams from the QUIC connection
    /// This includes all streams (both our managed ones and any others)
    pub fn get_stream_count(&self) -> u32 {
        if let Some(ref connection) = self.connection {
            // Since Quinn doesn't provide direct access to total active stream count,
            // we track our managed streams and report that count
            // In practice, for this application, we expect only our 2 managed streams
            let mut count = 0;
            if self.heartbeat_stream.is_some() {
                count += 1;
            }
            if self.data_stream.is_some() {
                count += 1;
            }
            count
        } else {
            0
        }
    }

    /// Gets the heartbeat stream (sender, receiver)
    pub fn get_heartbeat_stream(&self) -> Option<&(Arc<tokio::sync::Mutex<SendStream>>, Arc<tokio::sync::Mutex<RecvStream>>)> {
        self.heartbeat_stream.as_ref()
    }

    /// Gets the data stream (sender, receiver) 
    pub fn get_data_stream(&self) -> Option<&(Arc<tokio::sync::Mutex<SendStream>>, Arc<tokio::sync::Mutex<RecvStream>>)> {
        self.data_stream.as_ref()
    }

    /// Sends a heartbeat packet on the heartbeat stream
    pub async fn _send_heartbeat(&self) -> Result<()> {
        let start_time = std::time::Instant::now();
        info!("Sending heartbeat packet...");
        
        if let Some((sender, _)) = &self.heartbeat_stream {
            let heartbeat_packet = Packet::new_heartbeat();
            info!("Heartbeat packet created: {:?}", heartbeat_packet.meta);
            
            match send_packets(sender.clone(), heartbeat_packet, self.stats.clone()).await {
                Ok(()) => {
                    let elapsed = start_time.elapsed();
                    info!("Heartbeat sent successfully in {:?}", elapsed);
                    
                    // Log stream status
                    let (conn_alive, hb_available, data_available) = self.connection_status();
                    info!("Stream Status: Connection={}, Heartbeat={}, Data={}", 
                          conn_alive, hb_available, data_available);
                }
                Err(e) => {
                    error!("Failed to send heartbeat packet: {}", e);
                    return Err(e);
                }
            }
        } else {
            error!("Heartbeat stream not available for sending");
            return Err(anyhow!("Heartbeat stream not available"));
        }
        
        Ok(())
    }

    /// Waits for heartbeat response with timeout
    pub async fn _wait_for_heartbeat_response(&self, timeout: Duration) -> Result<()> {
        let start_time = std::time::Instant::now();
        info!("Waiting for heartbeat response (timeout: {:?})...", timeout);
        
        if let Some((_, receiver)) = &self.heartbeat_stream {
            match tokio::time::timeout(timeout, receive_packets(receiver.clone(), self.stats.clone())).await {
                Ok(Ok(packet)) => {
                    let elapsed = start_time.elapsed();
                    info!("Received packet in {:?}: {:?}", elapsed, packet.meta);
                    
                    if packet.meta.op == Operation::Heartbeat {
                        info!("Heartbeat response received successfully! RTT: {:?}", elapsed);
                        
                        // Update stats
                        let mut stats = self.stats.lock().await;
                        info!("Current Stats: {} packets sent, {} packets received", 
                              stats.packets_sent, stats.packets_received);
                        
                        Ok(())
                    } else {
                        error!("Expected heartbeat response, got: {:?}", packet.meta.op);
                        Err(anyhow!("Expected heartbeat response, got: {:?}", packet.meta.op))
                    }
                }
                Ok(Err(e)) => {
                    error!("Error receiving heartbeat response: {}", e);
                    Err(e)
                }
                Err(_) => {
                    error!("Heartbeat response timeout after {:?}", timeout);
                    Err(anyhow!("Heartbeat response timeout"))
                }
            }
        } else {
            error!("Heartbeat stream not available for receiving");
            Err(anyhow!("Heartbeat stream not available"))
        }
    }

    /// Listens for server-initiated heartbeat and responds to it
    pub async fn receive_heartbeat_from_server(&self) -> Result<()> {
        let start_time = std::time::Instant::now();
        info!("Listening for server-initiated heartbeat...");
        
        if let Some((sender, receiver)) = &self.heartbeat_stream {
            // Wait for heartbeat from server
            match receive_packets(receiver.clone(), self.stats.clone()).await {
                Ok(packet) => {
                    let elapsed = start_time.elapsed();
                    info!("Received server packet in {:?}: {:?}", elapsed, packet.meta);
                    
                    if packet.meta.op == Operation::Heartbeat {
                        info!("Server heartbeat received! Sending response...");
                        
                        // Respond with heartbeat
                        let response_packet = Packet::new_heartbeat();
                        match send_packets(sender.clone(), response_packet, self.stats.clone()).await {
                            Ok(()) => {
                                let total_elapsed = start_time.elapsed();
                                info!("Heartbeat response sent successfully! Total cycle: {:?}", total_elapsed);
                                Ok(())
                            }
                            Err(e) => {
                                error!("Failed to send heartbeat response: {}", e);
                                Err(e)
                            }
                        }
                    } else {
                        error!("Expected heartbeat from server, got: {:?}", packet.meta.op);
                        Err(anyhow!("Expected heartbeat from server, got: {:?}", packet.meta.op))
                    }
                }
                Err(e) => {
                    error!("Error receiving heartbeat from server: {}", e);
                    Err(e)
                }
            }
        } else {
            error!("Heartbeat stream not available");
            Err(anyhow!("Heartbeat stream not available"))
        }
    }

    /// Sends data operation on the data stream
    pub async fn send_data_operation(&self, packet: Packet) -> Result<()> {
        let start_time = std::time::Instant::now();
        info!("Sending data operation: {:?}", packet.meta);
        info!("   Operation: {:?}", packet.meta.op);
        info!("   Page: {}, Offset: {}, Length: {}", packet.meta.page_no, packet.meta.offset, packet.meta.length);
        
        if let Some((sender, _)) = &self.data_stream {
            match send_packets(sender.clone(), packet, self.stats.clone()).await {
                Ok(()) => {
                    let elapsed = start_time.elapsed();
                    info!("Data operation sent successfully in {:?}", elapsed);
                }
                Err(e) => {
                    error!("Failed to send data operation: {}", e);
                    return Err(e);
                }
            }
        } else {
            error!("Data stream not available for sending");
            return Err(anyhow!("Data stream not available"));
        }
        Ok(())
    }

    /// Receives data from the data stream
    pub async fn receive_data_operation(&self) -> Result<Packet> {
        let start_time = std::time::Instant::now();
        info!("Waiting for data operation on data stream...");
        
        if let Some((_, receiver)) = &self.data_stream {
            match receive_packets(receiver.clone(), self.stats.clone()).await {
                Ok(packet) => {
                    let elapsed = start_time.elapsed();
                    info!("Data operation received in {:?}: {:?}", elapsed, packet.meta);
                    info!("   Operation: {:?}", packet.meta.op);
                    info!("   Page: {}, Offset: {}, Length: {}", packet.meta.page_no, packet.meta.offset, packet.meta.length);
                    info!("   Data size: {} bytes", packet.data.len());
                    Ok(packet)
                }
                Err(e) => {
                    error!("Failed to receive data operation: {}", e);
                    Err(e)
                }
            }
        } else {
            error!("Data stream not available for receiving");
            Err(anyhow!("Data stream not available"))
        }
    }

    /// Handles data operations (Peek/Poke) on the dedicated data stream
    pub async fn handle_data_operation(&self, storage_state: &StorageState, packet: Packet) -> Result<()> {
        let start_time = std::time::Instant::now();
        info!("Handling data operation: {:?}", packet.meta.op);
        info!("   Target: Page {}, Offset {}, Length {}", packet.meta.page_no, packet.meta.offset, packet.meta.length);
        
        match packet.meta.op {
            Operation::Peek => {
                info!("Processing PEEK operation...");
                if let Some((sender, _)) = &self.data_stream {
                    match storage_state.handle_peek(sender.clone(), packet, self.stats.clone()).await {
                        Ok(()) => {
                            let elapsed = start_time.elapsed();
                            info!("PEEK operation completed successfully in {:?}", elapsed);
                        }
                        Err(e) => {
                            error!("PEEK operation failed: {}", e);
                            return Err(e);
                        }
                    }
                } else {
                    error!("Data stream sender not available for PEEK response");
                    return Err(anyhow!("Data stream sender not available"));
                }
            }
            Operation::Poke => {
                info!("Processing POKE operation...");
                match storage_state.handle_poke(packet).await {
                    Ok(()) => {
                        let elapsed = start_time.elapsed();
                        info!("POKE operation completed successfully in {:?}", elapsed);
                    }
                    Err(e) => {
                        error!("POKE operation failed: {}", e);
                        return Err(e);
                    }
                }
            }
            _ => {
                error!("Invalid operation for data stream: {:?}", packet.meta.op);
                return Err(anyhow!("Invalid operation for data stream: {:?}", packet.meta.op));
            }
        }
        Ok(())
    }
}

/// New persistent stream loop with heartbeat mechanism
pub async fn start_persistent_stream_loop(
    endpoint: Endpoint,
    addr: SocketAddr,
    storage_state: StorageState,
    shutdown_rx: &mut broadcast::Receiver<()>,
    stats: Arc<Mutex<Stats>>,
) {
    const RETRY_INTERVAL: u64 = 5;
    const STATUS_LOG_INTERVAL: u64 = 60; // Log detailed status every minute

    // Note: The heartbeat mechanism keeps the ENTIRE QUIC connection alive,
    // which means BOTH streams (heartbeat + data) remain persistent.
    // Even if no data is sent on the data stream for days/weeks, the
    // heartbeat stream activity prevents the connection from timing out.
    // The server (atlas) initiates heartbeats, and this client responds.
    
    info!("Starting persistent stream manager (heartbeats: server-initiated, status interval: {}s)", STATUS_LOG_INTERVAL);
    
    let mut rx2 = shutdown_rx.resubscribe();
    let mut stream_manager = PersistentStreamManager::new(endpoint, addr, stats);
    let mut status_log_interval = tokio::time::interval(Duration::from_secs(STATUS_LOG_INTERVAL));
    let mut connection_attempts = 0u32;
    let mut successful_heartbeats = 0u64;
    let mut failed_heartbeats = 0u64;
    let mut data_operations_handled = 0u64;
    
    loop {
        // Try to establish connection and streams
        if !stream_manager.is_connected() {
            connection_attempts += 1;
            info!("Connection attempt #{}", connection_attempts);
            
            match stream_manager.connect().await {
                Ok(()) => {
                    info!("Streams established successfully on attempt #{}", connection_attempts);
                    connection_attempts = 0; // Reset counter on success
                }
                Err(e) => {
                    error!("Connection attempt #{} failed: {} - retrying in {}s", connection_attempts, e, RETRY_INTERVAL);
                    tokio::time::sleep(Duration::from_secs(RETRY_INTERVAL)).await;
                    continue;
                }
            }
        }

        tokio::select! {
            Ok(_) = rx2.recv() => {
                info!("Shutdown signal received in persistent stream loop");
                info!("Final Stats: {} successful heartbeats, {} failed heartbeats, {} data operations", 
                      successful_heartbeats, failed_heartbeats, data_operations_handled);
                return;
            }
            
            // Periodic status logging
            _ = status_log_interval.tick() => {
                let (conn_alive, hb_available, data_available) = stream_manager.connection_status();
                let stats = stream_manager.stats.lock().await;
                let active_streams = stream_manager.get_stream_count();
                
                info!("Status report: {} streams active, heartbeats: {}/{} success, {} data operations handled, packets: sent={} received={}", 
                      active_streams, successful_heartbeats, successful_heartbeats + failed_heartbeats, 
                      data_operations_handled, stats.packets_sent, stats.packets_received);
            }
            
            // Listen for server-initiated heartbeats
            result = stream_manager.receive_heartbeat_from_server() => {
                match result {
                    Ok(()) => {
                        successful_heartbeats += 1;
                        info!("Heartbeat #{} responded successfully", successful_heartbeats);
                    }
                    Err(e) => {
                        failed_heartbeats += 1;
                        error!("Heartbeat #{} failed: {} - reconnecting streams", failed_heartbeats, e);
                        
                        // Reset active streams count when recreating stream manager
                        {
                            let mut stats_guard = stream_manager.stats.lock().await;
                            stats_guard.active_streams = 0;
                        }
                        
                        stream_manager = PersistentStreamManager::new(stream_manager.endpoint.clone(), stream_manager.addr, stream_manager.stats.clone());
                        continue;
                    }
                }
            }
            
            // Handle incoming data operations
            result = stream_manager.receive_data_operation() => {
                match result {
                    Ok(packet) => {
                        data_operations_handled += 1;
                        info!("Processing data operation #{}: {:?}", data_operations_handled, packet.meta.op);
                        
                        match stream_manager.handle_data_operation(&storage_state, packet).await {
                            Ok(()) => {
                                info!("Data operation #{} completed successfully", data_operations_handled);
                            }
                            Err(e) => {
                                error!("Data operation #{} failed: {}", data_operations_handled, e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Data stream error: {} - reconnecting streams", e);
                        
                        // Reset active streams count when recreating stream manager
                        {
                            let mut stats_guard = stream_manager.stats.lock().await;
                            stats_guard.active_streams = 0;
                        }
                        
                        stream_manager = PersistentStreamManager::new(stream_manager.endpoint.clone(), stream_manager.addr, stream_manager.stats.clone());
                        continue;
                    }
                }
            }
        }
    }
}

pub async fn _start_stream_loop(
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
            result = _connect_and_handle_stream(endpoint.clone(), addr, storage_state.clone(),shutdown_rx,stats.clone()) => {
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

pub async fn _connect_and_handle_stream(
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
                        _handle_stream(storage_clone, sender, receiver, stats_clone).await
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

async fn _handle_stream(
    storage_state: StorageState,
    sender: Arc<tokio::sync::Mutex<SendStream>>,
    receiver: Arc<tokio::sync::Mutex<RecvStream>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    // Receive packets
    let packet = receive_packets(receiver, stats.clone()).await?;

    // Handle based on operation
    match packet.meta.op {
        Operation::Handshake => {
            let pkt = Packet::new_handshake();
            let _ = send_packets(sender.clone(), pkt, stats.clone()).await?;
        }
        Operation::Heartbeat => {
            // Respond to heartbeat with heartbeat
            let pkt = Packet::new_heartbeat();
            let _ = send_packets(sender.clone(), pkt, stats.clone()).await?;
        }
        Operation::Version => {
          // do nothing
        }
        Operation::Peek => {
            // Handle peek and send response
            let _ = storage_state
                .handle_peek(sender.clone(), packet, stats.clone())
                .await;
        }
        Operation::Poke => {
            let _ = storage_state.handle_poke(packet).await;
        }
    }

    Ok(())
}

async fn receive_packets(
    receiver: Arc<Mutex<RecvStream>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<Packet> {
    let start_time = std::time::Instant::now();
    info!("Starting packet reception...");

    let mut packets_chunks = Vec::new();
    let mut expected_total_chunks = 1;

    let mut recv = receiver.lock().await;
    info!("Receiver stream locked, beginning read loop...");
    
    loop {
        let mut buffer = vec![0u8; MAX_PACKET_SIZE];
        info!("Reading packet chunk (expecting {} total chunks)...", expected_total_chunks);

        match recv.read_exact(&mut buffer).await {
            Ok(()) => {
                let packet: Packet = bincode::deserialize(&buffer)
                    .map_err(|e| anyhow!("Failed to deserialize packet : {:?}", e))?;

                if packets_chunks.is_empty() {
                    expected_total_chunks = packet.meta.total_chunks;
                    info!("First chunk received, expecting {} total chunks", expected_total_chunks);
                }
                info!("Received packet chunk {}/{}: {:?}", 
                      packets_chunks.len() + 1, expected_total_chunks, packet.meta);

                packets_chunks.push(packet);

                if packets_chunks.len() as u32 == expected_total_chunks {
                    info!("All {} chunks received, breaking read loop", expected_total_chunks);
                    break;
                }
            }
            Err(e) => {
                error!("Failed to receive from stream: {}", e);
                return Err(e.into());
            }
        };
    }

    let mut stat = stats.lock().await;
    stat.packets_received += packets_chunks.len() as u64;

    let elapsed = start_time.elapsed();
    info!("Updated stats: {} total packets received", stat.packets_received);
    info!("Packet reception completed in {:?}: {} chunks", elapsed, packets_chunks.len());

    let reassembled_packet = reassemble_packets(packets_chunks)
        .await
        .ok_or_else(|| anyhow!("Failed To reassemble packet"))?;

    info!("Packet reassembly successful: {:?}", reassembled_packet.meta);
    Ok(reassembled_packet)
}

pub async fn send_packets(
    sender_stream: Arc<tokio::sync::Mutex<SendStream>>,
    packet: Packet,
    stats: Arc<Mutex<Stats>>,
) -> Result<(), Error> {
    let start_time = std::time::Instant::now();
    info!("Starting packet transmission: {:?}", packet.meta);
    
    let mut sender = sender_stream.lock().await;
    info!("Sender stream locked");
    
    let packet_chunks = split_packet(packet);
    let chunks_len = packet_chunks.len();
    info!("Split into {} chunks for transmission", chunks_len);

    for (i, pkt) in packet_chunks.iter().enumerate() {
        info!("Sending chunk {}/{}: {:?}", i + 1, chunks_len, pkt.meta);
        
        let s = bincode::serialize(&pkt)
            .map_err(|e| anyhow!("Failed to serialize packet : {:?}", e))?;
            
        if let Err(e) = sender.write(&s).await {
            error!("Failed to send chunk {}/{}: {:?}", i + 1, chunks_len, e);
            return Err(e.into());
        }
        
        info!("Chunk {}/{} sent successfully ({} bytes)", i + 1, chunks_len, s.len());
    }
 
    let mut stat = stats.lock().await;
    stat.packets_sent += chunks_len as u64;
    let elapsed = start_time.elapsed();
    
    info!("Updated stats: {} total packets sent", stat.packets_sent);
    info!("Packet transmission completed in {:?}: {} chunks sent", elapsed, chunks_len);
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

    // let cert_file = File::open("server.crt")?;
    // let mut cert_reader = BufReader::new(cert_file);
    // let certs: Vec<CertificateDer<'_>> =
    // certs(&mut cert_reader).collect::<Result<_, std::io::Error>>()?;

    let cert_pem = include_str!("../server.crt");
    let mut cert_reader = BufReader::new(cert_pem.as_bytes());
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
        .max_idle_timeout(Some(Duration::from_secs(3600).try_into().unwrap())) // 1 hour idle timeout - heartbeat keeps it alive
        .stream_receive_window(VarInt::from_u64(1_000_000).unwrap()) // 1MB per stream
        .max_concurrent_bidi_streams(VarInt::from_u64(10).unwrap()) // Reduced to 10 since we only need 2 persistent streams
        .keep_alive_interval(Some(Duration::from_secs(30))); // Send keep-alive every 30 seconds to prevent idle timeout

    client_config.transport_config(Arc::new(transport_config));

    // Ok(ClientConfig::new(Arc::new(
    //     quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    // )))
    Ok(client_config)
}
