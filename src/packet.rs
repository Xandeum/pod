use common::consts::PAGE_SIZE;
use log::{error, info};

pub use crate::protos::{AtlasOperation, Meta, Packet};
// use serde::{Deserialize, Serialize};

pub const PACKET_META_SIZE: usize = 29;
pub const MAX_DATA_IN_PACKET: usize = 1232;
pub const MAX_PACKET_SIZE: usize = PACKET_META_SIZE + MAX_DATA_IN_PACKET;

// // impl Packet

// // #[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
// // pub enum Operation {
// //     Poke,
// //     Peek,
// //     Handshake
// // }

// // #[derive(Serialize, Deserialize, Debug, Clone)]
// // pub struct Packet {
// //     pub meta: Meta,
// //     pub data: Vec<u8>,
// // }

// // #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
// // pub struct Meta {
// //     pub op: Operation,
// //     pub page_no: u64,
// //     pub offset: u32,
// //     pub length: u32,
// //     pub chunk_seq: u32,
// //     pub total_chunks: u32,
// // }

impl Packet {
    pub fn new(page_no: u64, offset: u32, length: u64, op: i32, data: Vec<u8>) -> Self {
        Packet {
            meta: Some(Meta {
                op,
                chunk_seq: 0,
                total_chunks: 1,
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
}

pub fn split_packet(packet: Packet) -> Vec<Packet> {
    if packet.meta.unwrap().op != AtlasOperation::PPoke as i32
        || packet.data.len() <= MAX_DATA_IN_PACKET
    {
        let mut data = packet.clone().data;
        data.resize(MAX_DATA_IN_PACKET, 0);
        return vec![Packet {
            meta: Some(Meta {
                chunk_seq: 0,
                total_chunks: 1,
                op: packet.meta.unwrap().op,
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

    for (i, chunk) in packet.data.chunks(MAX_DATA_IN_PACKET).enumerate() {
        let mut chunk_vec = chunk.to_vec();
        chunk_vec.resize(MAX_DATA_IN_PACKET, 0);

        let chunk_offset = 0;
        let chunk_packet = Packet::new_poke(0, chunk_offset, chunk_vec, i as u32, total_chunks);
        packets.push(chunk_packet);
    }
    packets
}

// TO DO : Fix This, Currently made changes to work with Packet protos
pub async fn reassemble_packets(mut packets: Vec<Packet>) -> Option<Packet> {
    if packets.is_empty() {
        error!("No packets to reassemble");
        return None;
    }
    let first = &packets[0];

    let expected_total_chunks = first.meta.unwrap().total_chunks;
    let expected_op = first.meta.unwrap().op;
    let length = first.meta.unwrap().length;

    packets.sort_by_key(|p| p.meta.unwrap().chunk_seq);

    let mut res = Vec::new();

    for (i, packet) in packets.iter().enumerate() {
        if packet.meta.unwrap().chunk_seq != i as u32 {
            error!("Missing or out or order packet");
            return None;
        }
        res.extend_from_slice(&packet.data);
    }
    res.truncate(PAGE_SIZE as usize);
    Some(Packet::new(0, 00, length, expected_op, res))
}
