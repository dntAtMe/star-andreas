//! RenderWare binary stream chunks.
//!
//! Every chunk starts with a 12-byte header: u32 type, u32 payload size,
//! u32 library id stamp (encodes the RW version).

use anyhow::{Result, bail};

use crate::bin::{Reader, cstr};

pub mod id {
    pub const STRUCT: u32 = 0x01;
    pub const STRING: u32 = 0x02;
    pub const EXTENSION: u32 = 0x03;
    pub const TEXTURE: u32 = 0x06;
    pub const MATERIAL: u32 = 0x07;
    pub const MATERIAL_LIST: u32 = 0x08;
    pub const FRAME_LIST: u32 = 0x0E;
    pub const GEOMETRY: u32 = 0x0F;
    pub const CLUMP: u32 = 0x10;
    pub const ATOMIC: u32 = 0x14;
    pub const TEXTURE_NATIVE: u32 = 0x15;
    pub const TEX_DICTIONARY: u32 = 0x16;
    pub const GEOMETRY_LIST: u32 = 0x1A;
    pub const SKIN: u32 = 0x116;
    pub const HANIM: u32 = 0x11E;
    pub const BIN_MESH: u32 = 0x50E;
    pub const EFFECT_2D: u32 = 0x0253_F2F8;
    pub const EXTRA_VERT_COLOUR: u32 = 0x0253_F2F9;
    pub const COLLISION: u32 = 0x0253_F2FA;
    pub const BREAKABLE: u32 = 0x0253_F2FD;
    pub const NODE_NAME: u32 = 0x0253_F2FE;
}

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub ty: u32,
    pub size: usize,
    pub version: u32,
}

/// Decode the library id stamp into a version like `0x36003`.
pub fn decode_version(libid: u32) -> u32 {
    if libid & 0xFFFF_0000 != 0 {
        ((libid >> 14 & 0x3_FF00) + 0x30000) | (libid >> 16 & 0x3F)
    } else {
        libid << 8
    }
}

pub fn header(r: &mut Reader) -> Result<Header> {
    let ty = r.u32()?;
    let size = r.u32()? as usize;
    let version = decode_version(r.u32()?);
    if size > r.remaining() {
        bail!("chunk {ty:#x} size {size} exceeds remaining {}", r.remaining());
    }
    Ok(Header { ty, size, version })
}

/// Read the next chunk header and require it to be `ty`.
pub fn expect(r: &mut Reader, ty: u32) -> Result<Header> {
    let h = header(r)?;
    if h.ty != ty {
        bail!("expected chunk {ty:#x}, found {:#x} at {}", h.ty, r.pos() - 12);
    }
    Ok(h)
}

/// Read a chunk of `ty` and return a reader over just its payload.
pub fn sub<'a>(r: &mut Reader<'a>, ty: u32) -> Result<(Header, Reader<'a>)> {
    let h = expect(r, ty)?;
    Ok((h, Reader::new(r.bytes(h.size)?)))
}

pub fn string(r: &mut Reader) -> Result<String> {
    let h = expect(r, id::STRING)?;
    Ok(cstr(r.bytes(h.size)?))
}

/// Iterate children of an extension (or any container) payload.
pub fn children<'a>(mut r: Reader<'a>) -> impl Iterator<Item = Result<(Header, Reader<'a>)>> {
    std::iter::from_fn(move || {
        if r.remaining() < 12 {
            return None;
        }
        Some(header(&mut r).and_then(|h| Ok((h, Reader::new(r.bytes(h.size)?)))))
    })
}
