//! Chunks generated in this process, in the shape the network layer hands the
//! game: the packet body is read exactly as `net::chunk_wire` reads it off the
//! wire, with nothing in between.

use pomme_protocol::chunk::{self, ChunkPacket, LightData};
use pomme_protocol::codec::Reader;

use super::chunk::HeightmapKind;
use crate::position::ChunkPos;

/// A generated column's data, ready for `ChunkStore::load_chunk`.
pub struct GeneratedChunk {
    pub pos: ChunkPos,
    /// The packed sections, biomes included.
    pub sections: Box<[u8]>,
    pub heightmaps: Vec<(HeightmapKind, Box<[u64]>)>,
    pub light: LightData,
}

/// Decodes the body of a `level_chunk_with_light` packet.
pub fn decode(payload: &[u8]) -> Result<GeneratedChunk, String> {
    let mut reader = Reader::new(payload);
    let packet = chunk::read("level_chunk_with_light", &mut reader)
        .ok_or("not a level chunk packet")?
        .map_err(|error| error.to_string())?;
    reader.ensure_eof().map_err(|error| error.to_string())?;
    match packet {
        ChunkPacket::Level {
            x,
            z,
            heightmaps,
            sections,
            light,
            ..
        } => Ok(GeneratedChunk {
            pos: ChunkPos::new(x, z),
            sections,
            heightmaps: heightmaps
                .into_iter()
                .filter_map(|(id, data)| Some((HeightmapKind::from_id(id)?, data)))
                .collect(),
            light,
        }),
        _ => Err("not a level chunk packet".to_owned()),
    }
}
