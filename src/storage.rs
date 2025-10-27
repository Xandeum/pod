use anyhow::{anyhow, Context, Result};
use bincode::{deserialize, serialize};
use chrono::Utc;
use core::error;
use log::{debug, info};
use quinn::SendStream;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::sync::Arc;
use std::{
    collections::HashMap,
    io::Error,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::Mutex,
};

use crate::client::send_packets;
use crate::packet::{AtlasOperation, Meta, Packet};
use crate::protos::{
    ArmageddonData, AssignCoownerPayload, BigBangData, CachePayload, CreateFilePayload,
    DirectoryEntryPage, FileSystemRecord, GlobalCatalogPage, Inode, MkDirPayload, MovePayload,
    PeekPayload, PodMapping, PodMappingsPage, PokePayload, RenamePayload, RmDirPayload,
    RmFilePayload, XentriesPage, XentryMapping,
};
use crate::stats::Stats;
use common::consts::PAGE_SIZE;

// pub const FILE_PATH: &str = "/run/xandeum-pod";
pub const FILE_PATH: &str = "xandeum-pod";

// Global operation counter to track operation ordering
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

const INODE_METADATA_SIZE: u64 = 1024;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum PageDataType {
    Xentries(XentriesPage),
    PodMappings(PodMappingsPage),
    Inode(Inode),
    Directory(DirectoryEntryPage),
    Data(Vec<u8>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]

pub struct PeekResponse {
    response: HashMap<u64, Vec<PageDataType>>,
}

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
    // Global-page => Local-page mapping
    pub index: HashMap<u64, u64>,
}

impl Index {
    fn new() -> Self {
        Index {
            index: HashMap::new(),
        }
        // let mut index = HashMap::with_capacity(200);
        // for i in 0..200 {
        //     index.insert(0, i);
        // }
        // Index { index }
    }

