//! Wire format 1: `SBW\0`, format byte 1, direction byte (1 client, 2 server),
//! then one Postcard value. No trailing bytes, compression, or partial frames.
//! Each typed packet must fit a single reliable, ordered host payload.

use crate::{
    limits,
    types::{ClientPacket, ServerPacket, Snapshot, SnapshotPage},
    validate::{Validate, ValidationError},
};
use serde::{Serialize, de::DeserializeOwned};
use std::fmt;

const MAGIC: &[u8; 4] = b"SBW\0";
const FORMAT: u8 = 1;
const HEADER: usize = 6;

#[derive(Debug)]
pub enum WireError {
    TooLarge,
    InvalidHeader,
    WrongDirection,
    TrailingBytes,
    InvalidValue(ValidationError),
    Codec(postcard::Error),
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WireError {}

fn encode<T: Serialize + Validate>(packet: &T, direction: u8) -> Result<Vec<u8>, WireError> {
    packet.validate().map_err(WireError::InvalidValue)?;
    let mut bytes = [0; limits::PACKET_BYTES];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4] = FORMAT;
    bytes[5] = direction;
    let len = postcard::to_slice(packet, &mut bytes[HEADER..])
        .map_err(|err| {
            if err == postcard::Error::SerializeBufferFull {
                WireError::TooLarge
            } else {
                WireError::Codec(err)
            }
        })?
        .len();
    Ok(bytes[..HEADER + len].to_vec())
}

fn decode<T: DeserializeOwned + Validate>(bytes: &[u8], direction: u8) -> Result<T, WireError> {
    if bytes.len() > limits::PACKET_BYTES {
        return Err(WireError::TooLarge);
    }
    if bytes.len() < HEADER || &bytes[..4] != MAGIC || bytes[4] != FORMAT {
        return Err(WireError::InvalidHeader);
    }
    if bytes[5] != direction {
        return Err(WireError::WrongDirection);
    }
    let (packet, rest) =
        postcard::take_from_bytes::<T>(&bytes[HEADER..]).map_err(WireError::Codec)?;
    if !rest.is_empty() {
        return Err(WireError::TrailingBytes);
    }
    packet.validate().map_err(WireError::InvalidValue)?;
    Ok(packet)
}

pub fn encode_client(packet: &ClientPacket) -> Result<Vec<u8>, WireError> {
    encode(packet, 1)
}
pub fn decode_client(bytes: &[u8]) -> Result<ClientPacket, WireError> {
    decode(bytes, 1)
}
pub fn encode_server(packet: &ServerPacket) -> Result<Vec<u8>, WireError> {
    encode(packet, 2)
}
pub fn decode_server(bytes: &[u8]) -> Result<ServerPacket, WireError> {
    decode(bytes, 2)
}

/// Splits a logical snapshot into independently encodable pages. Fails rather
/// than truncating the tracked set. Pages are applied atomically by SnapshotAssembler.
pub fn snapshot_pages(snapshot: &Snapshot) -> Result<Vec<SnapshotPage>, WireError> {
    snapshot.validate().map_err(WireError::InvalidValue)?;
    let mut current = SnapshotPage {
        session: snapshot.session,
        server_tick: snapshot.server_tick,
        acknowledged_input: snapshot.acknowledged_input,
        page: 0,
        pages: limits::SNAPSHOT_PAGES as u16,
        weapons: snapshot.weapons.clone(),
        entities: Vec::new(),
    };
    encode_server(&ServerPacket::Snapshot(current.clone()))?;
    let mut result = Vec::new();
    for entity in &snapshot.entities {
        current.entities.push(entity.clone());
        match encode_server(&ServerPacket::Snapshot(current.clone())) {
            Ok(_) => {}
            Err(WireError::TooLarge) => {
                current.entities.pop();
                result.push(current);
                current = SnapshotPage {
                    session: snapshot.session,
                    server_tick: snapshot.server_tick,
                    acknowledged_input: snapshot.acknowledged_input,
                    page: result.len() as u16,
                    pages: limits::SNAPSHOT_PAGES as u16,
                    weapons: Vec::new(),
                    entities: vec![entity.clone()],
                };
                encode_server(&ServerPacket::Snapshot(current.clone()))?;
            }
            Err(error) => return Err(error),
        }
    }
    result.push(current);
    if result.len() > limits::SNAPSHOT_PAGES {
        return Err(WireError::TooLarge);
    }
    let pages = result.len() as u16;
    for part in &mut result {
        part.pages = pages;
    }
    Ok(result)
}
