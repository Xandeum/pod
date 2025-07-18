use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use bincode::{deserialize, serialize};
use chrono::Utc;
use log::{error, info, warn};
use quinn::{Connection, Endpoint, SendStream, VarInt};
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
const GOSSIP_INTERVAL_SECS: u64 = 30;
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
            // New peer, add it to the list
            self.list.push(Peer {
                addr: peer_addr,
                last_seen: Utc::now().timestamp() as u64,
            });
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
    // server_endpoint: Endpoint,
) -> Result<()> {
    // ------------------------ Connect to Entrypoint -----------------------

    let client_config = configure_server()?;

    // let addr = SocketAddr::from_str(&entrypoint)?;

    let mut server_endpoint =
        Endpoint::client(SocketAddr::from(([0, 0, 0, 0], GOSSIP_SERVER_PORT)))?;

    server_endpoint.set_server_config(Some(client_config));

    // // let connection = endpoint.connect(addr, "xandeum-pod")?.await?;
    // let connection = try_connect(endpoint.clone(), addr).await?;

    // info!("connection : {:?}", connection);
    // {
    //     let mut peer_list_guard = peer_list.write().await;
    //     peer_list_guard.add(connection.remote_address(), true);
    // }

    // let _ = bootstrap_from_entrypoint(entrypoint, peer_list.clone(), endpoint.clone()).await?;
    // ------------------------- Start Gossip -----------------------

    tokio::spawn(accept_gossip_connections(
        server_endpoint.clone(),
        peer_list.clone(),
        stats.clone(),
    ));
    tokio::spawn(start_gossip_loop(client_endpoint, peer_list, stats));

    // let _ = accept_gossip_connections(endpoint.clone(), peer_list.clone()).await?;

    // let _ = start_gossip_loop(endpoint, peer_list, stats);

    Ok(())
}

async fn accept_gossip_connections(
    endpoint: Endpoint,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    while let Some(connecting) = endpoint.accept().await {
        let peer_list_clone = peer_list.clone();
        let stats_clone = stats.clone();

        tokio::spawn(async move {
            match connecting.await {
                Ok(connection) => {
                    let client_address = connection.remote_address();
                    info!("Accepted new connection from {}", client_address);

                    let timeout_duration = Duration::from_secs(30);
                    let result = timeout(
                        timeout_duration,
                        handle_incoming_gossip(connection, peer_list_clone, stats_clone),
                    )
                    .await;

                    match result {
                        Err(_) => {
                            error!(
                                "Timeout: Did not complete gossip with {} within {}s.",
                                client_address, 30
                            );
                        }
                        Ok(Err(e)) => {
                            error!(
                                "Failed to handle incoming gossip from {}: {}",
                                client_address, e
                            );
                        }
                        Ok(Ok(_)) => {}
                    }
                }
                Err(e) => {
                    log::error!("Failed to accept connection: {}", e);
                }
            }
        });
    }
    Ok(())
}

async fn handle_incoming_gossip(
    connection: Connection,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>, // Assuming you might want to track stats here too
) -> Result<()> {
    info!(
        "Handling incoming gossip from {}",
        connection.remote_address()
    );

    // 1. Accept a bidirectional stream from the peer who connected to us.
    let (mut send, mut recv) = connection.accept_bi().await?;

    let packet = receive_packets(Arc::new(Mutex::new(recv)), stats.clone()).await?;

    if packet.meta.unwrap().op() != AtlasOperation::Gossip {
        error!("Invalid Packet type");
    }

    let list: PeerList = deserialize(&packet.data)?;
    {
        let mut local_list_guard = peer_list.write().await;

        for peer in list.list.clone() {
            local_list_guard.add(peer.addr, false);
        }
        local_list_guard.add(connection.remote_address(), true);
    }

    let list_to_send = {
        let list_guard = peer_list.read().await;
        list_guard.list.clone()
    };

    info!(
        "Received peer list of size {} from {}",
        list.list.clone().len(),
        connection.remote_address()
    );

    let data_to_send = serialize(&list_to_send)?;

    let packet = Packet::new(
        0,
        0,
        data_to_send.len() as u64,
        AtlasOperation::Gossip as i32,
        data_to_send,
    );

    send_packets(Arc::new(Mutex::new(send)), packet, stats.clone()).await?;

    let mut local_list_guard = peer_list.write().await;
    for peer in list.list {
        local_list_guard.add(peer.addr, false);
    }
    local_list_guard.add(connection.remote_address(), true);

    Ok(())
}

async fn share_peer_list(
    peer_list: Arc<RwLock<PeerList>>,
    sender_stream: Arc<tokio::sync::Mutex<SendStream>>,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let list_guard = peer_list.read().await;

    let peers = list_guard.list.clone();
    let data = serialize(&peers)?;

    let packet = Packet::new(0, 0, data.len() as u64, AtlasOperation::Gossip as i32, data);

    let _ = send_packets(sender_stream, packet, stats).await?;

    Ok(())
}

