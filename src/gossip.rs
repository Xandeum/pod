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
    client::{
        configure_gossip_client, configure_server, receive_packets, send_and_receive_packets,
        send_packets, try_connect, try_connect_with_retries,
    },
    packet::{AtlasOperation, Packet},
    stats::Stats,
};

const MAX_CONNECTION_RETRIES: u8 = 3;
const GOSSIP_INTERVAL_SECS: u64 = 120;
const MAX_GOSSIP_PEERS: usize = 3;
const MAX_INACTIVITY_PERIOD_IN_SECS: u64 = 60 * 60;
pub const GOSSIP_PORT: u16 = 9000;
pub const GOSSIP_SERVER_PORT: u16 = 9001;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub addr: SocketAddr,
    pub last_seen: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerList {
    pub list: Vec<Peer>,
}

impl PeerList {
    pub fn new() -> Self {
        Self { list: Vec::new() }
    }

    /// Adds a peer to the list if it doesn't already exist.
    pub fn add(&mut self, peer_addr: SocketAddr, update_last_seen: bool) {
        if let Some(existing_peer) = self.list.iter_mut().find(|p| p.addr == peer_addr) {
            if update_last_seen {
                existing_peer.last_seen = Utc::now().timestamp() as u64;
            }
        } else {
            self.list.push(Peer {
                addr: peer_addr,
                last_seen: Utc::now().timestamp() as u64,
            });
            // This is a key event, so info! is perfect.
            info!(
                "Added new peer: {}. Total peers: {}",
                peer_addr,
                self.list.len()
            );
        }
    }
}

pub async fn start_gossip(
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
    client_endpoint: Endpoint,
) -> Result<()> {
    info!("Initializing gossip service...");

    let server_config = configure_server()?;

    let mut server_endpoint =
        Endpoint::client(SocketAddr::from(([0, 0, 0, 0], GOSSIP_SERVER_PORT)))?;

    server_endpoint.set_server_config(Some(server_config));

    // Spawn background tasks for listening and initiating gossip
    tokio::spawn(accept_gossip_connections(
        server_endpoint.clone(),
        peer_list.clone(),
        stats.clone(),
    ));
    tokio::spawn(start_gossip_loop(client_endpoint, peer_list, stats));

    Ok(())
}

async fn accept_gossip_connections(
    endpoint: Endpoint,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    info!("Gossip listener started on port {}", GOSSIP_SERVER_PORT);
    while let Some(connecting) = endpoint.accept().await {
        let peer_list_clone = peer_list.clone();
        let stats_clone = stats.clone();

        tokio::spawn(async move {
            match connecting.await {
                Ok(connection) => {
                    let peer_addr = connection.remote_address();
                    trace!("Handling new gossip connection from {}", peer_addr);

                    let timeout_duration = Duration::from_secs(30);
                    if let Err(e) = timeout(
                        timeout_duration,
                        handle_incoming_gossip(connection, peer_list_clone, stats_clone),
                    )
                    .await
                    {
                        warn!(
                            "Gossip exchange with {} failed or timed out: {}",
                            peer_addr, e
                        );
                    }
                }
                Err(e) => {
                    // A single failed connection isn't a fatal error for the server.
                    warn!("Failed to accept incoming connection: {}", e);
                }
            }
        });
    }
    Ok(())
}

async fn handle_incoming_gossip(
    connection: Connection,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let peer_addr = connection.remote_address();
    info!("[GOSSIP-IN] Started exchange with {}", peer_addr);

    let (mut send, mut recv) = connection.accept_bi().await?;

    // Receive peer list from the remote peer
    let packet = receive_packets(&mut recv, stats.clone()).await?;

    if packet.meta.unwrap().op() != AtlasOperation::Gossip {
        warn!(
            "[GOSSIP-IN] Received invalid packet type from {}",
            peer_addr
        );
        return Err(anyhow::anyhow!("Invalid Packet type for gossip"));
    }

    let list: PeerList = deserialize(&packet.data)?;
    debug!(
        "[GOSSIP-IN] Received peer list with {} entries from {}",
        list.list.len(),
        peer_addr
    );

    // Update our own peer list with the received information
    let received_list: PeerList = deserialize(&packet.data)?; // Original logic preserved
    {
        let mut local_list_guard = peer_list.write().await;
        for peer in received_list.list {
            local_list_guard.add(peer.addr, false);
        }
        local_list_guard.add(peer_addr, true);
    }

    // Prepare and send our peer list as a response
    let list_to_send = {
        let list_guard = peer_list.read().await;
        list_guard.list.clone()
    };

    debug!(
        "[GOSSIP-IN] Sending own peer list ({} entries) to {}",
        list_to_send.len(),
        peer_addr
    );
    let data_to_send = serialize(&list_to_send)?;
    let response_packet = Packet::new(
        0,
        1, // A single response packet has 1 chunk
        data_to_send.len() as u64,
        AtlasOperation::Gossip as i32,
        data_to_send,
    );

    send_packets(&mut send, response_packet, stats.clone()).await?;

    info!(
        "[GOSSIP-IN] Exchange with {} completed successfully.",
        peer_addr
    );
    Ok(())
}

