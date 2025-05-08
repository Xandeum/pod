use log::{error, info};
use serde::{Deserialize, Serialize};

pub const PACKET_META_SIZE: usize = 36;
pub const MAX_DATA_IN_PACKET: usize = 1232;
pub const MAX_PACKET_SIZE: usize = PACKET_META_SIZE + MAX_DATA_IN_PACKET;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum Operation {
    Poke,
    Peek,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Packet {
    pub meta: Meta,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Meta {
    pub op: Operation,
    pub page_no: u64,
    pub offset: u32,
    pub length: u32,
    pub chunk_seq: u32,
    pub total_chunks: u32,
}

impl Packet {
    pub fn new(page_no: u64, offset: u32, length: u32, op: Operation, data: Vec<u8>) -> Self {
        Packet {
            meta: Meta {
                op,
                page_no,
                offset,
                length,
                chunk_seq: 0,
                total_chunks: 1,
            },
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
            meta: Meta {
                op: Operation::Poke,
                page_no,
                offset,
                length,
                chunk_seq,
                total_chunks,
            },
            data,
        }
    }

    pub fn new_peek(page_no: u64, offset: u32, length: u32) -> Self {
        Packet {
            meta: Meta {
                op: Operation::Peek,
                page_no,
                offset,
                length,
                chunk_seq: 0,
                total_chunks: 1,
            },
            data: Vec::new(),
        }
    }
}

pub fn split_packet(packet: Packet) -> Vec<Packet> {
    if packet.meta.op != Operation::Poke || packet.data.len() <= MAX_DATA_IN_PACKET {
        return vec![Packet {
            meta: Meta {
                chunk_seq: 0,
                total_chunks: 1,
                ..packet.meta
            },
            ..packet
        }];
    }

    let mut packets = Vec::new();
    let page_no = packet.meta.page_no;
    let base_offset = packet.meta.offset;
    let total_chunks = ((packet.data.len() as u64 + MAX_DATA_IN_PACKET as u64 - 1)
        / MAX_DATA_IN_PACKET as u64) as u32;

    for (i, chunk) in packet.data.chunks(MAX_DATA_IN_PACKET).enumerate() {
        let mut chunk_vec = chunk.to_vec();
        chunk_vec.resize(MAX_DATA_IN_PACKET, 0);

        let chunk_offset = base_offset;
        let chunk_packet =
            Packet::new_poke(page_no, chunk_offset, chunk_vec, i as u32, total_chunks);
        packets.push(chunk_packet);
    }
    packets
}

pub async fn reassemble_packets(mut packets: Vec<Packet>) -> Packet {
    let first = &packets[0];

    let expected_page_no = first.meta.page_no;
    let expected_total_chunks = first.meta.total_chunks;
    let expected_op = first.meta.op;
    let expected_offset = first.meta.offset;
    let length = first.meta.length;

    packets.sort_by_key(|p| p.meta.chunk_seq);

    let mut res = Vec::with_capacity(length as usize);

    for (i, packet) in packets.iter().enumerate() {
        if packet.meta.chunk_seq != i as u32 {
            error!("Missing or out or order packet");
        }
        res.extend_from_slice(&packet.data);
    }

    Packet::new(expected_page_no, expected_offset, length, expected_op, res)
}