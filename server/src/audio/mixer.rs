// Audio mixer / relay
// For raw PCM, we simply relay packets from sender to all room members.
// The UDP server handles this directly. This module provides utilities
// for potential future mixing (e.g., combining multiple streams).

pub const HEADER_SIZE: usize = 22;
pub const MAGIC: [u8; 4] = [0x54, 0x43, 0x4F, 0x4E];

pub struct AudioPacketHeader {
    pub token: u32,
    pub sequence: u32,
    pub timestamp_ms: u32,
    pub sample_rate: u16,
    pub bit_depth: u8,
    pub channels: u8,
    pub payload_length: u16,
}

impl AudioPacketHeader {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }

        if data[0..4] != MAGIC {
            return None;
        }

        Some(Self {
            token: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            sequence: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            timestamp_ms: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            sample_rate: u16::from_le_bytes([data[16], data[17]]),
            bit_depth: data[18],
            channels: data[19],
            payload_length: u16::from_le_bytes([data[20], data[21]]),
        })
    }

    pub fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let end = (HEADER_SIZE + self.payload_length as usize).min(data.len());
        &data[HEADER_SIZE..end]
    }
}
