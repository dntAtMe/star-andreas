//! IMG archives, version 2 (`VER2` header, San Andreas).
//!
//! Layout: `"VER2"`, u32 entry count, then 32-byte entries:
//! u32 offset (sectors), u16 streaming size (sectors), u16 archive size
//! (sectors, usually 0), char name[24]. A sector is 2048 bytes.

use std::{collections::HashMap, fs::File, path::Path};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::bin::{Reader, cstr};

pub const SECTOR: usize = 2048;

#[derive(Debug, Clone)]
pub struct ImgEntry {
    pub name: String,
    pub offset: usize,
    pub size: usize,
}

pub struct Img {
    map: Mmap,
    entries: Vec<ImgEntry>,
    /// Lowercased name -> entry index.
    index: HashMap<String, usize>,
}

impl Img {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        // SAFETY: the game files are treated as read-only; nobody truncates them under us.
        let map = unsafe { Mmap::map(&file)? };
        let mut r = Reader::new(&map);
        if r.bytes(4)? != b"VER2" {
            bail!("{}: not a VER2 IMG archive", path.display());
        }
        let count = r.u32()? as usize;
        let mut entries = Vec::with_capacity(count);
        let mut index = HashMap::with_capacity(count);
        for i in 0..count {
            let offset = r.u32()? as usize * SECTOR;
            let streaming = r.u16()? as usize;
            let archive = r.u16()? as usize;
            let name = cstr(r.bytes(24)?);
            let size = if archive != 0 { archive } else { streaming } * SECTOR;
            if offset + size > map.len() {
                bail!("{}: entry {name} out of bounds", path.display());
            }
            index.insert(name.to_ascii_lowercase(), i);
            entries.push(ImgEntry { name, offset, size });
        }
        Ok(Self { map, entries, index })
    }

    pub fn entries(&self) -> &[ImgEntry] {
        &self.entries
    }

    pub fn entry(&self, name: &str) -> Option<&ImgEntry> {
        self.index.get(&name.to_ascii_lowercase()).map(|&i| &self.entries[i])
    }

    pub fn data(&self, e: &ImgEntry) -> &[u8] {
        &self.map[e.offset..e.offset + e.size]
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entry(name).map(|e| self.data(e))
    }
}
