use anyhow::{anyhow, Error, Result};
use bincode::deserialize;
use log::{error, info, warn};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig, VarInt};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::RootCertStore;
use rustls_pemfile::certs;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use rustls::ServerConfig as RustlsServerConfig;

use crate::cert::AcceptAllVerifier;
use crate::packet::{reassemble_packets, split_packet, AtlasOperation, Packet, MAX_PACKET_SIZE};
use crate::protos::{ArmageddonData, BigBangData, CachePayload, CreateFilePayload, GlobalCatalogPage, MkDirPayload, RmDirPayload};
use crate::protos::{PeekPayload, PokePayload, RenamePayload};
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
        pub async fn connect(&mut self, storage_state: StorageState) -> Result<()> {
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

                if let Some((_, receiver)) = &self.data_stream {
                    let packet = receive_packets(receiver.clone(), self.stats.clone()).await?;
                    info!("Received packet: {:?}", packet.meta);

                    if packet.meta.as_ref().map_or(false, |meta| meta.op == AtlasOperation::PPoke as i32) {
                        info!("poke packet received : {:?}", packet);
                        let received_catalog: GlobalCatalogPage = deserialize(&packet.data)?;
                        info!("📖 RECEIVED CATALOG from Atlas: {} filesystems", received_catalog.filesystems.len());
                        
                        for fs in &received_catalog.filesystems {
                            info!("  📁 Atlas FS ID: {} (Pod: {})", fs.fs_id, fs.home_pod_id);
                        }

                        // 📝 WRITE CATALOG: Write the received catalog to pod's storage
                        info!("📝 WRITING CATALOG: Replacing pod's catalog with received data");
                        match storage_state.write_catalog(received_catalog.clone()).await {
                            Ok(()) => {
                                info!("✅ CATALOG WRITTEN: Atlas catalog successfully written to pod storage");
                                
                                // Verify the write by reading it back
                                // match storage_state.clone().read_catalog().await {
                                //     Ok(written_catalog) => {
                                //         info!("✅ CATALOG VERIFIED: {} filesystems now in pod storage", written_catalog.filesystems.len());
                                //     }
                                //     Err(e) => {
                                //         error!("❌ CATALOG VERIFY FAILED: {}", e);
                                //     }
                                // }
                            }
                            Err(e) => {
                                error!("❌ CATALOG WRITE ``: {}", e);
                            }
                        }
                    }
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
                    
                    if packet.meta.as_ref().map_or(false, |meta| meta.op == AtlasOperation::PHeartbeat as i32) {
                        info!("Heartbeat response received successfully! RTT: {:?}", elapsed);
                        
                        // Update stats
                        let mut stats = self.stats.lock().await;
                        info!("Current Stats: {} packets sent, {} packets received", 
                              stats.packets_sent, stats.packets_received);
                        
                        Ok(())
                    } else {
                        let op_code = packet.meta.as_ref().map(|m| m.op).unwrap_or(-1);
                        error!("Expected heartbeat response, got: {:?}", op_code);
                        Err(anyhow!("Expected heartbeat response, got: {:?}", op_code))
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
                    
                    if packet.meta.as_ref().map_or(false, |meta| meta.op == AtlasOperation::PHeartbeat as i32) {
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
                        let op_code = packet.meta.as_ref().map(|m| m.op).unwrap_or(-1);
                        error!("Expected heartbeat from server, got: {:?}", op_code);
                        Err(anyhow!("Expected heartbeat from server, got: {:?}", op_code))
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
        if let Some(meta) = packet.meta.as_ref() {
            info!("   Operation: {:?}", meta.op);
        }
        // info!("   Page: {}, Offset: {}, Length: {}", packet.meta.as_ref().unwrap().page_no, packet.meta.as_ref().unwrap().offset, packet.meta.as_ref().unwrap().length);
        
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
                    if let Some(meta) = packet.meta.as_ref() {
                        info!("   Operation: {:?}", meta.op);
                    }
                    // info!("   Page: {}, Offset: {}, Length: {}", packet.meta.as_ref().unwrap().page_no, packet.meta.as_ref().unwrap().offset, packet.meta.as_ref().unwrap().length);
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

        let op = packet.meta.as_ref()
            .ok_or_else(|| anyhow!("Packet missing metadata"))
            .and_then(|meta| AtlasOperation::try_from(meta.op))?;
        info!("Handling data operation: {:?}", op);
        let storage_state_clone = storage_state.clone();
        
        match op {
            AtlasOperation::Handshake => {
                let pkt = Packet::new_handshake();
                // send_packets(sender, pkt, stats.clone()).await?;
            }
            AtlasOperation::PBigbang => {
                info!("Received big bang data: {:?}", packet.data);
                let big_bang_data: BigBangData = deserialize(&packet.data)?;
                info!("Received big bang data: {:?}", big_bang_data);

                storage_state_clone.handle_bigbang(big_bang_data).await?;
            }
            AtlasOperation::PArmageddon => {
                let armageddon_data: ArmageddonData = deserialize(&packet.data)?;
                storage_state_clone.handle_armageddon(armageddon_data).await?;
            }
            AtlasOperation::PMkdir => {
                let mkdir_data: MkDirPayload = deserialize(&packet.data)?;
                storage_state_clone.handle_mkdir(mkdir_data).await?;
            }
            AtlasOperation::PRmdir => {
                let rmdir_data: RmDirPayload = deserialize(&packet.data)?;
                storage_state_clone.handle_rmdir(rmdir_data).await?;
            }
            AtlasOperation::POpenrw => {
                let create_file_data: CreateFilePayload = deserialize(&packet.data)?;
                storage_state_clone.handle_create_file(create_file_data).await?;
            }
            AtlasOperation::PPeek => {
                let peek_data: PeekPayload = deserialize(&packet.data)?;
                if let Some((sender, _)) = &self.data_stream {
                    storage_state_clone.handle_peek(sender.clone(), peek_data, self.stats.clone()).await?;
                }
            }
            AtlasOperation::PPoke => {
                let poke_data: PokePayload = deserialize(&packet.data)?;
                storage_state_clone.handle_poke(poke_data, self.stats.clone()).await?;
            }
            AtlasOperation::PRename => {
                let rename_data: RenamePayload = deserialize(&packet.data)?;
                storage_state_clone.handle_rename(rename_data).await?;
            }
            AtlasOperation::PQuorum => {
                if let Some((sender, _)) = &self.data_stream {
                    storage_state_clone.handle_quorum(sender.clone(), self.stats.clone()).await?;
                }
            }
            AtlasOperation::PCache => {
                // let cache_data: CachePayload = deserialize(&packet.data)?;
                if let Some((sender, _)) = &self.data_stream {
                    storage_state_clone.handle_cache(packet, sender.clone(), self.stats.clone()).await?;
                }
            }
            _ => {
                // warn!("[STREAM-{}] Operation {:?} is not yet implemented.", self.s.id(), op);
                return Err(anyhow!("Operation {:?} is not yet implemented", op));
            }
        }
    
    // Err(e) => {
    //     error!(
    //         "[STREAM-{}] Received packet with unknown operation code: {:?}",
    //         sender.id(),
    //         packet.meta
    //     );
    //     return Err(anyhow!("Received packet with unknown operation code: {:?}", packet.meta));
    // }

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
            
            match stream_manager.connect(storage_state.clone()).await {
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
                        // info!("Processing data operation #{}: {:?}", data_operations_handled, packet.meta.op);
                        
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

// pub async fn _start_stream_loop(
//     endpoint: Endpoint,
//     addr: SocketAddr,
//     storage_state: StorageState,
//     shutdown_rx: &mut broadcast::Receiver<()>,
//     stats: Arc<Mutex<Stats>>,
// ) {
//     const RETRY_INTERVAL: u64 = 5;

//     let mut rx2 = shutdown_rx.resubscribe();
//     loop {
//         tokio::select! {
//             Ok(_) = rx2.recv() => {
//                 info!("Shutdown received in start_stream_loop");
//                 return ();
//             }
//             result = _connect_and_handle_stream(endpoint.clone(), addr, storage_state.clone(),shutdown_rx,stats.clone()) => {
//                 match result {
//                     Ok(()) => {
//                         log::info!("Stream loop completed successfully");
//                         break;
//                     }
//                     Err(e) => {
//                         log::error!("Stream error: {}, retrying in {} seconds", e, RETRY_INTERVAL);
//                         tokio::time::sleep(Duration::from_secs(RETRY_INTERVAL)).await;
//                     }
//                 }
//             }
//         }
//     }
// }

// pub async fn _connect_and_handle_stream(
//     endpoint: Endpoint,
//     addr: SocketAddr,
//     storage_state: StorageState,
//     shutdown_rx: &mut broadcast::Receiver<()>,
//     stats: Arc<Mutex<Stats>>,
// ) -> Result<()> {
//     let connection = try_connect(endpoint, addr).await?;

//     info!("Connected to server, waiting for streams");

//     loop {
//         let stats_clone = stats.clone();
//         match connection.accept_bi().await {
//             Ok((send, recv)) => {
//                 info!("Accepted A Bi Stream");
//                 let sender = Arc::new(tokio::sync::Mutex::new(send));
//                 let receiver = Arc::new(tokio::sync::Mutex::new(recv));
//                 let storage_clone = storage_state.clone();

//                 tokio::spawn(async move {
//                     if let Err(e) =
//                         _handle_stream(storage_clone, sender, receiver, stats_clone).await
//                     {
//                         error!("Error handling stream: {:?}", e);
//                     }
//                 });
//             }
//             Err(e) => {
//                 error!("Failed to accept bidirectional stream: {:?}", e);
//                 return Err(e.into());
//             }
//         }
//     }
// }

// async fn _handle_stream(
//     storage_state: StorageState,
//     sender: Arc<tokio::sync::Mutex<SendStream>>,
//     receiver: Arc<tokio::sync::Mutex<RecvStream>>,
//     stats: Arc<Mutex<Stats>>,
// ) -> Result<()> {
//     // Receive packets
//     let packet = receive_packets(receiver, stats.clone()).await?;

//     // Handle based on operation
//     match packet.meta.op {
//         Operation::Handshake => {
//             let pkt = Packet::new_handshake();
//             let _ = send_packets(sender.clone(), pkt, stats.clone()).await?;
//         }
//         Operation::Heartbeat => {
//             // Respond to heartbeat with heartbeat
//             let pkt = Packet::new_heartbeat();
//             let _ = send_packets(sender.clone(), pkt, stats.clone()).await?;
//         }
//         Operation::Version => {
//           // do nothing
//         }
//         Operation::Peek => {
//             // Handle peek and send response
//             let _ = storage_state
//                 .handle_peek(sender.clone(), packet, stats.clone())
//                 .await;
//         }
//         Operation::Poke => {
//             let _ = storage_state.handle_poke(packet).await;
//         }
//     }

//     Ok(())
// }

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
                info!("buffer : {:?}", buffer);
                let packet: Packet = bincode::deserialize(&buffer)
                    .map_err(|e| anyhow!("Failed to deserialize packet : {:?}", e))?;

                if packets_chunks.is_empty() {
                    expected_total_chunks = packet.meta.as_ref()
                        .map(|meta| meta.total_chunks)
                        .unwrap_or(1);
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
        .max_idle_timeout(Some(Duration::from_secs(24 * 60 * 60).try_into().map_err(|e| anyhow!("Invalid timeout: {}", e))?))
        .stream_receive_window(VarInt::from_u64(1_000_000).map_err(|e| anyhow!("Invalid stream window: {}", e))?) 
        .max_concurrent_bidi_streams(VarInt::from_u64(100).map_err(|e| anyhow!("Invalid stream count: {}", e))?);

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
        .max_idle_timeout(Some(Duration::from_secs(60).try_into().map_err(|e| anyhow!("Invalid timeout: {}", e))?))
        .stream_receive_window(VarInt::from_u64(1_000_000).map_err(|e| anyhow!("Invalid stream window: {}", e))?)
        .max_concurrent_bidi_streams(VarInt::from_u64(10).map_err(|e| anyhow!("Invalid stream count: {}", e))?);

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
        .max_idle_timeout(Some(Duration::from_secs(60).try_into().map_err(|e| anyhow!("Invalid timeout: {}", e))?))
        .stream_receive_window(VarInt::from_u64(1_000_000).map_err(|e| anyhow!("Invalid stream window: {}", e))?)
        .max_concurrent_bidi_streams(VarInt::from_u64(10).map_err(|e| anyhow!("Invalid stream count: {}", e))?);

    server_config.transport_config(Arc::new(transport_config));
    Ok(server_config)
}

pub fn set_default_client() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|e| anyhow!("Failed to install CryptoProvider: {:?}", e))?;

    Ok(())
}