    pub fn size() -> u64 {
        3200
        // bincode::serialized_size(&Index::new()).unwrap_or(3200)
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
        info!("Opening storage file at: {}", FILE_PATH);
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(FILE_PATH)
            .await
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(e).context(format!("Symlinked File not Found : {}", FILE_PATH));
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                return Err(e).context(format!("Permission denied for : {}", FILE_PATH));
            }
            Err(e) => {
                return Err(e).context(format!("Failed to open file: {}", FILE_PATH));
            }
        };

        let mut metadata_bytes = vec![0u8; Metadata::size() as usize];
        let mut index_bytes = vec![0u8; Index::size() as usize];

        file.seek(SeekFrom::Start(PAGE_SIZE)).await?; // Starting from page 1,, Since page 0 is reserved for global catalog
        file.read_exact(&mut metadata_bytes).await?;

        file.seek(SeekFrom::Start(PAGE_SIZE + metadata_bytes.len() as u64))
            .await?;
        file.read_exact(&mut index_bytes).await?;

        let metadata = if metadata_bytes.iter().all(|f| *f == 0) {
            let metadata = Metadata::new();

            file.seek(SeekFrom::Start(PAGE_SIZE)).await?;
            file.write_all(&metadata.to_bytes()?).await?;
            metadata
        } else {
            Metadata::from_bytes(&metadata_bytes)?
        };

        let index = if index_bytes.iter().all(|f| *f == 0) {
            let index = Index::new();
            file.seek(SeekFrom::Start(PAGE_SIZE + Metadata::size()))
                .await?;
            file.write_all(&index.to_bytes()?).await?;
            index
        } else {
            Index::from_bytes(&index_bytes)?
        };

        file.seek(SeekFrom::Start(0)).await?;

        let mut cat_bytes = vec![0u8; PAGE_SIZE as usize];

        file.read_exact(&mut cat_bytes).await?;

        let end = cat_bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);

        // info!("end : {:?}", end);

        // info!("cat bytes ; {:?}", &cat_bytes[..end]);

        match deserialize::<GlobalCatalogPage>(&cat_bytes) {
            Ok(cat) => {
                // info!("catalog : {:?}", cat);
            }
            Err(e) => {
                info!("failed to deserialize catalog : {:?}", e);
                let cat = GlobalCatalogPage {
                    filesystems: vec![],
                    next_catalog_page: 0,
                };

                let mut buf = serialize(&cat)?;
                file.seek(SeekFrom::Start(0)).await?;

                buf.resize(PAGE_SIZE as usize, 0);
                info!("writing catalog");
                file.write_all(&buf).await?;
                info!("flush");

                file.flush().await?;
            }
        }

        Ok(StorageState {
            file: Arc::new(Mutex::new(file)),
            metadata: Arc::new(Mutex::new(metadata)),
            index: Arc::new(Mutex::new(index)),
        })
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
        drop(file);
        Ok(())
    }

    pub async fn read(&self, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut buffer = vec![0u8; length];
        file.read_exact(&mut buffer).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to read {} bytes at offset {}: {}",
                length,
                offset,
                e
            )
        })?;
        debug!("Read {} bytes at offset {}", length, offset);
        drop(file);
        Ok(buffer)
    }

    pub async fn read_page(&self, page_no: u64) -> Result<Vec<u8>> {
        info!("reading global page : {:?}", page_no);
        let local_page_index = self.get_local_index(page_no).await?;
        self.read_page_by_local_index(local_page_index).await
    }

    /// Gets the local index for a global page number
    async fn get_local_index(&self, global_page_no: u64) -> Result<u64> {
        let index_map = self.index.lock().await;
        match index_map.index.get(&global_page_no) {
            Some(local_index) => Ok(*local_index),
            None => Err(anyhow!(
                "Global page number {} not found in the index.",
                global_page_no
            )),
        }
    }

    /// Reads a page by its local index (not global page number)
    pub async fn read_page_by_local_index(&self, local_page_index: u64) -> Result<Vec<u8>> {
        let base_data_offset = PAGE_SIZE + Metadata::size() + Index::size();
        let page_offset = base_data_offset + (local_page_index * PAGE_SIZE);

        info!(
            "Reading local index: {} from offset: {}",
            local_page_index, page_offset
        );

        self.read(page_offset, PAGE_SIZE as usize).await
    }

    pub async fn read_page_data(
        &self,
        page_no: u64,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        let page_bytes = self.read_page(page_no).await?;

        let start = offset as usize + INODE_METADATA_SIZE as usize;
        let end = start + length;
        if end > page_bytes.len() {
            return Err(anyhow!(
                "Requested range ({} to {}) exceeds page size {}",
                start,
                end,
                page_bytes.len()
            ));
        }

        Ok(page_bytes[start..end].to_vec())
    }

    pub async fn get_inode(&self, page_no: u64) -> Result<Inode> {
        let page_bytes = self.read_page(page_no).await?;

        if page_bytes.len() < INODE_METADATA_SIZE as usize {
            return Err(anyhow!(
                "Page {} is smaller than the required inode metadata size.",
                page_no
            ));
        }
        let inode_bytes = &page_bytes[..INODE_METADATA_SIZE as usize];

        Inode::from_bytes(inode_bytes)
    }

    pub async fn get_directory_page(&self, page_no: u64) -> Result<DirectoryEntryPage> {
        let page = self.read_page(page_no).await?;
        let content_bytes = &page[INODE_METADATA_SIZE as usize..];
        let dir_page = bincode::deserialize(content_bytes)?;
        Ok(dir_page)
    }

    pub async fn get_xentries_page(&self, page_no: u64) -> Result<XentriesPage> {
        let page = self.read_page(page_no).await?;
        let content_bytes = &page[INODE_METADATA_SIZE as usize..];
        let dir_page = bincode::deserialize(content_bytes)?;

        Ok(dir_page)
    }

    pub async fn get_pod_mappings_page(&self, page_no: u64) -> Result<PodMappingsPage> {
        let page = self.read_page(page_no).await?;
        let content_bytes = &page[INODE_METADATA_SIZE as usize..];
        let dir_page = bincode::deserialize(content_bytes)?;
        Ok(dir_page)
    }

    pub async fn handle_poke(&self, data: PokePayload, stats: Arc<Mutex<Stats>>) -> Result<()> {
        let operation_id = chrono::Utc::now().timestamp_nanos();
        let operation_order = OPERATION_COUNTER.fetch_add(1, Ordering::SeqCst);
        let start_time = std::time::Instant::now();

        info!(
            "✍️  POKE-{} [ORDER:{}] STARTING - Page: {}, Offset: {}, Length: {} bytes",
            operation_id,
            operation_order,
            data.page_no,
            data.offset,
            data.data.len()
        );

        // Log first and last few bytes for debugging
        let data_preview = if data.data.len() > 10 {
            format!(
                "First 10 bytes: {:?}, Last 10 bytes: {:?}",
                &data.data[..10],
                &data.data[data.data.len() - 10..]
            )
        } else {
            format!("All bytes: {:?}", &data.data)
        };
        info!(
            "✍️  POKE-{} [ORDER:{}] DATA - {}",
            operation_id, operation_order, data_preview
        );

        // Log in hex format for easier debugging
        let bytes_to_hex = |bytes: &[u8]| -> String {
            bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join("")
        };
        let hex_preview = if data.data.len() > 20 {
            format!(
                "Hex: {}...{}",
                bytes_to_hex(&data.data[..10]),
                bytes_to_hex(&data.data[data.data.len() - 10..])
            )
        } else {
            format!("Hex: {}", bytes_to_hex(&data.data))
        };
        info!(
            "✍️  POKE-{} [ORDER:{}] HEX - {}",
            operation_id, operation_order, hex_preview
        );

        if data.offset + data.data.len() as u64 > PAGE_SIZE - INODE_METADATA_SIZE {
            return Err(anyhow!(
                "Poke operation would exceed page boundaries: offset {} + length {} > available space {}",
                data.offset,
                data.data.len(),
                PAGE_SIZE - INODE_METADATA_SIZE
            ));
        }

        let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;

        // Handle inode creation or reuse if provided
        match data.parent_file_inode {
            Some(inode) => {
                let logical_page = inode.pages[0];

                // Check if this logical page already has a physical mapping
                let local_index = match global_index.index.get(&logical_page) {
                    Some(existing_index) => {
                        info!("✍️  POKE-{} [ORDER:{}] REUSING - Logical page {} already mapped to physical index {} (updating inode, preserving content)", 
                               operation_id, operation_order, logical_page, existing_index);

                        // Update only the inode metadata, preserve existing content bytes
                        self.write_inode_only(&inode, *existing_index).await?;

                        *existing_index
                    }
                    None => {
                        // Create new mapping for this logical page
                        let new_index = global_meta.current_index;
                        global_index.index.insert(logical_page, new_index);
                        global_meta.current_index += 1;

                        info!("✍️  POKE-{} [ORDER:{}] NEW_MAPPING - Logical page {} mapped to new physical index {}", 
                               operation_id, operation_order, logical_page, new_index);

                        // Call write_object for new mappings to initialize both inode and content area
                        self.write_object(&inode, &[0u8], new_index).await?;
                        new_index
                    }
                };

                info!(
                    "✍️  POKE-{} [ORDER:{}] USING - Physical index {} for logical page {}",
                    operation_id, operation_order, local_index, logical_page
                );
            }
            None => {
                info!("Inode not Present, Checking Poke operation");
            }
        }

        let local_page_index = match global_index.index.get(&data.page_no) {
            Some(local_index) => *local_index,
            None => {
                return Err(anyhow!(
                    "Page number {} not found in the index.",
                    data.page_no
                ))
            }
        };

        self.write(PAGE_SIZE, &global_meta.to_bytes()?).await?;
        self.write(PAGE_SIZE + Metadata::size(), &global_index.to_bytes()?)
            .await?;

        drop(global_index);
        drop(global_meta);

        let base_data_area_offset = PAGE_SIZE + Metadata::size() + Index::size();
        let physical_offset = base_data_area_offset
            + (local_page_index * PAGE_SIZE)
            + data.offset
            + INODE_METADATA_SIZE;

        info!("✍️  POKE-{} [ORDER:{}] PHYSICAL - Calculated offset: {} (base: {} + page_idx: {} * page_size: {} + data_offset: {} + inode_meta: {})", 
               operation_id, operation_order, physical_offset, base_data_area_offset, local_page_index, PAGE_SIZE, data.offset, INODE_METADATA_SIZE);

        self.write(physical_offset, &data.data).await?;

        let elapsed = start_time.elapsed();
        info!(
            "✍️  POKE-{} [ORDER:{}] COMPLETED - Written {} bytes to page {} at offset {} in {:?}",
            operation_id,
            operation_order,
            data.data.len(),
            data.page_no,
            data.offset,
            elapsed
        );

        match (data.pod_mapping_inode, data.pods_mapping) {
            (Some(inode), Some(entry)) => {
                info!("updating pods mapping");

                let global_index = self.index.lock().await;

                info!("inode pages {:?}", inode.pages[0]);
                // Update second entry
                let local_page = global_index
                    .index
                    .get(&inode.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in pod_mapping_inode"))?
                    .clone();
                drop(global_index);

                let mut dir_entry_page = self.get_pod_mappings_page(inode.pages[0]).await?;
                dir_entry_page.mappings.push(entry);
                self.write_object(&inode, &serialize(&dir_entry_page)?, local_page)
                    .await?;
                info!("Pod mapping updated successfully");
            }
            (None, None) => {
                info!("No parent inode or directory entry data present");
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Partial parent update: pod_mapping_inode and pods_mapping must be present"
                ));
            }
        }

        // Checking For Update on Parent inode

        // let page_id = packet.meta.page_no;
        // let offset = packet.meta.offset;

        // info!("Page id : {}", page_id);
        // info!("offset : {}", offset);

        // let mut indexes = self.index.lock().await;
        // let mut metadata = self.metadata.lock().await;

        // let current_index = metadata.current_index;

        // let index = indexes
        //     .index
        //     .iter()
        //     .find(|(_, &v)| v == page_id)
        //     .map(|(&k, _)| k)
        //     .unwrap_or({
        //         metadata.current_index += 1;
        //         current_index
        //     });

        // info!("index : {}", index);

        // let file_offset = Metadata::size() + Index::size() + (index * PAGE_SIZE) + offset as u64;

        // let data = packet.data;

        // if data.len() + offset as usize > PAGE_SIZE as usize {
        //     info!("data lengthy exceeds page");
        // }
        // info!("Writing in storage");

        // let _ = self.write(file_offset as u64, &data).await;
        // info!("Written in storage");

        // indexes.index.insert(index, page_id);
        // let index_bytes = indexes.to_bytes()?;

        // metadata.last_updated = Utc::now().timestamp() as u64;
        // metadata.total_bytes += data.len() as u64;
        // if index >= metadata.total_pages {
        //     metadata.total_pages = index + 1;
        // }
        // self.write(0, &metadata.to_bytes()?).await?;
        // self.write(Metadata::size(), &&index_bytes).await?;

        // Log filesystem state after poke operation
        let _ = self.log_filesystem_state("POKE").await;

        Ok(())
    }

    pub async fn handle_peek(
        &self,
        sender: Arc<Mutex<SendStream>>,
        data: PeekPayload,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<()> {
        let operation_id = chrono::Utc::now().timestamp_nanos();
        let operation_order = OPERATION_COUNTER.fetch_add(1, Ordering::SeqCst);
        let start_time = std::time::Instant::now();

        info!(
            "👁️  PEEK-{} [ORDER:{}] STARTING - Page: {}, Offset: {}, Length: {} bytes",
            operation_id, operation_order, data.page_no, data.offset, data.length
        );

        // Validate input parameters
        if data.offset + data.length > PAGE_SIZE - INODE_METADATA_SIZE {
            return Err(anyhow!(
                "Peek operation would exceed page boundaries: offset {} + length {} > available space {}",
                data.offset,
                data.length,
                PAGE_SIZE - INODE_METADATA_SIZE
            ));
        }

        let read_data = self
            .read_page_data(data.page_no, data.offset, data.length as usize)
            .await?;

        // Log detailed information about what was read
        let bytes_to_hex = |bytes: &[u8]| -> String {
            bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join("")
        };

        let data_preview = if read_data.len() > 10 {
            format!(
                "First 10 bytes: {:?}, Last 10 bytes: {:?}",
                &read_data[..10],
                &read_data[read_data.len() - 10..]
            )
        } else {
            format!("All bytes: {:?}", &read_data)
        };
        info!(
            "👁️  PEEK-{} [ORDER:{}] DATA - {}",
            operation_id, operation_order, data_preview
        );

        let hex_preview = if read_data.len() > 20 {
            format!(
                "Hex: {}...{}",
                bytes_to_hex(&read_data[..10]),
                bytes_to_hex(&read_data[read_data.len() - 10..])
            )
        } else {
            format!("Hex: {}", bytes_to_hex(&read_data))
        };
        info!(
            "👁️  PEEK-{} [ORDER:{}] HEX - {}",
            operation_id, operation_order, hex_preview
        );

        let elapsed = start_time.elapsed();
        info!(
            "👁️  PEEK-{} [ORDER:{}] COMPLETED - Read {} bytes from page {} at offset {} in {:?}",
            operation_id,
            operation_order,
            read_data.len(),
            data.page_no,
            data.offset,
            elapsed
        );

        // // Create response packet
        let response_packet = Packet {
            meta: Some(Meta {
                op: AtlasOperation::PPeek as i32,
                chunk_seq: 0,
                total_chunks: 1,
                length: 0,
            }),
            data: read_data,
        };

        info!("response packet : {:?}", response_packet);

        send_packets(sender, response_packet, stats.clone()).await?;

        info!("Sent peek response");

        Ok(())
    }

    pub async fn handle_cache(
        self,
        packet: Packet,
        sender: Arc<Mutex<SendStream>>,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<()> {
        let payload: CachePayload = deserialize(&packet.data)
            .map_err(|e| anyhow!("Failed to deserialize cache payload: {}", e))?;

        let pages = payload.pages;
        let catalog = self.clone().read_catalog().await?;

        let mut res: HashMap<u64, Vec<PageDataType>> = HashMap::new();

        info!("pagewseeefwesdf : {:?}", pages);
        for page in pages {
            info!("pagefrdsgfhsdvbgbnfvgdshgvfhvgsdjfghsagh :{:?}", page);

            info!("getting inode");
            let inode = self.get_inode(page).await?;
            info!("inode : {:?}", inode);

            // res.insert(page, PageDataType::Inode(inode.clone()));
            res.entry(page)
                .or_default()
                .push(PageDataType::Inode(inode.clone()));

            info!("resssssssss in mid : {:?}", res);

            if inode.is_directory {
                info!("fetching directory at {:?}", page);
                let page_data = self.get_directory_page(page).await?;

                // res.insert(page, PageDataType::Directory(page_data));
                res.entry(page)
                    .or_default()
                    .push(PageDataType::Directory(page_data));
                continue;
            }

            info!("inode : {:?}", inode);

            if inode.is_system_file {
                info!("fetching inode ");
                let data = self.read_page(page).await?;
                let content_bytes = &data[INODE_METADATA_SIZE as usize..];

                // if let Ok(xentries) = deserialize::<XentriesPage>(content_bytes) {
                //     info!("xetnriees : {:?}", xentries);
                //     res.insert(page, PageDataType::Xentries(xentries));
                // } else if let Ok(pod_mapping) = deserialize::<PodMappingsPage>(content_bytes) {
                //     info!("pod mapung : {:?}", pod_mapping);

                //     res.insert(page, PageDataType::PodMappings(pod_mapping));
                // } else {
                //     log::error!("Unknown system File Data");
                // }

                let page_data = if catalog
                    .filesystems
                    .iter()
                    .any(|fs| fs.xentries_start_page == page)
                {
                    let xentries = deserialize::<XentriesPage>(content_bytes).map_err(|e| {
                        anyhow!(
                            "Failed to deserialize XentriesPage for page {}: {}",
                            page,
                            e
                        )
                    })?;
                    info!("Deserialized XentriesPage: {:?}", xentries);
                    PageDataType::Xentries(xentries)
                } else if catalog
                    .filesystems
                    .iter()
                    .any(|fs| fs.pod_mappings_start_page == page)
                {
                    let pod_mapping =
                        deserialize::<PodMappingsPage>(content_bytes).map_err(|e| {
                            anyhow!(
                                "Failed to deserialize PodMappingsPage for page {}: {}",
                                page,
                                e
                            )
                        })?;
                    info!("Deserialized PodMappingsPage: {:?}", pod_mapping);
                    PageDataType::PodMappings(pod_mapping)
                } else {
                    return Err(anyhow!("Page {} is a system file but not found in catalog as XentriesPage or PodMappingsPage", page));
                };
                // res.insert(page, page_data);
                res.entry(page).or_default().push(page_data);

                // let tag = content_bytes[0];

                // info!("tag : {:?}",tag);
                // info!("tagged ");
                // let content = &content_bytes[1..];

                // info!("xen : {:?}",deserialize::<XentriesPage>(content_bytes).unwrap());
                // info!("mapping : {:?}",deserialize::<PodMappingsPage>(content_bytes).unwrap());

                // match tag {
                //     0 => {
                //         let xentries = deserialize::<XentriesPage>(content).unwrap();
                //         info!("inserting entries {:?}", xentries);
                //         res.insert(page, PageDataType::Xentries(xentries));
                //     }
                //     1 => {
                //         let pod_mapping = deserialize::<PodMappingsPage>(content).unwrap();

                //         info!("inserting mapping {:?}", pod_mapping);

                //         res.insert(page, PageDataType::PodMappings(pod_mapping));
                //     }
                //     _ => {
                //         info!("unknown tag")
                //     }
                // }
            }
        }

        info!("final resssssssssssssssssssssssss : {:?}", res);

        let payload = PeekResponse { response: res };

        let payload_bytes = serialize(&payload)?;

        let packet = Packet::new(0, 0, 0, AtlasOperation::PCache as i32, payload_bytes);

        // info!("sending pack : {:?}", packet);

        send_packets(sender, packet, stats).await?;

        // Log filesystem state after cache operation
        let _ = self.log_filesystem_state("CACHE").await;

        Ok(())
    }

    pub async fn handle_bigbang(self, data: BigBangData) -> Result<()> {
        info!("🚀 STARTING BIGBANG OPERATION");

        let fs_record = data
            .fs_record
            .ok_or_else(|| anyhow!("BigBangData is missing the required FileSystemRecord"))?;

        info!("fs record : {:?}", fs_record);
        let mut inodes = data.inode;
        info!("inodes : {:?}", inodes);

        if inodes.len() != 3 {
            info!("In Sync Mode");
            self.add_catalog_entry(fs_record).await?;
            return Ok(());
        }

        let root_dir_entry = data
            .directory_entery_page
            .ok_or_else(|| anyhow!("Root directory data missing"))?;

        inodes.sort_by_key(|i| i.inode_no);
        let root_inode = &inodes[0];
        let xentries_inode = &inodes[1];
        let pod_mappings_inode = &inodes[2];

        let root_page = root_inode.pages[0];

        let root_inode_no = root_inode.inode_no;
        let xentries_inode_no = xentries_inode.inode_no;
        let mapping_inode_no = pod_mappings_inode.inode_no;

        let home_pnode = fs_record.home_pod_id.clone();

        let pod_mapping_content = PodMappingsPage {
            mappings: vec![
                PodMapping {
                    logical_page: root_page,
                    pod_id: home_pnode.clone(),
                },
                PodMapping {
                    logical_page: fs_record.xentries_start_page,
                    pod_id: home_pnode.clone(),
                },
                PodMapping {
                    logical_page: fs_record.pod_mappings_start_page,
                    pod_id: home_pnode.clone(),
                },
            ],
        };

        let xentries_content = XentriesPage {
            mappings: vec![
                XentryMapping {
                    inode_no: root_inode_no,
                    start_page_number: root_page,
                },
                XentryMapping {
                    inode_no: xentries_inode_no,
                    start_page_number: fs_record.xentries_start_page,
                },
                XentryMapping {
                    inode_no: mapping_inode_no,
                    start_page_number: fs_record.pod_mappings_start_page,
                },
            ],
        };

        let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;

        let root_local_index = global_meta.current_index;
        let xentries_local_index = global_meta.current_index + 1;
        let pod_mappings_local_index = global_meta.current_index + 2;

        info!("writing index");

        global_index.index.insert(root_page, root_local_index);
        global_index
            .index
            .insert(fs_record.xentries_start_page, xentries_local_index);
        global_index
            .index
            .insert(fs_record.pod_mappings_start_page, pod_mappings_local_index);

        info!("writing directory and inode ");

        // Writing All three inodes and content
        self.write_object(root_inode, &serialize(&root_dir_entry)?, root_local_index)
            .await?;
        info!("writing xentries ");

        self.write_object(
            xentries_inode,
            &serialize(&xentries_content)?,
            xentries_local_index,
        )
        .await?;
        info!("writing mapping ");

        self.write_object(
            pod_mappings_inode,
            &serialize(&pod_mapping_content)?,
            pod_mappings_local_index,
        )
        .await?;

        global_meta.current_index += 3; // 3 Pages stored
        global_meta.last_updated = Utc::now().timestamp() as u64;

        info!("writing global meta and index");

        self.write(PAGE_SIZE, &global_meta.to_bytes()?).await?;
        info!("0");

        self.write(PAGE_SIZE + Metadata::size(), &global_index.to_bytes()?)
            .await?;
        drop(global_meta);
        drop(global_index);
        info!("1");

        let xent = self
            .get_xentries_page(fs_record.xentries_start_page)
            .await?;
        info!("3");

        let map = self
            .get_pod_mappings_page(fs_record.pod_mappings_start_page)
            .await?;
        info!("8");

        info!("xent : {:?}", xent);
        info!("map : {:?}", map);

        info!("adding catalog");
        self.add_catalog_entry(fs_record).await?;

        let cat = self.clone().read_catalog().await?;

        info!("catalog : {:?}", cat);

        // info!(
        //     "bigBang for fs_id {} completed successfully.",
        //     fs_record.fs_id
        // );

        // Log filesystem state after bigbang operation
        let _ = self.log_filesystem_state("BIGBANG").await;

        Ok(())
    }

    pub async fn handle_armageddon(self, data: ArmageddonData) -> Result<()> {
        info!("💥 STARTING ARMAGEDDON OPERATION");

        let fs_record = data
            .fs_record
            .ok_or_else(|| anyhow!("ArmageddonData is missing the required FileSystemRecord"))?;

        let pages_to_delete = data.page_ids;
        let cat = self.clone().read_catalog().await?;

        info!("catalog : {:?}", cat);

        if pages_to_delete.is_empty() {
            self.remove_catalog_entry(fs_record.clone()).await?;

            let cat = self.clone().read_catalog().await?;

            info!("catalog : {:?}", cat);
        }

        // let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;

        for page in pages_to_delete {
            // TO DO : Use global index instead of local index for consistency
            let local_index = global_index
                .index
                .remove(&page)
                .ok_or_else(|| anyhow!("Page  {} is not stored on This Pod ", page))?;

            self.clear_page(local_index).await?;
        }

        drop(global_index);

        // let xent = self
        //     .get_xentries_page(fs_record.clone().xentries_start_page)
        //     .await?;
        // let map = self
        //     .get_pod_mappings_page(fs_record.clone().pod_mappings_start_page)
        //     .await?;

        // info!("xent : {:?}", xent);
        // info!("map : {:?}", map);

        // // Log filesystem state after armageddon operation
        // let _ = self.log_filesystem_state("ARMAGEDDON").await;

        Ok(())
    }

    pub async fn handle_mkdir(self, data: MkDirPayload) -> Result<()> {
        info!("📁 STARTING MKDIR OPERATION");
        // Check if the new directory is to be stored here or not

        info!("mkdir payload : {:?}", data);
        match (data.new_inode, data.directory_entery) {
            (None, None) => {
                info!("No inode or directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing new_inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing directory_entery")),
            (Some(inode), Some(entry)) => {
                info!("creating dir");

                let mut global_meta = self.metadata.lock().await;
                let mut global_index = self.index.lock().await;

                let dir_entry_page = DirectoryEntryPage {
                    entries: vec![entry],
                };

                let local_index = global_meta.current_index;

                global_index.index.insert(inode.pages[0], local_index);
                global_meta.current_index += 1;

                self.write_object(&inode, &serialize(&dir_entry_page)?, local_index)
                    .await?;

                self.write(PAGE_SIZE, &global_meta.to_bytes()?).await?;
                self.write(PAGE_SIZE + Metadata::size(), &global_index.to_bytes()?)
                    .await?;

                drop(global_meta);
                drop(global_index);

                let dir_entry_page = self.get_directory_page(inode.pages[0]).await?;

                info!("new directory page : {:?}", dir_entry_page);
            }
        }

        // check if Parent Inode is present / Does it need to be updated

        match (data.parent_inode, data.directory_entery_parent) {
            (None, None) => {
                info!("No parent inode or parent directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing updated parent_inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing parent directory_entry")),
            (Some(inode), Some(entry)) => {
                info!("update parent");

                info!("entry : {:?}", entry);

                info!("hjsakdh iundoeowd : {:?}", inode);

                let global_index = self.index.lock().await;

                let local_page = global_index
                    .index
                    .get(&inode.pages[0])
                    .ok_or_else(|| anyhow!("Page {} not found in index", inode.pages[0]))?
                    .clone();
                drop(global_index);

                let mut dir_entry_page = self.get_directory_page(inode.pages[0]).await?;

                dir_entry_page.entries.push(entry);

                self.write_object(&inode, &serialize(&dir_entry_page)?, local_page)
                    .await?;
            }
        }

        // Update root of File system, Check if it exists here
        match (
            data.xentires_inode,
            data.xentry_mapping,
            data.pod_mapping_inode,
            data.pods_mapping,
        ) {
            (Some(inode1), Some(entry1), Some(inode2), Some(entry2)) => {
                info!("update xentry and mapping");

                let global_index = self.index.lock().await;

                // Update first entry
                let xentries_local_page = global_index
                    .index
                    .get(&inode1.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in xentires_inode"))?
                    .clone();

                let local_page2 = global_index
                    .index
                    .get(&inode2.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in pod_mapping_inode"))?
                    .clone();
                drop(global_index);

                info!("INODE1 : {:?}", inode1);
                info!("INODE2 : {:?}", inode2);

                let mut dir_entry_page1 = self.get_xentries_page(inode1.pages[0]).await?;
                info!("old xentry : {:?}", dir_entry_page1);
                dir_entry_page1.mappings.push(entry1);

                self.write_object(&inode1, &serialize(&dir_entry_page1)?, xentries_local_page)
                    .await?;

                let mut p = self.get_xentries_page(inode1.pages[0]).await?;
                info!("new entry : {:?}", p);

                // Update second entry
                // let local_page2 = global_index.index.get(&inode2.pages[0]).ok_or_else(|| {
                //     anyhow::anyhow!("Invalid page reference in pod_mapping_inode")
                // })?;
                let mut dir_entry_page2 = self.get_pod_mappings_page(inode2.pages[0]).await?;
                info!("old mapping :{:?}", dir_entry_page2);
                dir_entry_page2.mappings.push(entry2);
                self.write_object(&inode2, &serialize(&dir_entry_page2)?, local_page2)
                    .await?;

                let p2 = self.get_pod_mappings_page(inode2.pages[0]).await?;
                info!("old mapping : {:?}", p2);
            }
            (None, None, None, None) => {
                info!("No parent inode or directory entry data present");
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Partial parent update: all of xentires_inode, xentry_mapping, pod_mapping_inode, and pods_mapping must be present"
                ));
            }
        }

        // Log filesystem state after mkdir operation
        let _ = self.log_filesystem_state("MKDIR").await;

        Ok(())
    }

    pub async fn handle_rmdir(self, data: RmDirPayload) -> Result<()> {
        let pages = data.pages;

        let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;
        info!("index : {:?}", global_index.index);

        info!("pages : {:?}", pages);
        for page in pages {
            let local_index = global_index
                .index
                .remove(&page)
                .ok_or_else(|| anyhow!("Page  {} is not stored on This Pod ", page))?;

            self.clear_page(local_index).await?;
        }
        drop(global_index);

        info!("Cleared pages !");

        match (data.inode, data.directory_entery) {
            (None, None) => {
                info!("No inode or directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing new_inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing directory_entery")),
            (Some(inode), Some(dir_entry)) => {
                let global_index = self.index.lock().await;

                info!("direntry : {:?}", dir_entry);
                info!("removing directory entry");
                info!("direntry : {:?}", dir_entry);

                let local_index = global_index
                    .index
                    .get(&inode.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in pod_mapping_inode"))?
                    .clone();

                drop(global_index);

                let mut dir_entry_page = self.get_directory_page(inode.pages[0]).await?;

                info!("directory page : {:?}", dir_entry_page);

                dir_entry_page
                    .entries
                    .retain(|entry| entry.inode_no != dir_entry.inode_no);

                info!("after retain: {:?}", dir_entry_page.entries);

                self.write_object(&inode, &serialize(&dir_entry_page)?, local_index)
                    .await?;

                let n = self.get_directory_page(inode.pages[0]).await?;
                info!("new Directory page : {:?}", n);
            }
        }

        // Update root of File system, Check if it exists here
        match (data.xentires_inode, data.pod_mapping_inode) {
            (Some(inode1), Some(inode2)) => {
                info!("removing xentry and mapping");

                let global_index = self.index.lock().await;

                let xentires = data.xentry_mapping;
                let mappings = data.pods_mapping;

                // Update first entry
                let xentries_local_page = global_index
                    .index
                    .get(&inode1.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in xentires_inode"))?
                    .clone();

                let local_page2 = global_index
                    .index
                    .get(&inode2.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in pod_mapping_inode"))?
                    .clone();

                drop(global_index);

                let mut dir_entry_page1 = self.get_xentries_page(xentries_local_page).await?;
                info!("old xentry : {:?}", dir_entry_page1);

                for mapping in xentires {
                    dir_entry_page1
                        .mappings
                        .retain(|entry| entry.inode_no != mapping.inode_no);
                }

                self.write_object(&inode1, &serialize(&dir_entry_page1)?, xentries_local_page)
                    .await?;

                let global_index = self.index.lock().await;

                // Update second entry
                let local_page2 = global_index.index.get(&inode2.pages[0]).ok_or_else(|| {
                    anyhow::anyhow!("Invalid page reference in pod_mapping_inode")
                })?;
                let mut dir_entry_page2 = self.get_pod_mappings_page(*local_page2).await?;

                for mapping in mappings {
                    dir_entry_page2
                        .mappings
                        .retain(|entry| entry.logical_page != mapping.logical_page);
                }

                self.write_object(&inode2, &serialize(&dir_entry_page2)?, *local_page2)
                    .await?;

                let p2 = self.get_pod_mappings_page(*local_page2).await?;
                info!("old mapping : {:?}", p2);
            }
            (None, None) => {
                info!("No parent inode or directory entry data present");
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Partial parent update: all of xentires_inode, xentry_mapping, pod_mapping_inode, and pods_mapping must be present"
                ));
            }
        }

        // Log filesystem state after rmdir operation
        let _ = self.log_filesystem_state("RMDIR").await;

        Ok(())
    }

    pub async fn handle_create_file(self, data: CreateFilePayload) -> Result<()> {
        info!("📄 STARTING CREATE_FILE OPERATION");
        info!("create file");
        let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;

        match data.new_inode {
            Some(inode) => {
                info!("inode : {:?}", inode);

                let local_index = global_meta.current_index;

                global_index.index.insert(inode.pages[0], local_index);
                global_meta.current_index += 1;

                self.write_object(&inode, &[0u8], local_index).await?;

                self.write(PAGE_SIZE, &global_meta.to_bytes()?).await?;
                self.write(PAGE_SIZE + Metadata::size(), &global_index.to_bytes()?)
                    .await?;
            }
            None => {
                info!("Inode not Present, Checking parent");
            }
        }
        drop(global_meta);
        drop(global_index);

        match (data.parent_inode, data.directory_entery_parent) {
            (None, None) => {
                info!("No parent inode or parent directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing updated parent_inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing parent directory_entry")),
            (Some(inode), Some(entry)) => {
                info!("inodeddededede  {:?}", inode);
                let global_index = self.index.lock().await;

                let local_page = global_index
                    .index
                    .get(&inode.pages[0])
                    .ok_or_else(|| {
                        anyhow!(
                            "Page {} not found in index for parent inode",
                            inode.pages[0]
                        )
                    })?
                    .clone();
                drop(global_index);

                let mut dir_entry_page = self.get_directory_page(inode.pages[0]).await?;
                info!("old directory page : {:?}", dir_entry_page);

                dir_entry_page.entries.push(entry);

                self.write_object(&inode, &serialize(&dir_entry_page)?, local_page)
                    .await?;

                let mut n = self.get_directory_page(inode.pages[0]).await?;
                info!("old directory page : {:?}", n);
            }
        }

        // Update root of File system, Check if it exists here
        match (
            data.xentires_inode,
            data.xentry_mapping,
            data.pod_mapping_inode,
            data.pods_mapping,
        ) {
            (Some(inode1), Some(entry1), Some(inode2), Some(entry2)) => {
                let global_index = self.index.lock().await;

                // Update first entry
                let xentries_local_page = global_index
                    .index
                    .get(&inode1.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in xentires_inode"))?
                    .clone();

                let local_page2 = global_index
                    .index
                    .get(&inode2.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in pod_mapping_inode"))?
                    .clone();

                drop(global_index);

                let mut dir_entry_page1 = self.get_xentries_page(inode1.pages[0]).await?;
                info!("old xentry : {:?}", dir_entry_page1);
                dir_entry_page1.mappings.push(entry1);

                self.write_object(&inode1, &serialize(&dir_entry_page1)?, xentries_local_page)
                    .await?;

                let mut p = self.get_xentries_page(inode1.pages[0]).await?;
                info!("new xentry : {:?}", p);

                // Update second entry

                let mut dir_entry_page2 = self.get_pod_mappings_page(inode2.pages[0]).await?;
                info!("old mapping : {:?}", dir_entry_page2);

                dir_entry_page2.mappings.push(entry2);
                self.write_object(&inode2, &serialize(&dir_entry_page2)?, local_page2)
                    .await?;

                let mut q = self.get_pod_mappings_page(inode2.pages[0]).await?;
                info!("new mapping : {:?}", q);
            }
            (None, None, None, None) => {
                info!("No parent inode or directory entry data present");
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Partial parent update: all of xentires_inode, xentry_mapping, pod_mapping_inode, and pods_mapping must be present"
                ));
            }
        }

        // Log filesystem state after create_file operation
        let _ = self.log_filesystem_state("CREATE_FILE").await;

        Ok(())
    }

    pub async fn handle_delete_file(self, data: RmFilePayload) -> Result<()> {
        let pages = data.pages;

        let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;

        info!("pages : {:?}", pages);
        info!("global_index : {:?}", global_index);
        for page in pages {

            let local_index = global_index
                .index
                .remove(&page)
                .ok_or_else(|| anyhow!("Page  {} is not stored on This Pod ", page))?;

            self.clear_page(local_index).await?;
        }
        drop(global_index);

        info!("Cleared pages !");

        match (data.inode, data.directory_entery) {
            (None, None) => {
                info!("No inode or directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing new_inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing directory_entery")),
            (Some(inode), Some(dir_entry)) => {
                let global_index = self.index.lock().await;

                // Look up the actual local index for this inode's page
                let local_index = global_index
                    .index
                    .get(&inode.pages[0])
                    .ok_or_else(|| {
                        anyhow!(
                            "Page {} not found in index for parent directory",
                            inode.pages[0]
                        )
                    })?
                    .clone();
                drop(global_index);

                let mut dir_entry_page = self.get_directory_page(inode.pages[0]).await?;

                dir_entry_page
                    .entries
                    .retain(|entry| entry.inode_no != dir_entry.inode_no);

                self.write_object(&inode, &serialize(&dir_entry_page)?, local_index)
                    .await?;
            }
        }

        // Update root of File system, Check if it exists here
        match (data.xentires_inode, data.pod_mapping_inode) {
            (Some(inode1), Some(inode2)) => {
                let global_index = self.index.lock().await;

                let xentires = data.xentry_mapping;
                let mappings = data.pods_mapping;

                // Update first entry
                let xentries_local_page = global_index
                    .index
                    .get(&inode1.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in xentires_inode"))?
                    .clone();
                
                let local_page2 = global_index
                    .index
                    .get(&inode2.pages[0])
                    .ok_or_else(|| anyhow::anyhow!("Invalid page reference in pod_mapping_inode"))?
                    .clone();
                drop(global_index);
                let mut dir_entry_page1 = self.get_xentries_page(inode1.pages[0]).await?;

                for mapping in xentires {
                    dir_entry_page1
                        .mappings
                        .retain(|entry| entry.inode_no != mapping.inode_no);
                }

                self.write_object(&inode1, &serialize(&dir_entry_page1)?, xentries_local_page)
                    .await?;

                // Update second entry
               
                let mut dir_entry_page2 = self.get_pod_mappings_page(inode2.pages[0]).await?;

                for mapping in mappings {
                    dir_entry_page2
                        .mappings
                        .retain(|entry| entry.logical_page != mapping.logical_page);
                }

                self.write_object(&inode2, &serialize(&dir_entry_page2)?, local_page2)
                    .await?;
            }
            (None, None) => {
                info!("No parent inode or directory entry data present");
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Partial parent update: all of xentires_inode, xentry_mapping, pod_mapping_inode, and pods_mapping must be present"
                ));
            }
        }
        drop(global_meta);

        // Log filesystem state after delete_file operation
        let _ = self.log_filesystem_state("DELETE_FILE").await;

        Ok(())
    }

    pub async fn handle_rename(self, data: RenamePayload) -> Result<()> {
        // match (data.directory_inode, data.directory_entry) {
        //     (None, None) => {
        //         info!("No parent inode or parent directory entry present")
        //     }
        //     (None, _) => return Err(anyhow::anyhow!("Missing updated parent_inode")),
        //     (_, None) => return Err(anyhow::anyhow!("Missing parent directory_entry")),
        //     (Some(inode), Some(entry)) => {
        //         let global_index = self.index.lock().await;

        //         let local_page = global_index.index.get(&inode.pages[0]).unwrap();

        //         let mut dir_entry_page = self.get_directory_page(*local_page).await?;

        //         dir_entry_page
        //             .entries
        //             .retain(|entry| entry.inode_no != entry.inode_no);

        //         dir_entry_page.entries.push(entry);

        //         self.write_object(&inode, &serialize(&dir_entry_page)?, *local_page)
        //             .await?;
        //     }
        // }

        // Log filesystem state after rename operation
        let _ = self.log_filesystem_state("RENAME").await;

        Ok(())
    }

    pub async fn handle_move(self, data: MovePayload) -> Result<()> {
        let mut global_meta = self.metadata.lock().await;
        let mut global_index = self.index.lock().await;

        match (
            data.source_parent_directory_inode,
            data.source_parent_directory,
        ) {
            (None, None) => {
                info!("No inode or directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing old_inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing directory_entry")),
            (Some(inode), Some(dir_entry)) => {
                let mut dir_entry_page = self.get_directory_page(inode.pages[0]).await?;

                dir_entry_page
                    .entries
                    .retain(|entry| entry.inode_no != dir_entry.inode_no);

                let local_index = global_meta.current_index;

                self.write_object(&inode, &serialize(&dir_entry_page)?, local_index)
                    .await?;
            }
        }

        match (
            data.destination_parent_directory_inode,
            data.destination_parent_directory,
        ) {
            (None, None) => {
                info!("No inode or directory entry present")
            }
            (None, _) => return Err(anyhow::anyhow!("Missing updated inode")),
            (_, None) => return Err(anyhow::anyhow!("Missing directory_entry")),
            (Some(inode), Some(entry)) => {
                let global_index = self.index.lock().await;

                let local_page = global_index.index.get(&inode.pages[0]).ok_or_else(|| {
                    anyhow!(
                        "Page {} not found in index for destination directory",
                        inode.pages[0]
                    )
                })?;

                let mut dir_entry_page = self.get_directory_page(*local_page).await?;

                dir_entry_page.entries.push(entry);

                self.write_object(&inode, &serialize(&dir_entry_page)?, *local_page)
                    .await?;
            }
        }

        // Log filesystem state after move operation
        let _ = self.log_filesystem_state("MOVE").await;

        Ok(())
    }

    pub async fn handle_quorum(
        self,
        sender: Arc<Mutex<SendStream>>,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<()> {
        let catalogue = self.read_catalog().await?;

        let bytes =
            serialize(&catalogue).map_err(|e| anyhow!("Failed to serialize catalog: {}", e))?;

        let pkt = Packet::new(
            0,
            0,
            bytes.len() as u64,
            AtlasOperation::PQuorum as i32,
            bytes,
        );

        send_packets(sender, pkt, stats).await?;

        Ok(())
    }

    pub async fn handle_assign_co_owner(self, data: AssignCoownerPayload) -> Result<()> {
      
        for inode_to_update in data.inode {
            let global_index = self.index.lock().await;

            info!(
                "Processing co-owner assignment for inode {}",
                inode_to_update.inode_no
            );

            let page_number = inode_to_update.pages[0];
           
            let local_index = global_index
                .clone()
                .index
                .get(&page_number)
                .ok_or_else(|| {
                    anyhow!(
                        "Page {} not found in index for inode {}",
                        page_number,
                        inode_to_update.inode_no
                    )
                })?
                .clone();
            drop(global_index);

            let mut current_inode = self.get_inode(page_number).await?;

            current_inode.co_owner = inode_to_update.co_owner.clone();
            current_inode.modified_timestamp = Utc::now().timestamp() as u64;

            self.write_inode_only(&current_inode, local_index).await?;

            info!(
                "Successfully updated co-owner for inode {} to '{}'",
                current_inode.inode_no, current_inode.co_owner
            );
        }

        let mut global_meta = self.metadata.lock().await;

        // Update global metadata timestamp
        global_meta.last_updated = Utc::now().timestamp() as u64;

        // Persist metadata changes
        self.write(PAGE_SIZE, &global_meta.to_bytes()?).await?;

        drop(global_meta);

        Ok(())
    }

    /// Allocates a new global page number for a filesystem
    /// This should be called when creating new inodes or pages
    pub async fn allocate_global_page(&self, fs_id: u64) -> Result<u64> {
        let metadata = self.metadata.lock().await;
        // Use filesystem ID and current index to generate unique page numbers
        // This ensures different filesystems don't collide
        let base_page = fs_id * 1_000_000; // Each filesystem gets a million page range
        let page_offset = metadata.total_pages;
        let global_page = base_page + page_offset;
        Ok(global_page)
    }

    /// Allocates a new local page index and returns both global and local indices
    pub async fn allocate_page(&self, fs_id: u64) -> Result<(u64, u64)> {
        let mut metadata = self.metadata.lock().await;
        let mut index = self.index.lock().await;

        // Allocate global page number
        let base_page = fs_id * 1_000_000;
        let global_page = base_page + metadata.total_pages;

        // Allocate local index
        let local_index = metadata.current_index;

        // Update index mapping
        index.index.insert(global_page, local_index);

        // Update metadata
        metadata.current_index += 1;
        metadata.total_pages += 1;
        metadata.last_updated = Utc::now().timestamp() as u64;

        // Persist changes
        self.write(PAGE_SIZE, &metadata.to_bytes()?).await?;
        self.write(PAGE_SIZE + Metadata::size(), &index.to_bytes()?)
            .await?;

        Ok((global_page, local_index))
    }

    /// Updates only the inode metadata, preserving existing content
    async fn write_inode_only(&self, inode: &Inode, local_page_index: u64) -> Result<()> {
        let inode_bytes = inode.to_bytes()?;

        let base_data_area_offset = PAGE_SIZE + Metadata::size() + Index::size();
        let physical_offset = base_data_area_offset + (local_page_index * PAGE_SIZE);

        let mut file_handle = self.file.lock().await;
        file_handle
            .seek(std::io::SeekFrom::Start(physical_offset))
            .await?;
        file_handle.write_all(&inode_bytes).await?;
        // Don't write content - preserve existing content bytes
        drop(file_handle);
        Ok(())
    }

    async fn write_object(
        &self,
        inode: &Inode,
        content_bytes: &[u8],
        local_page_index: u64,
    ) -> Result<()> {
        let inode_bytes = inode.to_bytes()?;

        let base_data_area_offset = PAGE_SIZE + Metadata::size() + Index::size();
        let physical_offset = base_data_area_offset + (local_page_index * PAGE_SIZE);

        let mut file_handle = self.file.lock().await;
        file_handle
            .seek(std::io::SeekFrom::Start(physical_offset))
            .await?;
        file_handle.write_all(&inode_bytes).await?;
        file_handle
            .seek(std::io::SeekFrom::Start(
                physical_offset + INODE_METADATA_SIZE,
            ))
            .await?;
        file_handle.write_all(content_bytes).await?;
        drop(file_handle);
        Ok(())
    }

    async fn read_catalog(self) -> Result<GlobalCatalogPage> {
        let mut file_handle = self.file.lock().await;

        file_handle.seek(SeekFrom::Start(0)).await?;

        let mut buffer = vec![0u8; PAGE_SIZE as usize];
        file_handle.read_exact(&mut buffer).await?;

        drop(file_handle);

        let end = buffer.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        info!("end : {:?}", end);

        if end == 0 {
            return Ok(GlobalCatalogPage {
                filesystems: vec![],
                next_catalog_page: 0,
            });
        }

        // deserialize(&buffer[..end]).context("Failed to deserialize Page 0")
        match deserialize(&buffer) {
            Ok(res) => Ok(res),
            Err(e) => {
                log::error!("error reading catalog : {:?}", e);
                return Ok(GlobalCatalogPage {
                    filesystems: vec![],
                    next_catalog_page: 0,
                });
            }
        }
    }

    pub async fn add_catalog_entry(&self, fs_record: FileSystemRecord) -> Result<()> {
        let mut catalog_page = self.clone().read_catalog().await?;
        if catalog_page
            .filesystems
            .iter()
            .any(|fs| fs.fs_id == fs_record.fs_id)
        {
            // return Err(anyhow!(
            //     "Filesystem with fs_id {} already exists in the catalog.",
            //     fs_record.fs_id
            // ));
            info!("Filesystem with fs_id {} already exists in the catalog.", fs_record.fs_id);
            return Ok(());
        }
        catalog_page.filesystems.push(fs_record);
        self.write_catalog(catalog_page).await?;

        Ok(())
    }

    pub async fn remove_catalog_entry(&self, fs_record: FileSystemRecord) -> Result<()> {
        let mut catalog_page = self.clone().read_catalog().await?;
        catalog_page
            .filesystems
            .retain(|fs| fs.fs_id != fs_record.fs_id);
        self.write_catalog(catalog_page).await?;

        Ok(())
    }

    pub async fn write_catalog(&self, catalog_page: GlobalCatalogPage) -> Result<()> {
        let mut bytes = serialize(&catalog_page)?;
        bytes.resize(PAGE_SIZE as usize, 0);
        let mut file_handle = self.file.lock().await;
        file_handle.seek(SeekFrom::Start(0)).await?;
        file_handle.write_all(&bytes).await?;
        drop(file_handle);
        Ok(())
    }

    pub async fn clear_page(&self, page_no: u64) -> Result<()> {
        let base_data_area_offset = PAGE_SIZE + Metadata::size() + Index::size();
        let physical_offset = base_data_area_offset + (page_no * PAGE_SIZE);

        self.write(physical_offset, &[0; PAGE_SIZE as usize])
            .await?;

        Ok(())
    }

    /// Logs the current filesystem state for debugging
    async fn log_filesystem_state(&self, operation: &str) -> Result<()> {
        info!(
            "=== FILESYSTEM STATE AFTER {} ===",
            operation.to_uppercase()
        );

        // Log the global catalog
        match self.clone().read_catalog().await {
            Ok(catalog) => {
                info!("📁 GLOBAL CATALOG:");
                info!("  Total filesystems: {}", catalog.filesystems.len());
                for fs in &catalog.filesystems {
                    info!("  🗂️  Filesystem ID: {}", fs.fs_id);
                    info!("      Home Pod: {}", fs.home_pod_id[0]);
                    info!("      Root Inode: {}", fs.root_inode_id);
                    info!("      Xentries Page: {}", fs.xentries_start_page);
                    info!("      Pod Mappings Page: {}", fs.pod_mappings_start_page);
                    info!("      Owner: {}", fs.owner);

                    // Log xentries for this filesystem
                    match self.get_xentries_page(fs.xentries_start_page).await {
                        Ok(xentries) => {
                            info!("      📋 XENTRIES ({} entries):", xentries.mappings.len());
                            for mapping in &xentries.mappings {
                                info!(
                                    "        Inode {} -> Page {}",
                                    mapping.inode_no, mapping.start_page_number
                                );
                            }
                        }
                        Err(e) => info!("      ❌ Failed to read xentries: {}", e),
                    }

                    // Log pod mappings for this filesystem
                    match self.get_pod_mappings_page(fs.pod_mappings_start_page).await {
                        Ok(pod_mappings) => {
                            info!(
                                "      🗺️  POD MAPPINGS ({} entries):",
                                pod_mappings.mappings.len()
                            );
                            for mapping in &pod_mappings.mappings {
                                info!(
                                    "        Page {} -> Pod {}",
                                    mapping.logical_page, mapping.pod_id[0]
                                );
                            }
                        }
                        Err(e) => info!("      ❌ Failed to read pod mappings: {}", e),
                    }

                    // Log directory structure starting from root
                    if let Ok(root_inode_no) = fs.root_inode_id.parse::<u64>() {
                        if let Some(xentries) =
                            self.get_xentries_page(fs.xentries_start_page).await.ok()
                        {
                            if let Some(root_mapping) = xentries
                                .mappings
                                .iter()
                                .find(|m| m.inode_no == root_inode_no)
                            {
                                self.log_directory_tree(
                                    root_mapping.start_page_number,
                                    "      🌳 DIRECTORY TREE:",
                                    8,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            Err(e) => info!("❌ Failed to read catalog: {}", e),
        }

        // Log current metadata
        let metadata = self.metadata.lock().await;
        info!("📊 METADATA:");
        info!("  Current Index: {}", metadata.current_index);
        info!("  Total Pages: {}", metadata.total_pages);
        info!("  Total Bytes: {}", metadata.total_bytes);
        info!("  Last Updated: {}", metadata.last_updated);
        drop(metadata);

        // Log index mapping
        let index = self.index.lock().await;
        info!("🗂️  INDEX MAPPINGS ({} entries):", index.index.len());
        let mut sorted_mappings: Vec<_> = index.index.iter().collect();
        sorted_mappings.sort_by_key(|(global_page, _)| *global_page);
        for (global_page, local_index) in sorted_mappings {
            info!(
                "  Global Page {} -> Local Index {}",
                global_page, local_index
            );
        }
        drop(index);

        info!("=== END FILESYSTEM STATE ===\n");
        Ok(())
    }

    /// Recursively logs directory tree structure
    async fn log_directory_tree(&self, page_no: u64, prefix: &str, indent: usize) {
        let indent_str = " ".repeat(indent);

        match self.get_inode(page_no).await {
            Ok(inode) => {
                if inode.is_directory {
                    info!(
                        "{}📁 Directory (Inode {}, Page {})",
                        indent_str, inode.inode_no, page_no
                    );
                    match self.get_directory_page(page_no).await {
                        Ok(dir_page) => {
                            for entry in &dir_page.entries {
                                info!(
                                    "{}  └─ {} (Inode {})",
                                    indent_str, entry.name, entry.inode_no
                                );
                                // Could recursively explore subdirectories here if needed
                            }
                        }
                        Err(e) => info!("{}  ❌ Failed to read directory: {}", indent_str, e),
                    }
                } else {
                    info!(
                        "{}📄 File (Inode {}, Page {}, Size: {} bytes)",
                        indent_str, inode.inode_no, page_no, inode.size
                    );
                }
            }
            Err(e) => info!(
                "{}❌ Failed to read inode for page {}: {}",
                indent_str, page_no, e
            ),
        }
    }
}

impl Inode {
    pub fn new(
        inode_no: u64,
        ownership: String,
        is_directory: bool,
        is_system_file: bool,
        initial_pages: Vec<u64>,
    ) -> Self {
        Self {
            inode_no,
            created_timestamp: Utc::now().timestamp() as u64,
            modified_timestamp: Utc::now().timestamp() as u64,
            size: 0,
            is_directory,
            is_system_file,
            pages: initial_pages, // Use provided pages instead of hardcoded value
            // ownership field will be added after proto recompilation
            // ownership: ownership,
            co_owner: "a".to_string(),
        }
    }

    // Add a convenience method for creating an inode with a single page
    pub fn new_with_page(
        inode_no: u64,
        ownership: String,
        is_directory: bool,
        is_system_file: bool,
        page_no: u64,
    ) -> Self {
        Self::new(
            inode_no,
            ownership,
            is_directory,
            is_system_file,
            vec![page_no],
        )
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = bincode::serialize(self)?;
        if bytes.len() > INODE_METADATA_SIZE as usize {
            return Err(anyhow!(
                "Inode metadata exceeds {} bytes",
                INODE_METADATA_SIZE
            ));
        }
        bytes.resize(INODE_METADATA_SIZE as usize, 0);
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        deserialize(bytes).context("Failed to decode Inode using bincode")
    }
}