pub async fn bootstrap_from_entrypoint(
    entrypoint: Option<SocketAddr>,
    peer_list: Arc<RwLock<PeerList>>,
    endpoint: Endpoint,
) -> Result<()> {
    match entrypoint {
        Some(addr) => {
            info!("Trying to connect to entrypoint : {:?}", entrypoint);
            let connection = timeout(Duration::from_secs(10), try_connect(endpoint.clone(), addr))
                .await
                .map_err(|_| anyhow::anyhow!("Timed out trying to connect to entrypoint"))??;

            info!("connection : {:?}", connection);
            {
                let mut peer_list_guard = peer_list.write().await;
                peer_list_guard.add(connection.remote_address(), true);
            }
        }
        None => {
            warn!("No Entry point provided")
        }
    }

    Ok(())
}

async fn gossip(
    peer_list: Arc<RwLock<PeerList>>,
    connection: Connection,
    stats: Arc<Mutex<Stats>>,
) -> Result<()> {
    let list_guard = peer_list.read().await;

    let peers = list_guard.list.clone();
    let data = serialize(&peers)?;

    let packet = Packet::new(0, 0, data.len() as u64, AtlasOperation::Gossip as i32, data);

    let packet = send_and_receive_packets(connection.clone(), packet, stats).await?;

    if packet.meta.unwrap().op() != AtlasOperation::Gossip {
        error!("Invalid Packet type");
    }

    let list: PeerList = deserialize(&packet.data)?;
    {
        let mut local_list_guard = peer_list.write().await;

        for peer in list.list {
            local_list_guard.add(peer.addr, false);
        }
        local_list_guard.add(connection.remote_address(), true);
    }
    Ok(())
}

async fn start_gossip_loop(
    endpoint: Endpoint,
    peer_list: Arc<RwLock<PeerList>>,
    stats: Arc<Mutex<Stats>>,
) {
    info!("Gossip loop started.");
    // Create an interval that ticks every 30 seconds
    let mut interval = time::interval(Duration::from_secs(GOSSIP_INTERVAL_SECS));

    let mut gossip_list: Vec<SocketAddr>;

    {
        gossip_list = peer_list
            .read()
            .await
            .list
            .iter()
            .map(|p| p.addr)
            .collect::<Vec<_>>();
    }

    loop {
        interval.tick().await;
        let _ = prune_inactive_peers(peer_list.clone()).await;

        {
            // Syncing With latest peers

            let current_peers = peer_list.read().await;
            for peer in &current_peers.list {
                if !gossip_list.contains(&peer.addr) {
                    gossip_list.push(peer.addr);
                }
            }
        }

        let num_to_contact = MAX_GOSSIP_PEERS.min(gossip_list.len());
        let peers_to_contact: Vec<SocketAddr> = gossip_list.drain(0..num_to_contact).collect();
        {
            // let list_guard = peer_list.read().await;

            // // Randomly select up to 3 peers from list
            // peers_to_contact = list_guard
            //     .list
            //     .iter()
            //     .map(|p| p.addr)
            //     .collect::<Vec<_>>()
            //     .choose_multiple(&mut rand::rng(), MAX_GOSSIP_PEERS)
            //     .cloned()
            //     .collect();
        }

        if peers_to_contact.is_empty() {
            info!("No peers to gossip with yet, skipping this cycle.");
            continue;
        }

        info!("Gossiping with peers: {:?}", peers_to_contact);

        for peer_addr in peers_to_contact {
            let endpoint_clone = endpoint.clone();
            let peer_list_clone = peer_list.clone();
            let stats_clone = stats.clone();

            tokio::spawn(async move {
                if let Err(e) = gossip_to_peer(
                    endpoint_clone.clone(),
                    peer_list_clone,
                    stats_clone,
                    peer_addr,
                )
                .await
                {
                    error!("Gossip with peer {} failed: {}", peer_addr, e);
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
    let connection = try_connect_with_retries(endpoint, peer, MAX_CONNECTION_RETRIES).await?;

    let _ = gossip(peer_list, connection, stats).await?;

    Ok(())
}

async fn prune_inactive_peers(peer_list: Arc<RwLock<PeerList>>) -> Result<()> {
    let now = Utc::now().timestamp() as u64;

    let mut guard = peer_list.write().await;

    let before = guard.list.len();
    guard
        .list
        .retain(|peer| now.saturating_sub(peer.last_seen) <= MAX_INACTIVITY_PERIOD_IN_SECS);
    let after = guard.list.len();

    if before != after {
        log::info!(
            "Pruned {} inactive peers (>{}s inactive). {} peers remain.",
            before - after,
            MAX_INACTIVITY_PERIOD_IN_SECS,
            after
        );
    }

    Ok(())
}
