//! TXD texture dictionaries (PC / Direct3D 9 native rasters).

use anyhow::{Result, bail};

use crate::{
    bin::Reader,
    rw::{self, id},
};

/// Pixel layout of a decoded texture. Uncompressed formats are expanded to RGBA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rgba8,
    Dxt1,
    Dxt3,
    Dxt5,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub mask: String,
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub has_alpha: bool,
    /// Raw RW filter mode (low byte) and addressing (next byte).
    pub filter_flags: u32,
    /// Mip levels, largest first.
    pub mips: Vec<Vec<u8>>,
}

const FOURCC_DXT1: u32 = u32::from_le_bytes(*b"DXT1");
const FOURCC_DXT3: u32 = u32::from_le_bytes(*b"DXT3");
const FOURCC_DXT5: u32 = u32::from_le_bytes(*b"DXT5");

mod raster {
    pub const C1555: u32 = 0x100;
    pub const C565: u32 = 0x200;
    pub const C4444: u32 = 0x300;
    pub const LUM8: u32 = 0x400;
    pub const C8888: u32 = 0x500;
    pub const C888: u32 = 0x600;
    pub const C555: u32 = 0xA00;
    pub const PAL8: u32 = 0x2000;
    pub const PAL4: u32 = 0x4000;
}

pub fn parse(data: &[u8]) -> Result<Vec<Texture>> {
    let mut r = Reader::new(data);
    let (_, mut dict) = rw::sub(&mut r, id::TEX_DICTIONARY)?;
    let (_, mut s) = rw::sub(&mut dict, id::STRUCT)?;
    let count = s.u16()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let (_, mut native) = rw::sub(&mut dict, id::TEXTURE_NATIVE)?;
        let (_, body) = rw::sub(&mut native, id::STRUCT)?;
        out.push(parse_native(body)?);
    }
    Ok(out)
}

fn parse_native(mut s: Reader) -> Result<Texture> {
    let platform = s.u32()?;
    if platform != 9 {
        bail!("unsupported TXD platform {platform} (only PC D3D9)");
    }
    let filter_flags = s.u32()?;
    let name = s.fixed_str(32)?;
    let mask = s.fixed_str(32)?;
    let raster_format = s.u32()?;
    let d3d_format = s.u32()?;
    let width = s.u16()? as u32;
    let height = s.u16()? as u32;
    let _depth = s.u8()?;
    let num_levels = s.u8()? as usize;
    let _raster_type = s.u8()?;
    let flags = s.u8()?;
    let mut has_alpha = flags & 1 != 0;

    let palette: Option<Vec<[u8; 4]>> = if raster_format & raster::PAL8 != 0 {
        Some((0..256).map(|_| Ok([s.u8()?, s.u8()?, s.u8()?, s.u8()?])).collect::<Result<_>>()?)
    } else if raster_format & raster::PAL4 != 0 {
        Some((0..16).map(|_| Ok([s.u8()?, s.u8()?, s.u8()?, s.u8()?])).collect::<Result<_>>()?)
    } else {
        None
    };

    let format = match d3d_format {
        FOURCC_DXT1 => Format::Dxt1,
        FOURCC_DXT3 => Format::Dxt3,
        FOURCC_DXT5 => Format::Dxt5,
        _ => Format::Rgba8,
    };

    let mut mips = Vec::with_capacity(num_levels);
    for level in 0..num_levels {
        if s.remaining() < 4 {
            break;
        }
        let size = s.u32()? as usize;
        let raw = s.bytes(size)?;
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        if format != Format::Rgba8 {
            mips.push(raw.to_vec());
            continue;
        }
        let px = (w * h) as usize;
        let rgba = if let Some(pal) = &palette {
            let idx = |i: usize| -> u8 {
                if raster_format & raster::PAL4 != 0 {
                    // 4-bit indices, low nibble first.
                    let b = raw.get(i / 2).copied().unwrap_or(0);
                    if i % 2 == 0 { b & 0xF } else { b >> 4 }
                } else {
                    raw.get(i).copied().unwrap_or(0)
                }
            };
            (0..px).flat_map(|i| pal[idx(i) as usize % pal.len()]).collect()
        } else {
            decode_uncompressed(raster_format & 0xF00, raw, px)?
        };
        mips.push(rgba);
    }
    if raster_format & 0xF00 == raster::C888 {
        has_alpha = false;
    }
    Ok(Texture { name, mask, width, height, format, has_alpha, filter_flags, mips })
}

