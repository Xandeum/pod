use bincode::serialize;
use chrono;
use common::consts::PAGE_SIZE;
use log::{error, info};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer};

use crate::protos::HeartbeatPayload;
pub use crate::protos::{AtlasOperation, Meta, Packet};

pub const PACKET_META_SIZE: usize = 29;
pub const MAX_DATA_IN_PACKET: usize = 1232;
pub const MAX_PACKET_SIZE: usize = PACKET_META_SIZE + MAX_DATA_IN_PACKET;

impl Packet {
    pub fn new(chunk_seq: u32, total_chunks: u32, length: u64, op: i32, data: Vec<u8>) -> Self {
        Packet {
            meta: Some(Meta {
                op,
                chunk_seq: chunk_seq,
                total_chunks: total_chunks,
                length: length,
            }),
            data,
        }
    }
    pub fn new_poke(
        page_no: u64,
        offset: u32,
        data: Vec<u8>,
        chunk_seq: u32,
        total_chunks: u32,
    ) -> Self {
        let length = data.len() as u32;
        Packet {
            meta: Some(Meta {
                op: AtlasOperation::PPoke as i32,
                chunk_seq,
                total_chunks,
                length: 0,
            }),
            data,
        }
    }

    // pub fn new_peek(page_no: u64, offset: u32, length: u32) -> Self {
    //     Packet {
    //         meta: Meta {
    //             op: Operation::Peek,
    //             page_no,
    //             offset,
    //             length,
    //             chunk_seq: 0,
    //             total_chunks: 1,
    //         },
    //         data: [0u8; MAX_DATA_IN_PACKET].to_vec(),
    //     }
    // }
    pub fn new_handshake() -> Self {
        Packet {
            meta: Some(Meta {
                op: AtlasOperation::Handshake as i32,
                chunk_seq: 0,
                total_chunks: 1,
                length: 0,
            }),
            data: [0u8; MAX_DATA_IN_PACKET].to_vec(),
        }
    }

    pub fn new_version(version: String) -> Self {
        let version_bytes = version.as_bytes();
        let mut data = vec![0u8; MAX_DATA_IN_PACKET];

        // Copy version string to packet data
        let copy_len = std::cmp::min(version_bytes.len(), MAX_DATA_IN_PACKET);
        data[..copy_len].copy_from_slice(&version_bytes[..copy_len]);

        Packet {
            meta: Some(Meta {
                op: AtlasOperation::PVersion as i32,
                length: copy_len as u64,
                chunk_seq: 0,
                total_chunks: 1,
            }),
            data,
        }
    }

    pub fn new_heartbeat() -> Self {
        Packet {
            meta: Some(Meta {
                op: AtlasOperation::PHeartbeat as i32,
                length: 0,
                chunk_seq: 0,
                total_chunks: 1,
            }),
            data: [0u8; MAX_DATA_IN_PACKET].to_vec(),
        }
    }

    pub fn new_signed_heartbeat(keypair: &Keypair) -> Self {
        // Create heartbeat message to sign
        let heartbeat_msg = format!("heartbeat_{}", chrono::Utc::now().timestamp());
        let message_bytes = heartbeat_msg.as_bytes();

        // Sign the heartbeat message
        let signature = keypair.sign_message(message_bytes);

        // Create HeartbeatPayload with structured data
        let heartbeat_payload = HeartbeatPayload {
            pubkey: keypair.pubkey().to_string(),
            signature: signature.to_string(),
            message: heartbeat_msg,
        };

        // Serialize the HeartbeatPayload to JSON bytes
        match serialize(&heartbeat_payload) {
            Ok(payload_bytes) => {
                let mut data = vec![0u8; MAX_DATA_IN_PACKET];

                if payload_bytes.len() <= MAX_DATA_IN_PACKET {
                    // Copy serialized payload to packet data
                    data[0..payload_bytes.len()].copy_from_slice(&payload_bytes);

                    Packet {
                        meta: Some(Meta {
                            op: AtlasOperation::PHeartbeat as i32,
                            length: payload_bytes.len() as u64,
                            chunk_seq: 0,
                            total_chunks: 1,
                        }),
                        data,
                    }
                } else {
                    error!(
                        "Signed heartbeat payload too large: {} bytes > {}",
                        payload_bytes.len(),
                        MAX_DATA_IN_PACKET
                    );
                    Self::new_heartbeat()
                }
            }
            Err(e) => {
                error!("Failed to serialize HeartbeatPayload: {}", e);
                Self::new_heartbeat()
            }
        }
    }

