use anyhow::{anyhow, Context, Result};
use bincode::{deserialize, serialize};
use chrono::Utc;
use log::{debug, info};
use quinn::SendStream;
use serde::{Deserialize, Serialize};
use std::error;
use std::io::ErrorKind;
use std::sync::Arc;
use std::{collections::HashMap, io::Error};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::Mutex,
};

use crate::client::send_packets;
use crate::packet::{AtlasOperation, Meta, Packet, MAX_DATA_IN_PACKET};
use crate::protos::{
    DirectoryEntry, DirectoryEntryPage, FileSystemRecord, GlobalCatalogPage, Inode, PeekPayload,
    PodMapping, PodMappingsPage, XentriesPage, XentryMapping,
};
use crate::stats::Stats;
use common::consts::PAGE_SIZE;

pub const FILE_PATH: &str = "xandeum-pod";
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
    response: HashMap<u64, PageDataType>,
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
        Ok(StorageState {
            file: Arc::new(Mutex::new(file)),
            metadata: Arc::new(Mutex::new(metadata)),
            index: Arc::new(Mutex::new(index)),
        })
    }

    pub async fn bootstrap_dummy_filesystem(&self, fs_id: u64, pod_id_str: &str) -> Result<()> {
        info!("Bootstrapping dummy filesystem with ID: {}", fs_id);

        // First, check if this filesystem already exists
        if self
            .clone()
            .read_catalog()
            .await?
            .filesystems
            .iter()
            .any(|fs| fs.fs_id == fs_id)
        {
            return Err(anyhow!("Filesystem with ID {} already exists.", fs_id));
        }

        let mut metadata = self.metadata.lock().await;
        let mut index_map = self.index.lock().await;

        // --- 1. Define Inodes and Page IDs ---
        // We need 5 new pages for this filesystem.
        let base_local_idx = metadata.current_index;
        let base_global_page_id = fs_id * 1000; // Simple scheme to avoid collisions

        let root_inode_no = 1;
        let docs_inode_no = 2;
        let file1_inode_no = 3;
        let xentries_inode_no = 4;
        let pod_map_inode_no = 5;

        let root_page_id = base_global_page_id + 1;
        let docs_page_id = base_global_page_id + 2;
        let file1_page_id = base_global_page_id + 3;
        let xentries_page_id = base_global_page_id + 4;
        let pod_map_page_id = base_global_page_id + 5;

        let mut root_inode = Inode::new(root_inode_no, "system".to_string(), true, false);
        root_inode.pages = vec![root_page_id];

        let mut docs_inode = Inode::new(docs_inode_no, "system".to_string(), true, false);
        docs_inode.pages = vec![docs_page_id];

        let mut file1_inode = Inode::new(file1_inode_no, "system".to_string(), false, false);
        file1_inode.pages = vec![file1_page_id];

        let mut xentries_inode = Inode::new(xentries_inode_no, "system".to_string(), false, true);
        xentries_inode.pages = vec![xentries_page_id];

        let mut pod_map_inode = Inode::new(pod_map_inode_no, "system".to_string(), false, true);
        pod_map_inode.pages = vec![pod_map_page_id];

        // --- 2. Define Page Contents ---
        let root_dir_content = DirectoryEntryPage {
            entries: vec![DirectoryEntry {
                name: "docs".to_string(),
                inode_no: docs_inode_no,
            }],
        };
        let docs_dir_content = DirectoryEntryPage {
            entries: vec![DirectoryEntry {
                name: "file1.txt".to_string(),
                inode_no: file1_inode_no,
            }],
        };
        let file1_content = b"Hello from a dummy file in a bootstrapped filesystem!";

        let xentries_content = XentriesPage {
            mappings: vec![
                XentryMapping {
                    inode_no: root_inode_no,
                    start_page_number: root_page_id,
                },
                XentryMapping {
                    inode_no: docs_inode_no,
                    start_page_number: docs_page_id,
                },
                XentryMapping {
                    inode_no: file1_inode_no,
                    start_page_number: file1_page_id,
                },
                XentryMapping {
                    inode_no: xentries_inode_no,
                    start_page_number: xentries_page_id,
                },
                XentryMapping {
                    inode_no: pod_map_inode_no,
                    start_page_number: pod_map_page_id,
                },
            ],
        };

        let pod_mappings_content = PodMappingsPage {
            mappings: vec![
                PodMapping {
                    logical_page: root_page_id,
                    pod_id: pod_id_str.to_string(),
                },
                PodMapping {
                    logical_page: docs_page_id,
                    pod_id: pod_id_str.to_string(),
                },
                PodMapping {
                    logical_page: file1_page_id,
                    pod_id: pod_id_str.to_string(),
                },
                PodMapping {
                    logical_page: xentries_page_id,
                    pod_id: pod_id_str.to_string(),
                },
                PodMapping {
                    logical_page: pod_map_page_id,
                    pod_id: pod_id_str.to_string(),
                },
            ],
        };

        // --- 3. Write objects to disk ---
        self.write_object(&root_inode, &serialize(&root_dir_content)?, base_local_idx)
            .await?;
        self.write_object(
            &docs_inode,
            &serialize(&docs_dir_content)?,
            base_local_idx + 1,
        )
        .await?;
        self.write_object(&file1_inode, file1_content, base_local_idx + 2)
            .await?;
        self.write_object(
            &xentries_inode,
            &serialize(&xentries_content)?,
            base_local_idx + 3,
        )
        .await?;
        self.write_object(
            &pod_map_inode,
            &serialize(&pod_mappings_content)?,
            base_local_idx + 4,
        )
        .await?;

        // --- 4. Update index and metadata ---
        index_map.index.insert(root_page_id, base_local_idx);
        index_map.index.insert(docs_page_id, base_local_idx + 1);
        index_map.index.insert(file1_page_id, base_local_idx + 2);
        index_map.index.insert(xentries_page_id, base_local_idx + 3);
        index_map.index.insert(pod_map_page_id, base_local_idx + 4);

        metadata.current_index += 5;
        metadata.total_pages += 5;
        metadata.last_updated = Utc::now().timestamp() as u64;

        // --- 5. Create Filesystem Record and add to Catalog ---
        let fs_record = FileSystemRecord {
            fs_id,
            home_pod_id: pod_id_str.to_string(),
            root_inode_id: root_inode_no.to_string(),
            xentries_start_page: xentries_page_id,
            pod_mappings_start_page: pod_map_page_id,
        };

        // This must be done before we drop the locks
        let catalog_bytes = {
            let mut catalog =
                self.clone()
                    .read_catalog()
                    .await
                    .unwrap_or_else(|_| GlobalCatalogPage {
                        filesystems: vec![],
                        next_catalog_page: 0,
                    });
            catalog.filesystems.push(fs_record);
            serialize(&catalog)?
        };

        // --- 6. Persist all metadata changes ---
        self.write(0, &catalog_bytes).await?;
        self.write(PAGE_SIZE, &metadata.to_bytes()?).await?;
        self.write(PAGE_SIZE + Metadata::size(), &index_map.to_bytes()?)
            .await?;

        info!("Successfully bootstrapped dummy filesystem ID: {}", fs_id);
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
        Ok(buffer)
    }

    pub async fn read_page(&self, page_no: u64) -> Result<Vec<u8>> {
        let index_map = self.index.lock().await;

        let local_page_index = match index_map.index.get(&page_no) {
            Some(local_index) => *local_index,
            None => return Err(anyhow!("Page number {} not found in the index.", page_no)),
        };

        let base_data_offset = PAGE_SIZE + Metadata::size() + Index::size();
        let page_offset = base_data_offset + (local_page_index * PAGE_SIZE);

        info!(
            "Reading page_no: {} (local index: {}) from offset: {}",
            page_no, local_page_index, page_offset
        );
        self.read(page_offset, PAGE_SIZE as usize).await
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

    pub async fn handle_poke(&self, packet: Packet) -> Result<()> {
        info!("poking");

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
        Ok(())
    }

    pub async fn handle_peek(
        &self,
        sender: Arc<Mutex<SendStream>>,
        packet: Packet,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<()> {
        info!("Handling peek");

        // let page_id = packet.meta.page_no;
        // let offset = packet.meta.offset;
        // let length = packet.meta.length;

        // info!("Page id: {}", page_id);
        // info!("offset: {}", offset);
        // info!("length: {}", length);

        // // Validate page_id
        // if page_id >= 200 {
        //     return Err(anyhow::anyhow!(
        //         "Page ID {} exceeds fixed index size 200",
        //         page_id
        //     ));
        // }
        // let indexes = self.index.lock().await;
        // // info!("indexes:{:?}", indexes.index);

        // // Get index safely
        // let index = indexes
        //     .index
        //     .iter()
        //     .find_map(|(k, v)| if *v == page_id { Some(*k) } else { None })
        //     .ok_or_else(|| anyhow::anyhow!("Page ID {} not found in index map", page_id))?;

        // info!("index: {}", index);

        // // Compute file offset
        // let file_offset =
        //     Metadata::size() + Index::size() + (index * PAGE_SIZE as u64) + offset as u64;

        // // Validate length
        // if length as usize > PAGE_SIZE as usize - offset as usize {
        //     return Err(anyhow::anyhow!(
        //         "Length {} exceeds page size {} at offset {}",
        //         length,
        //         PAGE_SIZE,
        //         offset
        //     ));
        // }

        // info!("Reading from storage at offset {}", file_offset);

        // let mut data = self.read(file_offset, length as usize).await?;
        // info!("Read {} bytes from storage", data.len());

        // if data.len() < MAX_DATA_IN_PACKET {
        //     data.resize(MAX_DATA_IN_PACKET, 0);
        // }

        // // Create response packet
        // let response_packet = Packet {
        //     meta: Meta {
        //         op: Operation::Poke,
        //         page_no: page_id,
        //         offset,
        //         length: data.len() as u32,
        //         chunk_seq: 0,
        //         total_chunks: 1,
        //     },
        //     data,
        // };

        // send_packets(sender, response_packet, stats.clone()).await?;
        info!("Sent peek response");

        Ok(())
    }

    pub async fn handle_cache(
        self,
        packet: Packet,
        sender: Arc<Mutex<SendStream>>,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<()> {
        let payload: PeekPayload = deserialize(&packet.data).unwrap();

        let pages = payload.pages;

        let mut res: HashMap<u64, PageDataType> = HashMap::new();

        for page in pages {
            let inode = self.get_inode(page).await?;

            res.insert(page, PageDataType::Inode(inode.clone()));

            if inode.is_directory {
                let page_data = self.get_directory_page(page).await?;

                res.insert(page, PageDataType::Directory(page_data));
                continue;
            }

            if inode.is_system_file {
                let data = self.read_page(page).await?;
                let content_bytes = &data[INODE_METADATA_SIZE as usize..];

                if let Ok(xentries) = deserialize::<XentriesPage>(content_bytes) {
                    res.insert(page, PageDataType::Xentries(xentries));
                } else if let Ok(pod_mapping) = deserialize::<PodMappingsPage>(content_bytes) {
                    res.insert(page, PageDataType::PodMappings(pod_mapping));
                } else {
                    log::error!("Unknown system File Data");
                }
            }
        }

        let payload = PeekResponse { response: res };

        let payload_bytes = serialize(&payload)?;

        let packet = Packet::new(0, 0, 0, AtlasOperation::Cache as i32, payload_bytes);

        send_packets(sender, packet, stats).await?;

        Ok(())
    }

    pub async fn handle_bigbang(
        self,
        fs_record: FileSystemRecord,
        inodes: Option<Vec<Inode>>,
    ) -> Result<()> {
        let mut inode_vec =
            inodes.ok_or_else(|| anyhow!("handle_bigbang requires a vector of 3 inodes"))?;
        if inode_vec.len() != 3 {
            return Err(anyhow!(
                "Expected exactly 3 inodes, but received {}",
                inode_vec.len()
            ));
        }

        inode_vec.sort_by_key(|i| i.inode_no);
        let root_inode = &inode_vec[0];
        let xentries_inode = &inode_vec[1];
        let pod_mappings_inode = &inode_vec[2];

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

        global_index.index.insert(root_page, root_local_index);
        global_index
            .index
            .insert(fs_record.xentries_start_page, xentries_local_index);
        global_index
            .index
            .insert(fs_record.pod_mappings_start_page, pod_mappings_local_index);

        self.write_object(root_inode, &[0u8], root_local_index)
            .await?;
        self.write_object(
            xentries_inode,
            &serialize(&xentries_content)?,
            xentries_local_index,
        )
        .await?;
        self.write_object(
            pod_mappings_inode,
            &serialize(&pod_mapping_content)?,
            pod_mappings_local_index,
        )
        .await?;

        global_meta.last_updated = Utc::now().timestamp() as u64;
        self.write(PAGE_SIZE, &global_meta.to_bytes()?).await?;
        self.write(PAGE_SIZE + Metadata::size(), &global_index.to_bytes()?)
            .await?;

        self.add_catalog_entry(fs_record).await?;

        // info!(
        //     "bigBang for fs_id {} completed successfully.",
        //     fs_record.fs_id
        // );

        Ok(())
    }

    pub async fn handle_quorum(
        self,
        sender: Arc<Mutex<SendStream>>,
        stats: Arc<Mutex<Stats>>,
    ) -> Result<()> {
        let catalogue = self.read_catalog().await?;

        let bytes = serialize(&catalogue).unwrap();

        let pkt = Packet::new(0, 0, 0, AtlasOperation::Quorum as i32, bytes);

        send_packets(sender, pkt, stats).await?;

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
        Ok(())
    }

    async fn read_catalog(self) -> Result<GlobalCatalogPage> {
        let mut file_handle = self.file.lock().await;
        file_handle.seek(SeekFrom::Start(0)).await?;

        let mut buffer = vec![0u8; PAGE_SIZE as usize];
        file_handle.read_exact(&mut buffer).await?;

        let end = buffer.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);

        if end == 0 {
            return Ok(GlobalCatalogPage {
                filesystems: vec![],
                next_catalog_page: 0,
            });
        }

        // deserialize(&buffer[..end]).context("Failed to deserialize Page 0")
        match deserialize(&buffer[..end]) {
            Ok(res) => Ok(res),
            Err(e) => {
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
            return Err(anyhow!(
                "Filesystem with fs_id {} already exists in the catalog.",
                fs_record.fs_id
            ));
        }
        catalog_page.filesystems.push(fs_record);
        self.write_catalog(catalog_page).await?;

        Ok(())
    }

    pub async fn write_catalog(&self, catalog_page: GlobalCatalogPage) -> Result<()> {
        let mut bytes = serialize(&catalog_page)?;
        bytes.resize(PAGE_SIZE as usize, 0);
        let mut file_handle = self.file.lock().await;
        file_handle.seek(SeekFrom::Start(0)).await?;
        file_handle.write_all(&bytes).await?;
        Ok(())
    }
}

impl Inode {
    pub fn new(inode_no: u64, ownership: String, is_directory: bool, is_system_file: bool) -> Self {
        Self {
            inode_no,
            ownership,
            created_timestamp: Utc::now().timestamp() as u64,
            modified_timestamp: Utc::now().timestamp() as u64,
            size: 0,
            is_directory,
            is_system_file,
            pages: Vec::new(),
        }
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