fn decode_uncompressed(fmt: u32, raw: &[u8], px: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(px * 4);
    let need = |bpp: usize| -> Result<()> {
        if raw.len() < px * bpp {
            bail!("raster too small: {} < {}", raw.len(), px * bpp);
        }
        Ok(())
    };
    let x5 = |v: u16| ((v as u32 * 255 + 15) / 31) as u8;
    let x6 = |v: u16| ((v as u32 * 255 + 31) / 63) as u8;
    let x4 = |v: u16| (v as u8) * 17;
    match fmt {
        raster::C8888 | raster::C888 => {
            need(4)?;
            for p in raw[..px * 4].chunks_exact(4) {
                let a = if fmt == raster::C888 { 255 } else { p[3] };
                out.extend_from_slice(&[p[2], p[1], p[0], a]);
            }
        }
        raster::C565 | raster::C1555 | raster::C555 | raster::C4444 => {
            need(2)?;
            for p in raw[..px * 2].chunks_exact(2) {
                let v = u16::from_le_bytes([p[0], p[1]]);
                let rgba = match fmt {
                    raster::C565 => [x5(v >> 11), x6((v >> 5) & 0x3F), x5(v & 0x1F), 255],
                    raster::C4444 => [x4((v >> 8) & 0xF), x4((v >> 4) & 0xF), x4(v & 0xF), x4(v >> 12)],
                    _ => {
                        let a = if fmt == raster::C1555 && v & 0x8000 == 0 { 0 } else { 255 };
                        [x5((v >> 10) & 0x1F), x5((v >> 5) & 0x1F), x5(v & 0x1F), a]
                    }
                };
                out.extend_from_slice(&rgba);
            }
        }
        raster::LUM8 => {
            need(1)?;
            for &l in &raw[..px] {
                out.extend_from_slice(&[l, l, l, 255]);
            }
        }
        _ => bail!("unsupported raster format {fmt:#x}"),
    }
    Ok(out)
}

/// Decode one DXT mip level to RGBA8 (CPU fallback / tooling).
pub fn decode_dxt(format: Format, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let block_size = if format == Format::Dxt1 { 8 } else { 16 };
    let bw = w.div_ceil(4);
    for (bi, block) in data.chunks_exact(block_size).enumerate() {
        let (bx, by) = ((bi % bw) * 4, (bi / bw) * 4);
        if by >= h {
            break;
        }
        let (alpha_part, color) = block.split_at(block_size - 8);
        let c0 = u16::from_le_bytes([color[0], color[1]]);
        let c1 = u16::from_le_bytes([color[2], color[3]]);
        let rgb = |c: u16| {
            [
                ((c >> 11) as u32 * 255 / 31) as u8,
                (((c >> 5) & 0x3F) as u32 * 255 / 63) as u8,
                ((c & 0x1F) as u32 * 255 / 31) as u8,
            ]
        };
        let (a, b) = (rgb(c0), rgb(c1));
        let lerp = |x: u8, y: u8, n: u32, d: u32| ((x as u32 * (d - n) + y as u32 * n) / d) as u8;
        let mut pal = [[0u8; 4]; 4];
        pal[0] = [a[0], a[1], a[2], 255];
        pal[1] = [b[0], b[1], b[2], 255];
        if c0 > c1 || format != Format::Dxt1 {
            pal[2] = [lerp(a[0], b[0], 1, 3), lerp(a[1], b[1], 1, 3), lerp(a[2], b[2], 1, 3), 255];
            pal[3] = [lerp(a[0], b[0], 2, 3), lerp(a[1], b[1], 2, 3), lerp(a[2], b[2], 2, 3), 255];
        } else {
            pal[2] = [lerp(a[0], b[0], 1, 2), lerp(a[1], b[1], 1, 2), lerp(a[2], b[2], 1, 2), 255];
            pal[3] = [0, 0, 0, 0];
        }
        let bits = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
        let alphas: [u8; 16] = match format {
            Format::Dxt3 => std::array::from_fn(|i| ((alpha_part[i / 2] >> (4 * (i % 2))) & 0xF) * 17),
            Format::Dxt5 => {
                let (a0, a1) = (alpha_part[0] as u32, alpha_part[1] as u32);
                let mut ab = [0u8; 8];
                ab[..6].copy_from_slice(&alpha_part[2..8]);
                let abits = u64::from_le_bytes(ab);
                std::array::from_fn(|i| {
                    let code = ((abits >> (3 * i)) & 7) as u32;
                    (match (code, a0 > a1) {
                        (0, _) => a0,
                        (1, _) => a1,
                        (c, true) => (a0 * (7 - (c - 1)) + a1 * (c - 1)) / 7,
                        (6, false) => 0,
                        (7, false) => 255,
                        (c, false) => (a0 * (5 - (c - 1)) + a1 * (c - 1)) / 5,
                    }) as u8
                })
            }
            _ => [255; 16],
        };
        for i in 0..16 {
            let (x, y) = (bx + i % 4, by + i / 4);
            if x >= w || y >= h {
                continue;
            }
            let mut p = pal[((bits >> (2 * i)) & 3) as usize];
            if format != Format::Dxt1 {
                p[3] = alphas[i];
            }
            out[(y * w + x) * 4..][..4].copy_from_slice(&p);
        }
    }
    out
}
