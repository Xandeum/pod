use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    sync::{atomic::{AtomicU64, Ordering}, Arc},
    time::Duration,
};

use anyhow::{anyhow, Result};
use bincode::{deserialize, serialize};
use chrono::Utc;
use log::{debug, error, info, trace, warn};
use rand::seq::{IndexedRandom, SliceRandom};
use serde::{Deserialize, Serialize};
use tokio::{
    net::UdpSocket as TokioUdpSocket,
    sync::{Mutex, RwLock},
    time::{self, timeout},
};

use crate::stats::Stats;

const MAX_CONNECTION_RETRIES: u8 = 3;
const GOSSIP_INTERVAL_SECS: u64 = 120;
const MAX_GOSSIP_PEERS: usize = 3;
const MAX_INACTIVITY_PERIOD_IN_SECS: u64 = 60 * 60;
pub const GOSSIP_PORT: u16 = 9000;
pub const GOSSIP_SERVER_PORT: u16 = 9001;

// UDP-specific constants
const MAX_UDP_PACKET_SIZE: usize = 1400; // Stay under typical MTU
const UDP_TIMEOUT_SECS: u64 = 5;
const UDP_RETRY_ATTEMPTS: u32 = 3;

/// Get the local IP address by creating a test connection
fn get_local_ip() -> Result<std::net::IpAddr> {
    // Connect to a dummy address to discover our local IP
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("8.8.8.8:80")?;
    Ok(socket.local_addr()?.ip())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub addr: SocketAddr,
    pub last_seen: u64,
    pub version: Option<String>,
}

