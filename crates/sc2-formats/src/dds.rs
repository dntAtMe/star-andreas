//! Minimal DDS container parsing (no decoding).
//!
//! SC2 texture streaming: textures larger than 64px usually have a `foo.lvl0`
//! next to `foo.dds` (same name, extension replaced). The `.dds` always holds the
//! **full** mip chain; the `.lvl0` is a standalone DDS of just the low-res tail
//! of that chain (top mip <= 64px, e.g. 64x32 of a 512x256 diffuse, cube maps
//! included), byte-identical to the `.dds` mips — a preview loaded first / used
//! at low texture quality. In build 5.0.16 every `.lvl0` has its `.dds`, so the
//! `.dds` alone is enough; [`best`] falls back to the `.lvl0` if needed. Some
//! `.dds` files (normal maps) carry 4 trailing bytes after the last mip.

use anyhow::{Context, Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Rgba8,
    Bgra8,
    /// Legacy FourCC (or the raw pixel format flags if none) or, for DX10
    /// headers, the DXGI format number.
    Other(u32),
}

impl Format {
    /// Bytes per 4x4 block for BCn, per pixel for 8-bit RGBA; `None` if unknown.
    pub fn block_bytes(self) -> Option<usize> {
        match self {
            Format::Bc1 | Format::Bc4 => Some(8),
            Format::Bc2 | Format::Bc3 | Format::Bc5 | Format::Bc7 => Some(16),
            Format::Rgba8 | Format::Bgra8 => Some(4),
            Format::Other(_) => None,
        }
    }

    pub fn is_compressed(self) -> bool {
        !matches!(self, Format::Rgba8 | Format::Bgra8 | Format::Other(_))
    }
}

#[derive(Clone, Debug)]
pub struct Dds<'a> {
    pub width: u32,
    pub height: u32,
    pub mip_count: u32,
    pub format: Format,
    /// Cube map: `data` is 6 faces, each with its full mip chain.
    pub cube: bool,
    /// Everything after the header(s): all mips (and faces), largest first.
    pub data: &'a [u8],
}

impl<'a> Dds<'a> {
    pub fn parse(d: &'a [u8]) -> Result<Self> {
        ensure!(d.len() >= 128 && &d[..4] == b"DDS ", "not a DDS file");
        let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        ensure!(u(4) == 124, "bad DDS header size {}", u(4));
        let (height, width, mips) = (u(12), u(16), u(28));
        let (pf_flags, fourcc, bits, rmask) = (u(80), u(84), u(88), u(92));
        let cube = u(112) & 0x200 != 0;
        let mut off = 128;
        let format = if pf_flags & 0x4 != 0 {
            match &fourcc.to_le_bytes() {
                b"DXT1" => Format::Bc1,
                b"DXT2" | b"DXT3" => Format::Bc2,
                b"DXT4" | b"DXT5" => Format::Bc3,
                b"ATI1" | b"BC4U" => Format::Bc4,
                b"ATI2" | b"BC5U" => Format::Bc5,
                b"DX10" => {
                    ensure!(d.len() >= 148, "truncated DX10 header");
                    off = 148;
                    match u(128) {
                        70..=72 => Format::Bc1,
                        73..=75 => Format::Bc2,
                        76..=78 => Format::Bc3,
                        79..=81 => Format::Bc4,
                        82..=84 => Format::Bc5,
                        97..=99 => Format::Bc7,
                        28 | 29 => Format::Rgba8,
                        87 | 91 => Format::Bgra8,
                        f => Format::Other(f),
                    }
                }
                _ => Format::Other(fourcc),
            }
        } else if pf_flags & 0x40 != 0 && bits == 32 {
            match rmask {
                0xff => Format::Rgba8,
                0xff0000 => Format::Bgra8,
                _ => Format::Other(pf_flags),
            }
        } else {
            Format::Other(pf_flags)
        };
        ensure!(width > 0 && height > 0, "empty texture");
        Ok(Self { width, height, mip_count: mips.max(1), format, cube, data: &d[off..] })
    }

    /// Byte size of mip `level` (one face), if the format is known.
    pub fn mip_size(&self, level: u32) -> Option<usize> {
        let (w, h) = ((self.width >> level).max(1) as usize, (self.height >> level).max(1) as usize);
        let b = self.format.block_bytes()?;
        Some(if self.format.is_compressed() { w.div_ceil(4) * h.div_ceil(4) * b } else { w * h * b })
    }

    /// `(width, height, bytes)` of each mip present in `data` (first face only for
    /// cube maps); stops early if the data is truncated.
    pub fn mips(&self) -> Vec<(u32, u32, &'a [u8])> {
        let mut out = Vec::new();
        let mut off = 0;
        for l in 0..self.mip_count {
            let Some(n) = self.mip_size(l) else { break };
            let Some(m) = self.data.get(off..off + n) else { break };
            out.push(((self.width >> l).max(1), (self.height >> l).max(1), m));
            off += n;
        }
        out
    }
}

/// Name of the low-res sibling of a `.dds` path (`a/b.dds` -> `a/b.lvl0`).
pub fn lvl0_name(dds_path: &str) -> String {
    let stem = dds_path.len() - if dds_path.to_ascii_lowercase().ends_with(".dds") { 4 } else { 0 };
    format!("{}.lvl0", &dds_path[..stem])
}

/// Picks the best available image: the full `.dds` if it parses with all its
/// mips, otherwise the `.lvl0` (if any).
pub fn best<'a>(dds: Option<&'a [u8]>, lvl0: Option<&'a [u8]>) -> Result<Dds<'a>> {
    let full = dds.map(Dds::parse).transpose();
    let low = lvl0.map(Dds::parse).transpose();
    match (full, low) {
        (Ok(Some(f)), Ok(Some(l))) => {
            let complete = f.mips().len() as u32 == f.mip_count || f.format.block_bytes().is_none();
            Ok(if complete { f } else { l })
        }
        (Ok(Some(f)), _) => Ok(f),
        (_, Ok(Some(l))) => Ok(l),
        (Err(e), _) => Err(e).context(".dds"),
        (_, Err(e)) => Err(e).context(".lvl0"),
        (Ok(None), Ok(None)) => bail!("no texture data"),
    }
}
