//! Local CASC storage (`<game>/SC2Data`): `.build.info` -> build config ->
//! `data/*.idx` (encoding key -> archive location) -> `data/data.NNN` (BLTE blobs)
//! -> encoding table (content key -> encoding key).
//!
//! Name -> content key resolution lives in the root file, see [`crate::root`].

use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, Result, bail, ensure};
use memmap2::Mmap;

use crate::blte;

pub type Key = [u8; 16];

pub fn parse_key(hex: &str) -> Result<Key> {
    ensure!(hex.len() == 32, "bad key {hex:?}");
    let mut k = [0; 16];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(k)
}

pub fn key_hex(k: &[u8]) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug)]
struct Loc {
    archive: u16,
    offset: u32,
    size: u32,
}

pub struct Storage {
    data_dir: PathBuf,
    /// Key/value pairs from the active build config.
    pub build_config: HashMap<String, Vec<String>>,
    /// Truncated (9-byte) encoding key -> archive location.
    index: HashMap<[u8; 9], Loc>,
    /// Content key -> first encoding key.
    encoding: HashMap<Key, Key>,
    archives: Vec<OnceLock<Option<Mmap>>>,
}

impl Storage {
    /// `game_dir` is the StarCraft II install root (the folder with `.build.info`).
    pub fn open(game_dir: &Path) -> Result<Self> {
        let build_key = active_build_key(game_dir)?;
        let data_root = game_dir.join("SC2Data");
        let build_config = parse_config(&std::fs::read_to_string(config_path(&data_root, &build_key))
            .with_context(|| format!("build config {build_key}"))?);

        let data_dir = data_root.join("data");
        let (index, max_archive) = load_indices(&data_dir)?;
        let mut st = Self {
            data_dir,
            build_config,
            index,
            encoding: HashMap::new(),
            archives: (0..=max_archive).map(|_| OnceLock::new()).collect(),
        };

        // `encoding = <ckey> <ekey>`: the encoding file is fetched by its ekey.
        let enc_ekey = parse_key(st.config_value("encoding", 1)?)?;
        let enc = st.read_ekey(&enc_ekey).context("encoding file")?;
        st.encoding = parse_encoding(&enc)?;
        Ok(st)
    }

    pub fn config_value(&self, key: &str, i: usize) -> Result<&str> {
        self.build_config
            .get(key)
            .and_then(|v| v.get(i))
            .map(String::as_str)
            .with_context(|| format!("build config has no {key}[{i}]"))
    }

    pub fn index_len(&self) -> usize {
        self.index.len()
    }

    pub fn encoding_len(&self) -> usize {
        self.encoding.len()
    }

    pub fn ekey_for(&self, ckey: &Key) -> Option<Key> {
        self.encoding.get(ckey).copied()
    }

    /// Raw BLTE blob for an encoding key.
    fn blob(&self, ekey: &Key) -> Result<&[u8]> {
        let short: [u8; 9] = ekey[..9].try_into().unwrap();
        let loc = *self.index.get(&short).with_context(|| format!("ekey {} not in local index", key_hex(ekey)))?;
        let map = self.archives[loc.archive as usize]
            .get_or_init(|| {
                let f = File::open(self.data_dir.join(format!("data.{:03}", loc.archive))).ok()?;
                unsafe { Mmap::map(&f) }.ok()
            })
            .as_ref()
            .with_context(|| format!("data.{:03} unreadable", loc.archive))?;
        // 30-byte per-entry header (reversed ekey, size, flags, checksums) precedes the BLTE data.
        let (start, end) = (loc.offset as usize + 30, (loc.offset + loc.size) as usize);
        map.get(start..end).context("index entry past end of archive")
    }

    pub fn read_ekey(&self, ekey: &Key) -> Result<Vec<u8>> {
        blte::decode(self.blob(ekey)?).with_context(|| format!("ekey {}", key_hex(ekey)))
    }

    pub fn read_ckey(&self, ckey: &Key) -> Result<Vec<u8>> {
        let ekey = self.ekey_for(ckey).with_context(|| format!("ckey {} not in encoding", key_hex(ckey)))?;
        self.read_ekey(&ekey)
    }
}

fn active_build_key(game_dir: &Path) -> Result<String> {
    let text = std::fs::read_to_string(game_dir.join(".build.info")).context(".build.info")?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Vec<&str> = lines.next().context("empty .build.info")?.split('|').map(|h| h.split('!').next().unwrap()).collect();
    let col = |name: &str| header.iter().position(|h| *h == name).with_context(|| format!(".build.info: no {name}"));
    let (active, build) = (col("Active")?, col("Build Key")?);
    for line in lines {
        let f: Vec<&str> = line.split('|').collect();
        if f.get(active) == Some(&"1") {
            return Ok(f[build].to_string());
        }
    }
    bail!(".build.info: no active build")
}