    /// Creates a success response packet
    pub fn new_success_response(

    ) -> Self {
        Packet {
            meta: Some(Meta {
                op: AtlasOperation::PSuccessResponse as i32,
                length: 0 as u64,
                chunk_seq: 0,
                total_chunks: 1,
            }),
            data: vec![0u8; MAX_DATA_IN_PACKET],
        }
    }

    /// Creates an error response packet
    pub fn new_error_response(
    ) -> Self {
        Packet {
            meta: Some(Meta {
                op: AtlasOperation::PErrorResponse as i32,
                length: 0 as u64,
                chunk_seq: 0,
                total_chunks: 1,
            }),
            data: vec![0u8; MAX_DATA_IN_PACKET],
        }
    }

    /// Decodes and verifies a HeartbeatPayload from packet data
    pub fn decode_heartbeat_payload(
        packet_data: &[u8],
        length: u64,
    ) -> Result<HeartbeatPayload, String> {
        // Extract the payload bytes based on the length
        if length == 0 || length as usize > packet_data.len() {
            return Err("Invalid heartbeat payload length".to_string());
        }

        let payload_bytes = &packet_data[0..length as usize];

        // Deserialize the HeartbeatPayload using bincode
        match bincode::deserialize(payload_bytes) {
            Ok(heartbeat_payload) => Ok(heartbeat_payload),
            Err(e) => Err(format!("Failed to decode HeartbeatPayload: {}", e)),
        }
    }

    pub fn verify_heartbeat_payload(payload: &HeartbeatPayload) -> Result<bool, String> {
        use std::str::FromStr;

        let pubkey =
            Pubkey::from_str(&payload.pubkey).map_err(|e| format!("Invalid public key: {}", e))?;

        let signature = Signature::from_str(&payload.signature)
            .map_err(|e| format!("Invalid signature: {}", e))?;

        let message_bytes = payload.message.as_bytes();
        Ok(signature.verify(pubkey.as_ref(), message_bytes))
    }
}

pub fn split_packet(packet: Packet) -> Vec<Packet> {
    // if packet.meta.unwrap().op != AtlasOperation::PPoke as i32
    // || packet.data.len() <= MAX_DATA_IN_PACKET
    if packet.data.len() <= MAX_DATA_IN_PACKET {
        let mut data = packet.clone().data;
        data.resize(MAX_DATA_IN_PACKET, 0);
        return vec![Packet {
            meta: Some(Meta {
                chunk_seq: 0,
                total_chunks: 1,
                op: packet.meta.as_ref().map(|m| m.op).unwrap_or(0),
                length: packet.data.len() as u64,
            }),
            data: data,
        }];
    }

    let mut packets = Vec::new();
    // Note : This offset is used for Peek/Poke of a Page, It does not
    // relate to offset of split data.The Split data order is only determined by the chunk_seq.
    let total_chunks = ((packet.data.len() as u64 + MAX_DATA_IN_PACKET as u64 - 1)
        / MAX_DATA_IN_PACKET as u64) as u32;

    let pkt = packet.clone();

    for (i, chunk) in packet.data.clone().chunks(MAX_DATA_IN_PACKET).enumerate() {
        let data = pkt.data.clone();
        let mut chunk_vec = chunk.to_vec();
        chunk_vec.resize(MAX_DATA_IN_PACKET, 0);

        // let chunk_offset = 0;
        let chunk_packet = Packet::new(
            i as u32,
            total_chunks,
            pkt.data.clone().len() as u64,
            packet.meta.as_ref().map(|m| m.op).unwrap_or(0),
            chunk_vec,
        );
        packets.push(chunk_packet);
    }
    info!("packets : {:?}", packets);
    packets
}

pub async fn reassemble_packets(mut packets: Vec<Packet>) -> Option<Packet> {
    if packets.is_empty() {
        error!("No packets to reassemble");
        return None;
    }
    let first = &packets[0];

    let expected_total_chunks = first.meta.as_ref().map(|m| m.total_chunks).unwrap_or(1);
    let expected_op = first.meta.as_ref().map(|m| m.op).unwrap_or(0);
    let length = first.meta.as_ref().map(|m| m.length).unwrap_or(0);

    packets.sort_by_key(|p| p.meta.as_ref().map(|m| m.chunk_seq).unwrap_or(0));

    let mut res = Vec::new();

    for (i, packet) in packets.iter().enumerate() {
        if packet.meta.as_ref().map(|m| m.chunk_seq).unwrap_or(0) != i as u32 {
            error!("Missing or out or order packet");
            return None;
        }
        res.extend_from_slice(&packet.data);
    }
    // Fix: Use actual length instead of PAGE_SIZE
    if length > 0 {
        res.truncate(length as usize);
    }
    Some(Packet::new(0, 0, length, expected_op, res))
}
