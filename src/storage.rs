use anyhow::Result;
use bincode::{deserialize, serialize};
use chrono::Utc;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{collections::HashMap, io::Error};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::Mutex,
};

use crate::packet::Packet;

const FILE_PATH: &str = "xandeum-pod";
pub const PAGE_SIZE: usize = 1048576;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Metadata {
    pub current_index: u64,
    pub total_pages: u64,
    pub last_updated: u64,
    pub total_bytes: u64,
}

impl Metadata {
    fn new() -> Self {
        Metadata {
            current_index: 0,
            total_pages: 0,
            last_updated: 0,
            total_bytes: 0,
        }
    }

    fn size() -> u64 {
        const METADATA_SIZE: u64 = 42;
        bincode::serialized_size(&Metadata::new()).unwrap_or(METADATA_SIZE)
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serialize(self)?;
        let metadata_size = Metadata::size() as usize;
        if bytes.len() > metadata_size {
            info!("bytes : {}", bytes.len());
            return Err(anyhow::anyhow!("Metadata exceeds {} bytes", metadata_size));
        }
        bytes.resize(metadata_size as usize, 0);
        Ok(bytes)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let metadata = deserialize(&bytes)?;
        Ok(metadata)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Index {
    pub index: HashMap<u64, u64>,
}

impl Index {
    fn new() -> Self {
        Index {
            index: HashMap::with_capacity(200),
        }
    }

    pub fn size() -> u64 {
        bincode::serialized_size(&Index::new()).unwrap_or(3200)
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serialize(self)?;
        let index_size = Index::size() as usize;
        if bytes.len() > index_size {
            info!("bytes : {}", bytes.len());
            return Err(anyhow::anyhow!("Index exceeds {} bytes", index_size));
        }
        bytes.resize(index_size as usize, 0);
        Ok(bytes)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let index = deserialize(&bytes)?;
        Ok(index)
    }
}

#[derive(Clone)]
pub struct StorageState {
    pub file: Arc<Mutex<File>>,
    pub metadata: Arc<Mutex<Metadata>>,
    pub index: Arc<Mutex<Index>>,
}

impl StorageState {
    pub async fn get_or_create_state() -> Result<Self> {
        info!("path : {:?}", FILE_PATH);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(FILE_PATH)
            .await?;

        let mut metadata_bytes = vec![0u8; Metadata::size() as usize];
        let mut index_bytes = vec![0u8; Index::size() as usize];

        file.seek(SeekFrom::Start(0)).await?;
        file.read_exact(&mut metadata_bytes).await?;

        file.seek(SeekFrom::Start(metadata_bytes.len() as u64))
            .await?;
        file.read_exact(&mut index_bytes).await?;

        let metadata = if metadata_bytes.iter().all(|f| *f == 0) {
            let metadata = Metadata::new();
            file.seek(SeekFrom::Start(0)).await?;
            file.write_all(&metadata.to_bytes()?).await?;
            metadata
        } else {
            Metadata::from_bytes(&metadata_bytes)?
        };

        let index = if index_bytes.iter().all(|f| *f == 0) {
            let index = Index::new();
            file.seek(SeekFrom::Start(Metadata::size())).await?;
            file.write_all(&index.to_bytes()?).await?;
            index
        } else {
            Index::from_bytes(&index_bytes)?
        };

        Ok(StorageState {
            file: Arc::new(Mutex::new(file)),
            metadata: Arc::new(Mutex::new(metadata)),
            index: Arc::new(Mutex::new(index)),
        })
    }

    // pub async fn write_page(&self, data: &[u8]) -> Result<()> {
    //     if data.len() != PAGE_SIZE {
    //         info!("data length is not equal page size, Skipping");
    //         return Ok(());
    //     }

    //     let mut metadata = self.metadata.lock().await;
    //     let page_index = metadata.current_index;
    //     let offset = Metadata::size() + page_index * PAGE_SIZE as u64;

    //     if offset + PAGE_SIZE as u64 > 107_374_182_400 {
    //         info!("Storage Out of capacity!, Skipping");
    //         return Ok(());
    //     }

    //     if let Err(e) = self.write(offset, data).await {
    //         info!("error during writing file : {:?}, Skipping", e);
    //     }

    //     metadata.current_index += 1;
    //     metadata.total_pages += 1;
    //     metadata.total_bytes += data.len() as u64;
    //     metadata.last_updated = Utc::now().timestamp() as u64;

    //     let bytes = metadata.to_bytes()?;

    //     if let Err(e) = self.write(0, &bytes).await {
    //         info!("error during writing metadata in file : {:?}, Skipping", e);
    //     }

    //     info!("Wrote page {} at offset {}", page_index, offset);
    //     Ok(())
    // }

    async fn write(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        let mut file = self.file.lock().await;

        if let Err(e) = file.seek(SeekFrom::Start(offset)).await {
            info!("error during seeking file : {:?}", e);
            return Err(e);
        }
        if let Err(e) = file.write_all(data).await {
            info!("error during writing file : {:?}", e);
            return Err(e);
        }
        Ok(())
    }
}

pub async fn handle_poke(storage_state: StorageState, packet: Packet) {
    let page_id = packet.meta.page_no;
    let offset = packet.meta.offset;

    let mut  indexes = storage_state.index.lock().await;
    let mut metadata = storage_state.metadata.lock().await;

    let current_index = metadata.current_index;

    let index =  indexes
        .index
        .get(&page_id)
        .map(|ind| ind)
        .unwrap_or(&current_index);
    if !indexes.index.contains_key(&page_id) {
        metadata.current_index += 1;
    }

    let file_offset = Metadata::size() + Index::size() + (index * PAGE_SIZE as u64) + offset as u64;

    let data = packet.data;

    if data.len()+offset as usize > PAGE_SIZE {
        info!("data lengthy exceeds page");
    }

    let _ = storage_state.write(offset as u64, &data).await;

    // indexes.index.insert(page_id, *index);
    let index_bytes = indexes.to_bytes().unwrap();

    metadata.last_updated = Utc::now().timestamp() as u64;
    metadata.total_bytes += data.len() as u64;
    if index >= &metadata.total_pages {
        metadata.total_pages = index + 1;
    }


}