fn config_path(data_root: &Path, key: &str) -> PathBuf {
    data_root.join("config").join(&key[0..2]).join(&key[2..4]).join(key)
}

fn parse_config(text: &str) -> HashMap<String, Vec<String>> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(" = "))
        .map(|(k, v)| (k.trim().to_string(), v.split_whitespace().map(String::from).collect()))
        .collect()
}

/// Loads the newest `.idx` of each of the 16 buckets (`BBVVVVVVVV.idx`, bucket + version in hex).
fn load_indices(data_dir: &Path) -> Result<(HashMap<[u8; 9], Loc>, u16)> {
    let mut newest: HashMap<u8, (u32, PathBuf)> = HashMap::new();
    for e in std::fs::read_dir(data_dir).with_context(|| format!("{}", data_dir.display()))? {
        let p = e?.path();
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        if p.extension().is_none_or(|x| x != "idx") || stem.len() != 10 {
            continue;
        }
        let (Ok(bucket), Ok(ver)) = (u8::from_str_radix(&stem[..2], 16), u32::from_str_radix(&stem[2..], 16)) else {
            continue;
        };
        if newest.get(&bucket).is_none_or(|(v, _)| ver > *v) {
            newest.insert(bucket, (ver, p));
        }
    }
    ensure!(!newest.is_empty(), "no .idx files in {}", data_dir.display());

    let mut index = HashMap::new();
    let mut max_archive = 0;
    for (_, path) in newest.values() {
        let d = std::fs::read(path)?;
        // Header (v7): hashSize, hash, version u16, bucket, extra, spanSize, spanOffset, keySize, offsetBits, maxSize u64.
        ensure!(d.len() >= 0x28, "{}: short", path.display());
        let (size_len, off_len, key_len, off_bits) = (d[12] as usize, d[13] as usize, d[14] as usize, d[15] as u32);
        ensure!(key_len == 9 && off_len == 5 && size_len == 4, "{}: unexpected idx layout", path.display());
        let entries_len = u32::from_le_bytes(d[0x20..0x24].try_into().unwrap()) as usize;
        let rec = key_len + off_len + size_len;
        let body = d.get(0x28..0x28 + entries_len).context("idx entries truncated")?;
        for r in body.chunks_exact(rec) {
            let key: [u8; 9] = r[..9].try_into().unwrap();
            let v = r[9..14].iter().fold(0u64, |a, &b| a << 8 | b as u64);
            let loc = Loc {
                archive: (v >> off_bits) as u16,
                offset: (v & ((1 << off_bits) - 1)) as u32,
                size: u32::from_le_bytes(r[14..18].try_into().unwrap()),
            };
            max_archive = max_archive.max(loc.archive);
            index.entry(key).or_insert(loc);
        }
    }
    Ok((index, max_archive))
}

fn parse_encoding(d: &[u8]) -> Result<HashMap<Key, Key>> {
    ensure!(d.len() >= 22 && &d[..2] == b"EN", "bad encoding magic");
    let (ckey_len, ekey_len) = (d[3] as usize, d[4] as usize);
    ensure!(ckey_len == 16 && ekey_len == 16, "unexpected encoding key sizes");
    let page_size = u16::from_be_bytes([d[5], d[6]]) as usize * 1024;
    let page_count = u32::from_be_bytes(d[9..13].try_into().unwrap()) as usize;
    let espec_len = u32::from_be_bytes(d[18..22].try_into().unwrap()) as usize;

    // Header, espec string block, CE page index (first key + md5 per page), then the pages.
    let pages = 22 + espec_len + page_count * 32;
    let mut map = HashMap::with_capacity(page_count * page_size / 40);
    for p in 0..page_count {
        let page = d.get(pages + p * page_size..pages + (p + 1) * page_size).context("encoding page truncated")?;
        let mut i = 0;
        while i + 6 + ckey_len <= page.len() {
            let n = page[i] as usize;
            if n == 0 {
                break;
            }
            let ck: Key = page[i + 6..i + 22].try_into().unwrap();
            let ek: Key = page[i + 22..i + 38].try_into().unwrap();
            map.insert(ck, ek);
            i += 6 + ckey_len + n * ekey_len;
        }
    }
    Ok(map)
}
