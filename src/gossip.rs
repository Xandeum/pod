use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use bincode::{deserialize, serialize};
use chrono::Utc;
use log::{debug, error, info, trace, warn};
use quinn::{Connection, Endpoint, RecvStream, SendStream, VarInt};
use rand::seq::IndexedRandom;
// Added RecvStream
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, RwLock},
    time::{self, timeout},
};

use crate::{
    // client::{
    //     configure_gossip_client, configure_server, receive_packets, send_and_receive_packets,
    //     send_packets, try_connect, try_connect_with_retries,
    // },
    packet::{AtlasOperation, Packet},
    stats::Stats,
};

const MAX_CONNECTION_RETRIES: u8 = 3;
const GOSSIP_INTERVAL_SECS: u64 = 120;
const MAX_GOSSIP_PEERS: usize = 3;
const MAX_INACTIVITY_PERIOD_IN_SECS: u64 = 60 * 60;
pub const GOSSIP_PORT: u16 = 9000;
pub const GOSSIP_SERVER_PORT: u16 = 9001;

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

// #[derive(Debug, Clone, Serialize, Deserialize)]
// pub struct GossipMessage {
//     pub peer_list: PeerList,
//     pub sender_version: String,
// }

// impl PeerList {
//     pub fn new() -> Self {
//         Self { list: Vec::new() }
//     }

//     /// Adds a peer to the list if it doesn't already exist.
//     pub fn add(&mut self, peer_addr: SocketAddr, update_last_seen: bool) {
//         self.add_with_version(peer_addr, update_last_seen, None)
//     }

//     /// Adds a peer to the list with version information.
//     pub fn add_with_version(&mut self, peer_addr: SocketAddr, update_last_seen: bool, version: Option<String>) {
//         if let Some(existing_peer) = self.list.iter_mut().find(|p| p.addr == peer_addr) {
//             if update_last_seen {
//                 existing_peer.last_seen = Utc::now().timestamp() as u64;
//             }
//             // Update version if provided
//             if version.is_some() {
//                 existing_peer.version = version;
//             }
//         } else {
//             self.list.push(Peer {
//                 addr: peer_addr,
//                 last_seen: Utc::now().timestamp() as u64,
//                 version: version.clone(),
//             });
//             // This is a key event, so info! is perfect.
//             let version_str = version.as_deref().unwrap_or("unknown");
//             info!(
//                 "Added new peer: {} (version: {}). Total peers: {}",
//                 peer_addr,
//                 version_str,
//                 self.list.len()
//             );
//         }
//     }
// }

// pub async fn start_gossip(
//     peer_list: Arc<RwLock<PeerList>>,
//     stats: Arc<Mutex<Stats>>,
//     client_endpoint: Endpoint,
// ) -> Result<()> {
//     info!("Initializing gossip service...");

//     let server_config = configure_server()?;

//     let mut server_endpoint =
//         Endpoint::client(SocketAddr::from(([0, 0, 0, 0], GOSSIP_SERVER_PORT)))?;

//     server_endpoint.set_server_config(Some(server_config));

//     // Get our local IP address to filter out self-gossip
//     let local_ip = get_local_ip()?;
//     info!("Local IP detected: {}, will filter out peers with same IP", local_ip);

//     // Spawn background tasks for listening and initiating gossip
//     tokio::spawn(accept_gossip_connections(
//         server_endpoint.clone(),
//         peer_list.clone(),
//         stats.clone(),
//         local_ip,
//     ));
//     tokio::spawn(start_gossip_loop(client_endpoint, peer_list, stats, local_ip));

//     Ok(())
// }

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
