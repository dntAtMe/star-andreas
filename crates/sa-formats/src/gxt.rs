//! GXT text tables (`CText`, text.md §1): the TABL chunk of mission tables, the main table's
//! TKEY (offset, key hash) / TDAT (NUL-terminated 8-bit strings) and the mission tables.
//! Keys are hashed with `CKeyGen::GetUppercaseKey` (CRC32 with init 0xFFFFFFFF over the
//! upper-cased key, no final xor).

use std::collections::HashMap;

/// One loaded key table: hash → byte range of its string in `data`.
#[derive(Debug, Clone, Default)]
pub struct KeyTable {
    keys: HashMap<u32, (u32, u32)>,
    data: Vec<u8>,
}

impl KeyTable {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn get(&self, hash: u32) -> Option<&[u8]> {
        let &(s, e) = self.keys.get(&hash)?;
        Some(&self.data[s as usize..e as usize])
    }
}

/// `CText`: the main table, the mission table offsets and the loaded mission table.
#[derive(Debug, Clone, Default)]
pub struct Gxt {
    pub main: KeyTable,
    /// TABL entries (name, file offset); entry 0 is "MAIN".
    pub tables: Vec<(String, u32)>,
    pub mission: Option<(String, KeyTable)>,
}

/// `CKeyGen::GetUppercaseKey` (0x53CF30).
pub fn key_hash(key: &[u8]) -> u32 {
    let mut h = 0xFFFF_FFFFu32;
    for &c in key {
        if c == 0 {
            break;
        }
        h = CRC_TABLE[((c.to_ascii_uppercase() as u32 ^ h) & 0xFF) as usize] ^ (h >> 8);
    }
    h
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn chunk(d: &[u8], off: usize) -> Option<([u8; 4], usize, usize)> {
    let id: [u8; 4] = d.get(off..off + 4)?.try_into().ok()?;
    let size = u32::from_le_bytes(d.get(off + 4..off + 8)?.try_into().ok()?) as usize;
    Some((id, size, off + 8))
}

/// The TKEY / TDAT chunk loop from `off` (other chunks skipped).
fn load_table(d: &[u8], mut off: usize, read_tabl: Option<&mut Vec<(String, u32)>>) -> Option<KeyTable> {
    let mut tabl = read_tabl;
    let (mut keys, mut data) = (None, None);
    while keys.is_none() || data.is_none() {
        let (id, size, body) = chunk(d, off)?;
        let payload = d.get(body..body + size)?;
        match &id {
            b"TABL" => {
                if let Some(t) = tabl.as_deref_mut() {
                    for e in payload.chunks_exact(12) {
                        let name = String::from_utf8_lossy(&e[..8]).trim_end_matches('\0').to_string();
                        t.push((name, u32::from_le_bytes(e[8..12].try_into().unwrap())));
                    }
                }
            }
            b"TKEY" => keys = Some(payload),
            b"TDAT" => data = Some(payload),
            _ => {}
        }
        off = body + size;
    }
    let (keys, data) = (keys?, data?);
    let mut out = KeyTable { keys: HashMap::with_capacity(keys.len() / 8), data: data.to_vec() };
    for e in keys.chunks_exact(8) {
        let start = u32::from_le_bytes(e[0..4].try_into().unwrap());
        let hash = u32::from_le_bytes(e[4..8].try_into().unwrap());
        let s = (start as usize).min(data.len());
        let end = s + data[s..].iter().position(|&b| b == 0).unwrap_or(data.len() - s);
        out.keys.insert(hash, (s as u32, end as u32));
    }
    Some(out)
}

impl Gxt {
    /// `CText::Load` (0x6A01A0): the header (version, bits per char; ignored) and the main table.
    pub fn parse(d: &[u8]) -> Option<Self> {
        let mut tables = Vec::new();
        let main = load_table(d, 4, Some(&mut tables))?;
        Some(Self { main, tables, mission: None })
    }

    /// `CText::LoadMissionText` (0x69FBF0): the named table (skipping its 8-byte name).
    pub fn load_mission(&mut self, d: &[u8], name: &str) {
        self.mission = None;
        let Some(&(_, off)) = self.tables.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) else { return };
        if let Some(t) = load_table(d, off as usize + 8, None) {
            self.mission = Some((name.to_string(), t));
        }
    }

    /// `CText::Get` (0x6A0050): main table first, then the mission table; a missing key (or one
    /// starting with a space / NUL) gives None (the shared empty string).
    pub fn lookup(&self, key: &str) -> Option<(u32, &[u8])> {
        let k = key.as_bytes();
        if k.first().is_none_or(|&c| c == b' ' || c == 0) {
            return None;
        }
        let h = key_hash(k);
        if let Some(s) = self.main.get(h) {
            return Some((h, s));
        }
        self.mission.as_ref().and_then(|(_, t)| t.get(h)).map(|s| (h, s))
    }

    /// The string of a key hash (main table first).
    pub fn lookup_hash(&self, h: u32) -> Option<&[u8]> {
        self.main.get(h).or_else(|| self.mission.as_ref().and_then(|(_, t)| t.get(h)))
    }

    /// The text of `key`, empty when missing.
    pub fn get(&self, key: &str) -> &[u8] {
        self.lookup(key).map_or(&[], |(_, s)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        assert_eq!(key_hash(b"CDERROR"), 0xEF12_8EC3);
        assert_eq!(key_hash(b"GAN"), 0x0D6D_AF7B);
        assert_eq!(key_hash(b"gan"), 0x0D6D_AF7B);
        assert_eq!(key_hash(b"BURRITO"), 0x896C_F409);
    }
}
