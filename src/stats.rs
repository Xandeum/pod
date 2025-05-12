use std::{sync::Arc, time::Duration};

use serde::Serialize;
use sysinfo::System;
use tokio::{sync::Mutex, time::Instant};

use crate::storage::Metadata;

#[derive(Serialize, Clone)]
pub struct Stats {
    pub cpu_percent: f32,
    pub ram_used: u64,
    pub ram_total: u64,
    pub uptime: u64,
    pub packets_sent: u64,
    pub packets_received: u64,
}

#[derive(Clone)]
pub struct AppState {
    pub meta: Arc<Mutex<Metadata>>,
    pub stats: Arc<Mutex<Stats>>,
}

#[derive(Serialize)]
pub struct CombinedStats {
    #[serde(flatten)]
    pub metadata: Metadata,
    #[serde(flatten)]
    pub stats: Stats,
    pub file_size: u64,
}

pub async fn update_system_stats(stats: Arc<Mutex<Stats>>) {
    let mut sys = System::new_all();
    let start_time = Instant::now();
    let mut last_packets_sent = 0;
    let mut last_packets_received = 0;
    let mut first_iteration = true;

    loop {
        sys.refresh_all();
        let mut networks = sysinfo::Networks::new_with_refreshed_list();
        let mut stats = stats.lock().await;

        // let mut packets_sent = 0;
        // let mut packets_received = 0;
        // for (_interface, network) in &networks {
        //     packets_sent += network.packets_transmitted();
        //     packets_received += network.packets_received();
        // }

        // if first_iteration {
        //     stats.packets_sent = 0;
        //     stats.packets_received = 0;
        //     first_iteration = false;
        // } else {
        //     stats.packets_sent = packets_sent.saturating_sub(last_packets_sent);
        //     stats.packets_received = packets_received.saturating_sub(last_packets_received);
        // }
        // last_packets_sent = packets_sent;
        // last_packets_received = packets_received;

        stats.cpu_percent = sys.global_cpu_usage();
        stats.ram_used = sys.used_memory();
        stats.ram_total = sys.total_memory();
        stats.uptime = start_time.elapsed().as_secs();

        drop(stats);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
