//! Chunks generated in this process, in the shape the network layer hands the
//! game: the packet body is read exactly as `net::chunk_wire` reads it off the
//! wire, after repacking the one field whose codec changed in the
//! generator's protocol version (the light masks, see `repack_light_masks`).

use pomme_protocol::chunk::{self, ChunkPacket, LightData};
use pomme_protocol::codec::Reader;
use pomme_protocol::wire::write_varint;

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
    let payload = repack_light_masks(payload)?;
    let mut reader = Reader::new(&payload);
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

/// The generator speaks protocol 26.3, whose light masks are
/// `ByteBufCodecs.BIT_SET` byte strings where the native 26.2 reader expects
/// long arrays — the same change `net::translate` rewrites for a 26.3
/// server's packets. Repacks the four masks in place in the byte stream;
/// everything before and after them is byte-identical between the versions.
fn repack_light_masks(payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = Reader::new(payload);
    skip_to_light(&mut reader)?;

    let mut out = Vec::with_capacity(payload.len() + 32);
    out.extend_from_slice(&payload[..reader.pos()]);
    for _ in 0..4 {
        let len = reader.varint().map_err(|error| error.to_string())? as usize;
        let bytes = reader.bytes(len).map_err(|error| error.to_string())?;
        write_varint(&mut out, len.div_ceil(8) as u32);
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            out.extend_from_slice(&u64::from_le_bytes(word).to_be_bytes());
        }
    }
    out.extend_from_slice(&payload[reader.pos()..]);
    Ok(out)
}

/// Advances past the chunk position, heightmaps, section buffer and block
/// entities, leaving the reader at the light data.
fn skip_to_light(reader: &mut Reader<'_>) -> Result<(), String> {
    let err = |error: pomme_protocol::codec::DecodeError| error.to_string();
    reader.i32().map_err(err)?;
    reader.i32().map_err(err)?;
    reader
        .list(usize::MAX, |reader| {
            reader.varint()?;
            reader.long_array()?;
            Ok(())
        })
        .map_err(err)?;
    let sections = reader.varint().map_err(err)? as usize;
    reader.bytes(sections).map_err(err)?;
    reader
        .list(usize::MAX, |reader| {
            reader.u8()?;
            reader.i16()?;
            reader.varint()?;
            reader.nbt()?;
            Ok(())
        })
        .map_err(err)?;
    Ok(())
}
