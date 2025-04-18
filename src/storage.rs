use anyhow::Result;
use bincode::{deserialize, serialize};
use chrono::Utc;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::io::Error;
use std::sync::Arc;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::Mutex,
};

const METADATA_SIZE: u64 = 42;
const FILE_PATH: &str = "xandeum-pod";
const PAGE_SIZE: usize = 1048576;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Metadata {
    current_index: u64,
    total_pages: u64,
    last_updated: u64,
    total_bytes: u64,
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

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serialize(self)?;
        if bytes.len() > METADATA_SIZE as usize {
            info!("bytes : {}", bytes.len());
            return Err(anyhow::anyhow!("Metadata exceeds {} bytes", METADATA_SIZE));
        }
        bytes.resize(METADATA_SIZE as usize, 0);
        Ok(bytes)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let metadata = deserialize(&bytes)?;
        Ok(metadata)
    }
}

#[derive(Clone)]
pub struct StorageState {
    pub file: Arc<Mutex<File>>,
    pub metadata: Arc<Mutex<Metadata>>,
}

impl StorageState {
    pub async fn get_or_create_state() -> Result<Self> {
        info!("path : {:?}", FILE_PATH);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(FILE_PATH)
            .await?;

        let mut metadata_bytes = vec![0u8; METADATA_SIZE as usize];

        file.seek(SeekFrom::Start(0)).await?;
        file.read_exact(&mut metadata_bytes).await?;

        let metadata = if metadata_bytes.iter().all(|f| *f == 0) {
            let metadata = Metadata::new();
            file.seek(SeekFrom::Start(0)).await?;
            file.write_all(&metadata.to_bytes()?).await?;
            metadata
        } else {
            Metadata::from_bytes(&metadata_bytes)?
        };

        Ok(StorageState {
            file: Arc::new(Mutex::new(file)),
            metadata: Arc::new(Mutex::new(metadata)),
        })
    }

    pub async fn write_page(&self, data: &[u8]) -> Result<()> {
        if data.len() != PAGE_SIZE {
            info!("data length is not equal page size, Skipping");
            return Ok(());
        }

        let mut metadata = self.metadata.lock().await;
        let page_index = metadata.current_index;
        let offset = METADATA_SIZE as u64 + page_index * PAGE_SIZE as u64;

        if offset + PAGE_SIZE as u64 > 107_374_182_400 {
            info!("Storage Out of capacity!, Skipping");
            return Ok(());
        }

        if let Err(e) = self.write(offset, data).await {
            info!("error during writing file : {:?}, Skipping", e);
        }

        metadata.current_index += 1;
        metadata.total_pages += 1;
        metadata.total_bytes += data.len() as u64;
        metadata.last_updated = Utc::now().timestamp() as u64;

        let bytes = metadata.to_bytes()?;

        if let Err(e) = self.write(0, &bytes).await {
            info!("error during writing metadata in file : {:?}, Skipping", e);
        }

        info!("Wrote page {} at offset {}", page_index, offset);
        Ok(())
    }

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