pub async fn bootstrap_from_entrypoint(
    entrypoint: Option<SocketAddr>,
    peer_list: Arc<RwLock<PeerList>>,
    endpoint: Endpoint,
) -> Result<()> {
    match entrypoint {
        Some(addr) => {
            info!("Attempting to bootstrap from entrypoint: {}", addr);
            let connection = timeout(Duration::from_secs(10), try_connect(endpoint.clone(), addr))
                .await
                .map_err(|_| anyhow::anyhow!("Timed out connecting to entrypoint"))??;

            info!(
                "Successfully bootstrapped from entrypoint: {}",
                connection.remote_address()
            );
            {
                let mut peer_list_guard = peer_list.write().await;
                peer_list_guard.add(connection.remote_address(), true);
            }
        }
        None => {
            warn!("No entrypoint provided, starting as a seed node.");
        }
    }
    Ok(())
}

async fn gossip(
    peer_list: Arc<RwLock<PeerList>>,
    connection: Connection,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let peer_addr = connection.remote_address();
    info!("[GOSSIP-OUT] Started exchange with {}", peer_addr);

    let list_guard = peer_list.read().await;
    let peers_to_send = list_guard.list.clone();
    let data = serialize(&peers_to_send)?;

    let packet = Packet::new(
        0,
        1, // A single request packet has 1 chunk
        data.len() as u64,
        AtlasOperation::Gossip as i32,
        data,
    );

    debug!(
        "[GOSSIP-OUT] Sending peer list ({} entries) to {}",
        peers_to_send.len(),
        peer_addr
    );
    let response_packet = send_and_receive_packets(connection.clone(), packet, stats).await?;

    if response_packet.meta.unwrap().op() != AtlasOperation::Gossip {
        warn!(
            "[GOSSIP-OUT] Received invalid packet type from {}",
            peer_addr
        );
        return Err(anyhow::anyhow!("Invalid Packet type in gossip response"));
    }

    let received_list: PeerList = deserialize(&response_packet.data)?;
    debug!(
        "[GOSSIP-OUT] Received peer list with {} entries from {}",
        received_list.list.len(),
        peer_addr
    );

    {
        let mut local_list_guard = peer_list.write().await;
        for peer in received_list.list {
            local_list_guard.add(peer.addr, false);
        }
        local_list_guard.add(peer_addr, true);
    }

    info!(
        "[GOSSIP-OUT] Exchange with {} completed successfully.",
        peer_addr
    );
    Ok(())
}

async fn start_gossip_loop(
    endpoint: Endpoint,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
) {
    info!(
        "Starting periodic gossip loop (interval: {}s)",
        GOSSIP_INTERVAL_SECS
    );
    let mut interval = time::interval(Duration::from_secs(GOSSIP_INTERVAL_SECS));

    loop {
        interval.tick().await;
        trace!("Gossip loop ticked.");
        prune_inactive_peers(peer_list.clone()).await.ok();

        // let peers_to_contact: Vec<SocketAddr> = {
        //     let list_guard = peer_list.read().await;
        //     list_guard.list.iter().map(|p| p.addr).take(MAX_GOSSIP_PEERS).collect()
        // };

        let peers_to_contact: Vec<SocketAddr> = {
            let list_guard = peer_list.read().await;

            // Randomly choose up to MAX_GOSSIP_PEERS from the list.
            list_guard
                .list
                .choose_multiple(&mut rand::rng(), MAX_GOSSIP_PEERS)
                .map(|p| p.addr) // Get the address from each chosen peer.
                .collect()
        };

        if peers_to_contact.is_empty() {
            debug!("Skipping gossip cycle: No known peers to contact.");
            continue;
        }

        info!(
            "Starting new gossip cycle with {} peer(s): {:?}",
            peers_to_contact.len(),
            peers_to_contact
        );

        for peer_addr in peers_to_contact {
            let endpoint_clone = endpoint.clone();
            let peer_list_clone = peer_list.clone();
            let stats_clone = stats.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    gossip_to_peer(endpoint_clone, peer_list_clone, stats_clone, peer_addr).await
                {
                    warn!("Gossip with peer {} failed: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn gossip_to_peer(
    endpoint: Endpoint,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
    peer: SocketAddr,
) -> Result<()> {
    trace!("Attempting to gossip with peer: {}", peer);
    let connection = try_connect_with_retries(endpoint, peer, MAX_CONNECTION_RETRIES).await?;
    gossip(peer_list, connection, stats).await
}

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
        // This is an important maintenance event.
        info!(
            "Pruned {} inactive peer(s). {} peers remain.",
            pruned_count, after
        );
    }

    Ok(())
}