impl Peer {
    /// Returns a display-friendly version string
    pub fn version_display(&self) -> &str {
        self.version.as_deref().unwrap_or("unknown")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerList {
    pub list: Vec<Peer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    Request,
    Response,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipMessage {
    pub peer_list: PeerList,
    pub sender_version: String,
    pub message_id: u64, // Add unique ID for deduplication
    pub timestamp: u64,  // Add timestamp for message ordering
    pub message_type: MessageType, // Distinguish between requests and responses
}

impl PeerList {
    pub fn new() -> Self {
        Self { list: Vec::new() }
    }

    /// Adds a peer to the list if it doesn't already exist.
    pub fn add(&mut self, peer_addr: SocketAddr, update_last_seen: bool) {
        self.add_with_version(peer_addr, update_last_seen, None)
    }

    /// Adds a peer to the list with version information.
    /// If a peer with the same IP already exists, it replaces the old one with the new port.
    pub fn add_with_version(&mut self, peer_addr: SocketAddr, update_last_seen: bool, version: Option<String>) {
        let now = Utc::now().timestamp() as u64;
        
        // First check for exact address match
        if let Some(existing_peer) = self.list.iter_mut().find(|p| p.addr == peer_addr) {
            if update_last_seen {
                existing_peer.last_seen = now;
            }
            if version.is_some() {
                existing_peer.version = version;
            }
            return;
        }
        
        // Check if we already have this IP with a different port
        if let Some(existing_peer_index) = self.list.iter().position(|p| p.addr.ip() == peer_addr.ip()) {
            let old_addr = self.list[existing_peer_index].addr;
            info!("Replacing peer {} with {} (same IP, different port)", old_addr, peer_addr);
            
            // Replace the existing peer with the new one
            self.list[existing_peer_index] = Peer {
                addr: peer_addr,
                last_seen: now,
                version: version.clone(),
            };
        } else {
            // No existing peer with this IP, add new one
            self.list.push(Peer {
                addr: peer_addr,
                last_seen: now,
                version: version.clone(),
            });
        }
        
        let peer_addrs: Vec<SocketAddr> = self.list.iter().map(|p| p.addr).collect();
        info!("Updated peer list with {}: total {} peers {:?}", peer_addr, self.list.len(), peer_addrs);
    }
}

/// UDP-based gossip manager
pub struct UdpGossipManager {
    socket: Arc<TokioUdpSocket>,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
    local_ip: std::net::IpAddr,
    local_port: u16,
    message_counter: AtomicU64,
    seen_messages: Arc<Mutex<HashMap<String, u64>>>, // "sender_addr:message_id" -> timestamp
}

impl UdpGossipManager {
    pub async fn new(
        bind_addr: SocketAddr,
        peer_list: Arc<RwLock<PeerList>>,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<Self> {
        let socket = TokioUdpSocket::bind(bind_addr).await?;
        let local_ip = get_local_ip()?;
        let local_port = bind_addr.port();
        
        info!("UDP Gossip bound to {} (local_ip: {}, port: {})", bind_addr, local_ip, local_port);
        
        Ok(Self {
            socket: Arc::new(socket),
            peer_list,
            stats,
            local_ip,
            local_port,
            message_counter: AtomicU64::new(0),
            seen_messages: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Check if the given address is this node (only same IP)
    fn is_self(&self, addr: SocketAddr) -> bool {
        // Only check if it's the same IP - different pods can use the same port
        if addr.ip() == self.local_ip {
            info!("Detected self via local IP: {} (local: {})", addr, self.local_ip);
            return true;
        }
        
        false
    }

    /// Generates a unique message ID
    fn next_message_id(&self) -> u64 {
        self.message_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Checks if we've already seen this message (for deduplication)
    async fn is_duplicate_message(&self, sender_addr: SocketAddr, message_id: u64) -> bool {
        let mut seen = self.seen_messages.lock().await;
        let now = Utc::now().timestamp() as u64;
        
        // Clean up old entries (older than 5 minutes)
        seen.retain(|_, timestamp| now - *timestamp < 300);
        
        // Create composite key: sender_addr:message_id
        let composite_key = format!("{}:{}", sender_addr, message_id);
        
        if seen.contains_key(&composite_key) {
            true
        } else {
            seen.insert(composite_key, now);
            false
        }
    }

    /// Sends a gossip message to a specific peer
    async fn send_gossip_to_peer(&self, peer_addr: SocketAddr, message: &GossipMessage) -> Result<()> {
        let serialized = serialize(message)?;
        
        if serialized.len() > MAX_UDP_PACKET_SIZE {
            return Err(anyhow::anyhow!("Gossip message too large: {} bytes", serialized.len()));
        }

        for attempt in 1..=UDP_RETRY_ATTEMPTS {
            match timeout(
                Duration::from_secs(UDP_TIMEOUT_SECS),
                self.socket.send_to(&serialized, peer_addr)
            ).await {
                Ok(Ok(_)) => {
                    // Update stats
                    let mut stats = self.stats.lock().await;
                    stats.packets_sent += 1;
                    return Ok(());
                }
                Ok(Err(e)) => {
                    if attempt == UDP_RETRY_ATTEMPTS {
                        warn!("Failed to send to {}: {}", peer_addr, e);
                    }
                }
                Err(_) => {
                    if attempt == UDP_RETRY_ATTEMPTS {
                        warn!("Timeout sending to {}", peer_addr);
                    }
                }
            }
            
            if attempt < UDP_RETRY_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        
        Err(anyhow::anyhow!("Failed to send gossip to {} after {} attempts", peer_addr, UDP_RETRY_ATTEMPTS))
    }

    /// Receives and processes incoming gossip messages
    async fn receive_gossip(&self) -> Result<()> {
        let mut buffer = vec![0u8; MAX_UDP_PACKET_SIZE];
        
        match self.socket.recv_from(&mut buffer).await {
            Ok((bytes_received, sender_addr)) => {
                // Update stats
                let mut stats = self.stats.lock().await;
                stats.packets_received += 1;
                drop(stats);

                // Try to deserialize the message
                match deserialize::<GossipMessage>(&buffer[..bytes_received]) {
                    Ok(gossip_msg) => {
                        // Check for duplicate messages
                        if self.is_duplicate_message(sender_addr, gossip_msg.message_id).await {
                            return Ok(());
                        }

                        // Log received peer IPs
                        let peer_ips: Vec<SocketAddr> = gossip_msg.peer_list.list.iter().map(|p| p.addr).collect();
                        
                        match gossip_msg.message_type {
                            MessageType::Request => {
                                info!("GOSSIP-REQ from {}: {} peers {:?}", sender_addr, peer_ips.len(), peer_ips);
                                
                                // Update our peer list
                                self.process_received_peer_list(&gossip_msg, sender_addr).await?;

                                // Send our peer list back as a response (only for requests!)
                                self.send_gossip_response(sender_addr).await?;
                            }
                            MessageType::Response => {
                                info!("GOSSIP-RESP from {}: {} peers {:?}", sender_addr, peer_ips.len(), peer_ips);
                                
                                // Update our peer list but don't send another response
                                self.process_received_peer_list(&gossip_msg, sender_addr).await?;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Invalid gossip message from {}: {}", sender_addr, e);
                    }
                }
            }
            Err(e) => {
                error!("UDP receive error: {}", e);
                return Err(e.into());
            }
        }
        
        Ok(())
    }

    /// Processes received peer list and updates our own
    async fn process_received_peer_list(&self, gossip_msg: &GossipMessage, sender_addr: SocketAddr) -> Result<()> {
        let mut local_list_guard = self.peer_list.write().await;
        
        // Add peers from the received list (excluding self)
        for peer in &gossip_msg.peer_list.list {
            if !self.is_self(peer.addr) {
                local_list_guard.add_with_version(peer.addr, false, peer.version.clone());
            } else {
                info!("Ignoring self peer {} from gossip message", peer.addr);
            }
        }
        
        // Add the sender (excluding self)
        if !self.is_self(sender_addr) {
            local_list_guard.add_with_version(sender_addr, true, Some(gossip_msg.sender_version.clone()));
        } else {
            info!("Ignoring self sender {} from gossip message", sender_addr);
        }
        
        Ok(())
    }

    /// Sends our peer list as a response to a gossip request
    async fn send_gossip_response(&self, recipient: SocketAddr) -> Result<()> {
        let gossip_response = {
            let list_guard = self.peer_list.read().await;
            
            // Filter out self from the peer list we're sending
            let non_self_peers: Vec<Peer> = list_guard.list.iter()
                .filter(|p| !self.is_self(p.addr))
                .cloned()
                .collect();
            
            let peer_addrs: Vec<SocketAddr> = non_self_peers.iter().map(|p| p.addr).collect();
            
            info!("GOSSIP-OUT (RESPONSE) to {}: {} peers {:?}", recipient, peer_addrs.len(), peer_addrs);
            
            GossipMessage {
                peer_list: PeerList {
                    list: non_self_peers,
                },
                sender_version: env!("CARGO_PKG_VERSION").to_string(),
                message_id: self.next_message_id(),
                timestamp: Utc::now().timestamp() as u64,
                message_type: MessageType::Response,
            }
        };

        self.send_gossip_to_peer(recipient, &gossip_response).await
    }

    /// Initiates gossip with random peers
    pub async fn initiate_gossip(&self) -> Result<()> {
        let (peers_to_contact, all_peers) = {
            let list_guard = self.peer_list.read().await;
            
            // Filter out self from the peer list first
            let non_self_peers: Vec<&Peer> = list_guard.list.iter()
                .filter(|p| !self.is_self(p.addr))
                .collect();
            
            let all_peers: Vec<SocketAddr> = non_self_peers.iter().map(|p| p.addr).collect();
            
            if non_self_peers.is_empty() {
                return Ok(());
            }

            // Randomly choose up to MAX_GOSSIP_PEERS from the non-self peers
            let selected_peers: Vec<SocketAddr> = non_self_peers
                .choose_multiple(&mut rand::rng(), MAX_GOSSIP_PEERS.min(non_self_peers.len()))
                .map(|p| p.addr)
                .collect();
            
            (selected_peers, all_peers)
        };

        if peers_to_contact.is_empty() {
            return Ok(());
        }

        let gossip_msg = {
            let list_guard = self.peer_list.read().await;
            
            // Filter out self from the peer list we're sending
            let non_self_peers: Vec<Peer> = list_guard.list.iter()
                .filter(|p| !self.is_self(p.addr))
                .cloned()
                .collect();
            
            GossipMessage {
                peer_list: PeerList {
                    list: non_self_peers,
                },
                sender_version: env!("CARGO_PKG_VERSION").to_string(),
                message_id: self.next_message_id(),
                timestamp: Utc::now().timestamp() as u64,
                message_type: MessageType::Request,
            }
        };

        info!("GOSSIP-CYCLE: contacting {:?} from known peers {:?}", peers_to_contact, all_peers);

        // Send gossip to all selected peers
        for peer_addr in &peers_to_contact {
            let _ = self.send_gossip_to_peer(*peer_addr, &gossip_msg).await;
        }

        Ok(())
    }

    /// Starts the gossip listener in the background
    pub async fn start_listener(&self) -> Result<()> {
        info!("UDP gossip listener started on port {}", GOSSIP_SERVER_PORT);
        
        loop {
            if let Err(e) = self.receive_gossip().await {
                error!("Gossip listener error: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Starts the UDP-based gossip service
pub async fn start_udp_gossip(
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], GOSSIP_SERVER_PORT));
    let gossip_manager = Arc::new(UdpGossipManager::new(bind_addr, peer_list.clone(), stats.clone()).await?);

    // Spawn background task for listening to incoming gossip
    let listener_manager = gossip_manager.clone();
    tokio::spawn(async move {
        if let Err(e) = listener_manager.start_listener().await {
            error!("Gossip listener failed: {}", e);
        }
    });

    // Spawn background task for periodic gossip initiation
    let initiator_manager = gossip_manager.clone();
    tokio::spawn(async move {
        start_udp_gossip_loop(initiator_manager, peer_list.clone()).await;
    });

    Ok(())
}

/// Periodic gossip loop for UDP
async fn start_udp_gossip_loop(
    gossip_manager: Arc<UdpGossipManager>,
    peer_list: Arc<RwLock<PeerList>>,
) {
    let mut interval = time::interval(Duration::from_secs(GOSSIP_INTERVAL_SECS));

    loop {
        interval.tick().await;
        
        // Prune inactive peers
        let _ = prune_inactive_peers(peer_list.clone()).await;

        // Initiate gossip with peers
        let _ = gossip_manager.initiate_gossip().await;
    }
}

/// Bootstrap from an entrypoint using UDP
pub async fn bootstrap_from_entrypoint_udp(
    entrypoint: Option<SocketAddr>,
    peer_list: Arc<RwLock<PeerList>>,
) -> Result<()> {
    match entrypoint {
        Some(addr) => {
            info!("Bootstrapping from entrypoint: {}", addr);
            
            let temp_socket = TokioUdpSocket::bind("0.0.0.0:0").await?;
            let local_ip = get_local_ip()?;
            
            // Add entrypoint to peer list immediately (before attempting communication)
            // This ensures it's available for future gossip cycles even if bootstrap fails
            if addr.ip() != local_ip {
                let mut peer_list_guard = peer_list.write().await;
                peer_list_guard.add_with_version(addr, true, None); 
                drop(peer_list_guard);
                info!("Added entrypoint {} to peer list for future gossip cycles", addr);
            } else {
                info!("Skipping entrypoint (self): {}", addr);
            }
            
            let bootstrap_msg = GossipMessage {
                peer_list: PeerList { list: Vec::new() },
                sender_version: env!("CARGO_PKG_VERSION").to_string(),
                message_id: 0,
                timestamp: Utc::now().timestamp() as u64,
                message_type: MessageType::Request,
            };

            let serialized = serialize(&bootstrap_msg)?;
            
            match timeout(
                Duration::from_secs(UDP_TIMEOUT_SECS),
                temp_socket.send_to(&serialized, addr)
            ).await {
                Ok(Ok(_)) => {
                    let mut buffer = vec![0u8; MAX_UDP_PACKET_SIZE];
                    match timeout(
                        Duration::from_secs(UDP_TIMEOUT_SECS * 2),
                        temp_socket.recv_from(&mut buffer)
                    ).await {
                        Ok(Ok((bytes_received, response_addr))) => {
                            if response_addr == addr {
                                match deserialize::<GossipMessage>(&buffer[..bytes_received]) {
                                    Ok(response_msg) => {
                                        let peer_addrs: Vec<SocketAddr> = response_msg.peer_list.list.iter().map(|p| p.addr).collect();
                                        info!("Bootstrap successful: received {} peers {:?}", peer_addrs.len(), peer_addrs);
                                        
                                        let mut peer_list_guard = peer_list.write().await;
                                        
                                        // Update entrypoint with version info (it was already added earlier)
                                        if addr.ip() != local_ip {
                                            peer_list_guard.add_with_version(addr, true, Some(response_msg.sender_version.clone()));
                                        }
                                        
                                        // Add peers from response (only check IP, not port)
                                        for peer in response_msg.peer_list.list {
                                            if peer.addr.ip() != local_ip {
                                                peer_list_guard.add_with_version(peer.addr, false, peer.version.clone());
                                            } else {
                                                info!("Ignoring self peer from bootstrap: {}", peer.addr);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        return Err(anyhow!("Bootstrap failed: invalid response from {}: {}", addr, e));
                                    }
                                }
                            } else {
                                return Err(anyhow!("Bootstrap failed: response from wrong address {} (expected {})", response_addr, addr));
                            }
                        }
                        Ok(Err(e)) => {
                            return Err(anyhow!("Bootstrap failed: receive error from {}: {}", addr, e));
                        }
                        Err(_) => {
                            return Err(anyhow!("Bootstrap failed: timeout waiting for response from {}", addr));
                        }
                    }
                }
                Ok(Err(e)) => {
                    return Err(anyhow!("Bootstrap failed: send error to {}: {}", addr, e));
                }
                Err(_) => {
                    return Err(anyhow!("Bootstrap failed: timeout sending to {}", addr));
                }
            }
        }
        None => {
            info!("No entrypoint provided, starting as seed node");
        }
    }
    Ok(())
}

/// Prunes inactive peers from the peer list
async fn prune_inactive_peers(peer_list: Arc<RwLock<PeerList>>) -> Result<()> {
    let now = Utc::now().timestamp() as u64;
    let mut guard = peer_list.write().await;
    let before = guard.list.len();

    guard
        .list
        .retain(|peer| now.saturating_sub(peer.last_seen) <= MAX_INACTIVITY_PERIOD_IN_SECS);

    let after = guard.list.len();
    let pruned_count = before - after;

    if pruned_count > 0 {
        let remaining_peers: Vec<SocketAddr> = guard.list.iter().map(|p| p.addr).collect();
        info!("Pruned {} inactive peers. {} remain: {:?}", pruned_count, after, remaining_peers);
    }

    Ok(())
}

// async fn accept_gossip_connections(
//     endpoint: Endpoint,
//     peer_list: Arc<RwLock<PeerList>>,
//     stats: Arc<Mutex<Stats>>,
//     local_ip: std::net::IpAddr,
// ) -> Result<()> {
//     info!("Gossip listener started on port {}", GOSSIP_SERVER_PORT);
//     while let Some(connecting) = endpoint.accept().await {
//         let peer_list_clone = peer_list.clone();
//         let stats_clone = stats.clone();

//         tokio::spawn(async move {
//             match connecting.await {
//                 Ok(connection) => {
//                     let peer_addr = connection.remote_address();
//                     trace!("Handling new gossip connection from {}", peer_addr);

//                     let timeout_duration = Duration::from_secs(30);
//                     if let Err(e) = timeout(
//                         timeout_duration,
//                         handle_incoming_gossip(connection, peer_list_clone, stats_clone, local_ip),
//                     )
//                     .await
//                     {
//                         warn!(
//                             "Gossip exchange with {} failed or timed out: {}",
//                             peer_addr, e
//                         );
//                     }
//                 }
//                 Err(e) => {
//                     // A single failed connection isn't a fatal error for the server.
//                     warn!("Failed to accept incoming connection: {}", e);
//                 }
//             }
//         });
//     }
//     Ok(())
// }

// async fn handle_incoming_gossip(
//     connection: Connection,
//     peer_list: Arc<RwLock<PeerList>>,
//     stats: Arc<Mutex<Stats>>,
//     local_ip: std::net::IpAddr,
// ) -> Result<()> {
//     let peer_addr = connection.remote_address();
//     info!("[GOSSIP-IN] Started exchange with {}", peer_addr);

//     let (mut send, mut recv) = connection.accept_bi().await?;

//     // Receive peer list from the remote peer
//     let packet = receive_packets(&mut recv, stats.clone()).await?;

//     if packet.meta.unwrap().op() != AtlasOperation::Gossip {
//         warn!(
//             "[GOSSIP-IN] Received invalid packet type from {}",
//             peer_addr
//         );
//         return Err(anyhow::anyhow!("Invalid Packet type for gossip"));
//     }

//     let gossip_msg: GossipMessage = deserialize(&packet.data)?;
//     debug!(
//         "[GOSSIP-IN] Received peer list with {} entries from {} (version: {})",
//         gossip_msg.peer_list.list.len(),
//         peer_addr,
//         gossip_msg.sender_version
//     );

//     // Update our own peer list with the received information
//     {
//         let mut local_list_guard = peer_list.write().await;
//         for peer in gossip_msg.peer_list.list {
//             // Don't add our own address to the peer list
//             if peer.addr.ip() != local_ip {
//                 local_list_guard.add_with_version(peer.addr, false, peer.version);
//             } else {
//                 debug!("Skipped adding local address to peer list: {}", peer.addr);
//             }
//         }
//         // Add the sender (should not be our own address in incoming gossip)
//         if peer_addr.ip() != local_ip {
//             local_list_guard.add_with_version(peer_addr, true, Some(gossip_msg.sender_version));
//         } else {
//             debug!("Skipped adding local sender address to peer list: {}", peer_addr);
//         }
//     }

//     // Prepare and send our peer list as a response
//     let gossip_response = {
//         let list_guard = peer_list.read().await;
//         GossipMessage {
//             peer_list: PeerList {
//                 list: list_guard.list.clone(),
//             },
//             sender_version: env!("CARGO_PKG_VERSION").to_string(),
//         }
//     };

//     debug!(
//         "[GOSSIP-IN] Sending own peer list ({} entries) to {} (our version: {})",
//         gossip_response.peer_list.list.len(),
//         peer_addr,
//         gossip_response.sender_version
//     );
//     let data_to_send = serialize(&gossip_response)?;
//     let response_packet = Packet::new(
//         0,
//         1, // A single response packet has 1 chunk
//         data_to_send.len() as u64,
//         AtlasOperation::Gossip as i32,
//         data_to_send,
//     );

//     send_packets(&mut send, response_packet, stats.clone()).await?;

//     info!(
//         "[GOSSIP-IN] Exchange with {} completed successfully.",
//         peer_addr
//     );
//     Ok(())
// }

// pub async fn bootstrap_from_entrypoint(
//     entrypoint: Option<SocketAddr>,
//     peer_list: Arc<RwLock<PeerList>>,
//     endpoint: Endpoint,
// ) -> Result<()> {
//     match entrypoint {
//         Some(addr) => {
//             info!("Attempting to bootstrap from entrypoint: {}", addr);
//             let connection = timeout(Duration::from_secs(10), try_connect(endpoint.clone(), addr))
//                 .await
//                 .map_err(|_| anyhow::anyhow!("Timed out connecting to entrypoint"))??;

//             info!(
//                 "Successfully bootstrapped from entrypoint: {}",
//                 connection.remote_address()
//             );
//             {
//                 let mut peer_list_guard = peer_list.write().await;
//                 // Add entrypoint without version info since we don't know it yet
//                 // Version will be updated during the first gossip exchange
//                 peer_list_guard.add(connection.remote_address(), true);
//             }
//         }
//         None => {
//             warn!("No entrypoint provided, starting as a seed node.");
//         }
//     }
//     Ok(())
// }

// async fn gossip(
//     peer_list: Arc<RwLock<PeerList>>,
//     connection: Connection,
//     stats: Arc<Mutex<Stats>>,
//     local_ip: std::net::IpAddr,
// ) -> Result<()> {
//     let peer_addr = connection.remote_address();
//     info!("[GOSSIP-OUT] Started exchange with {}", peer_addr);

//     let gossip_msg = {
//         let list_guard = peer_list.read().await;
//         GossipMessage {
//             peer_list: PeerList {
//                 list: list_guard.list.clone(),
//             },
//             sender_version: env!("CARGO_PKG_VERSION").to_string(),
//         }
//     };
//     let data = serialize(&gossip_msg)?;

//     let packet = Packet::new(
//         0,
//         1, // A single request packet has 1 chunk
//         data.len() as u64,
//         AtlasOperation::Gossip as i32,
//         data,
//     );

//     debug!(
//         "[GOSSIP-OUT] Sending peer list ({} entries) to {} (our version: {})",
//         gossip_msg.peer_list.list.len(),
//         peer_addr,
//         gossip_msg.sender_version
//     );
//     let response_packet = send_and_receive_packets(connection.clone(), packet, stats).await?;

//     if response_packet.meta.unwrap().op() != AtlasOperation::Gossip {
//         warn!(
//             "[GOSSIP-OUT] Received invalid packet type from {}",
//             peer_addr
//         );
//         return Err(anyhow::anyhow!("Invalid Packet type in gossip response"));
//     }

//     let received_gossip_msg: GossipMessage = deserialize(&response_packet.data)?;
//     debug!(
//         "[GOSSIP-OUT] Received peer list with {} entries from {} (version: {})",
//         received_gossip_msg.peer_list.list.len(),
//         peer_addr,
//         received_gossip_msg.sender_version
//     );

//     {
//         let mut local_list_guard = peer_list.write().await;
//         for peer in received_gossip_msg.peer_list.list {
//             // Don't add our own address to the peer list
//             if peer.addr.ip() != local_ip {
//                 local_list_guard.add_with_version(peer.addr, false, peer.version);
//             } else {
//                 debug!("Skipped adding local address to peer list: {}", peer.addr);
//             }
//         }
//         // Add the sender (should not be our own address in outgoing gossip)
//         if peer_addr.ip() != local_ip {
//             local_list_guard.add_with_version(peer_addr, true, Some(received_gossip_msg.sender_version));
//         } else {
//             debug!("Skipped adding local sender address to peer list: {}", peer_addr);
//         }
//     }

//     info!(
//         "[GOSSIP-OUT] Exchange with {} completed successfully.",
//         peer_addr
//     );
//     Ok(())
// }

// async fn start_gossip_loop(
//     endpoint: Endpoint,
//     peer_list: Arc<RwLock<PeerList>>,
//     stats: Arc<Mutex<Stats>>,
//     local_ip: std::net::IpAddr,
// ) {
//     info!(
//         "Starting periodic gossip loop (interval: {}s)",
//         GOSSIP_INTERVAL_SECS
//     );
//     let mut interval = time::interval(Duration::from_secs(GOSSIP_INTERVAL_SECS));

//     loop {
//         interval.tick().await;
//         trace!("Gossip loop ticked.");
//         prune_inactive_peers(peer_list.clone()).await.ok();

//         // let peers_to_contact: Vec<SocketAddr> = {
//         //     let list_guard = peer_list.read().await;
//         //     list_guard.list.iter().map(|p| p.addr).take(MAX_GOSSIP_PEERS).collect()
//         // };

//         let peers_to_contact: Vec<SocketAddr> = {
//             let list_guard = peer_list.read().await;

//             // Randomly choose up to MAX_GOSSIP_PEERS from the list.
//             let selected_peers: Vec<SocketAddr> = list_guard
//                 .list
//                 .choose_multiple(&mut rand::rng(), MAX_GOSSIP_PEERS)
//                 .map(|p| p.addr) // Get the address from each chosen peer.
//                 .collect();
            
//             // Filter out peers with same IP as us
//             let filtered_peers: Vec<SocketAddr> = selected_peers
//                 .into_iter()
//                 .filter(|&addr| {
//                     if addr.ip() == local_ip {
//                         debug!("Filtered out local IP from gossip targets: {}", addr);
//                         false
//                     } else {
//                         true
//                     }
//                 })
//                 .collect();
            
//             filtered_peers
//         };

//         if peers_to_contact.is_empty() {
//             debug!("Skipping gossip cycle: No known peers to contact.");
//             continue;
//         }

//         info!(
//             "Starting new gossip cycle with {} peer(s): {:?}",
//             peers_to_contact.len(),
//             peers_to_contact
//         );

//         for peer_addr in peers_to_contact {
//             let endpoint_clone = endpoint.clone();
//             let peer_list_clone = peer_list.clone();
//             let stats_clone = stats.clone();

//             tokio::spawn(async move {
//                 if let Err(e) =
//                     gossip_to_peer(endpoint_clone, peer_list_clone, stats_clone, peer_addr, local_ip).await
//                 {
//                     warn!("Gossip with peer {} failed: {}", peer_addr, e);
//                 }
//             });
//         }
//     }
// }

// async fn gossip_to_peer(
//     endpoint: Endpoint,
//     peer_list: Arc<RwLock<PeerList>>,
//     stats: Arc<Mutex<Stats>>,
//     peer: SocketAddr,
//     local_ip: std::net::IpAddr,
// ) -> Result<()> {
//     trace!("Attempting to gossip with peer: {}", peer);
//     let connection = try_connect_with_retries(endpoint, peer, MAX_CONNECTION_RETRIES).await?;
//     gossip(peer_list, connection, stats, local_ip).await
// }

// async fn prune_inactive_peers(peer_list: Arc<RwLock<PeerList>>) -> Result<()> {
//     let now = Utc::now().timestamp() as u64;
//     let mut guard = peer_list.write().await;
//     let before = guard.list.len();

//     guard
//         .list
//         .retain(|peer| now.saturating_sub(peer.last_seen) <= MAX_INACTIVITY_PERIOD_IN_SECS);

//     let after = guard.list.len();
//     let pruned_count = before - after;

//     if pruned_count > 0 {
//         // This is an important maintenance event.
//         info!(
//             "Pruned {} inactive peer(s). {} peers remain.",
//             pruned_count, after
//         );
//     }

//     Ok(())
// }
