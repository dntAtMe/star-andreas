//! SA audio data (audio.md §1): SFX paks (banks of s16 mono PCM sounds) and the encrypted
//! Ogg Vorbis stream paks (cutscene tracks, radio, mission ambience).

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Stream XOR key (CAEStreamTransformer, 0x4F1750): `plain[p] = file[p] ^ KEY[p & 15]`,
/// `p` the absolute offset in the pak file.
pub const STREAM_KEY: [u8; 16] = [0xEA, 0x3A, 0xC4, 0xA1, 0x9A, 0xA8, 0x14, 0xF3, 0x48, 0xB0, 0xD7, 0x23, 0x9D, 0xE8, 0xFF, 0xF1];
/// Bank header: u16 count, u16 pad, 400 × 12-byte sound entries.
pub const BANK_HEADER: u64 = 4804;
/// Stream track header (1000 beat entries, Ogg length, ...).
pub const TRACK_HEADER: u64 = 8068;

#[derive(Debug, Clone, Copy)]
pub struct Lookup {
    pub pak: u8,
    pub offset: u32,
    pub size: u32,
}

/// One SFX sound: mono s16 samples.
#[derive(Debug, Clone)]
pub struct Sound {
    pub samples: Vec<i16>,
    pub rate: u32,
    /// Loop start in samples (-1 = one-shot).
    pub loop_start: i32,
    /// Headroom in dB (subtracted from the volume).
    pub headroom_db: f32,
}

/// The audio CONFIG tables and the pak paths.
#[derive(Debug, Clone)]
pub struct SaAudio {
    root: PathBuf,
    pub sfx_paks: Vec<String>,
    pub banks: Vec<Lookup>,
    pub stream_paks: Vec<String>,
    pub tracks: Vec<Lookup>,
    /// EventVol.dat: dB per audio event id (-128 = unset).
    pub event_vol: Vec<i8>,
}

fn names(d: &[u8], rec: usize) -> Vec<String> {
    d.chunks_exact(rec)
        .map(|c| {
            let n = c.iter().position(|&b| b == 0).unwrap_or(c.len());
            String::from_utf8_lossy(&c[..n]).into_owned()
        })
        .collect()
}

fn lookups(d: &[u8]) -> Vec<Lookup> {
    d.chunks_exact(12)
        .map(|c| Lookup {
            pak: c[0],
            offset: u32::from_le_bytes(c[4..8].try_into().unwrap()),
            size: u32::from_le_bytes(c[8..12].try_into().unwrap()),
        })
        .collect()
}

impl SaAudio {
    /// Read `audio/CONFIG` under the game directory.
    pub fn open(game: &Path) -> std::io::Result<Self> {
        let cfg = game.join("audio").join("CONFIG");
        let rd = |n: &str| std::fs::read(cfg.join(n));
        Ok(Self {
            root: game.join("audio"),
            sfx_paks: names(&rd("PakFiles.dat")?, 52),
            banks: lookups(&rd("BankLkup.dat")?),
            stream_paks: names(&rd("StrmPaks.dat")?, 16),
            tracks: lookups(&rd("TrakLkup.dat")?),
            event_vol: rd("EventVol.dat").map(|v| v.into_iter().map(|b| b as i8).collect()).unwrap_or_default(),
        })
    }

    /// Sound `idx` of SFX bank `bank`.
    pub fn sound(&self, bank: u16, idx: u16) -> Option<Sound> {
        let l = *self.banks.get(bank as usize)?;
        let pak = self.sfx_paks.get(l.pak as usize)?;
        let mut f = File::open(self.root.join("SFX").join(pak)).ok()?;
        f.seek(SeekFrom::Start(l.offset as u64)).ok()?;
        let mut hdr = vec![0u8; BANK_HEADER as usize];
        f.read_exact(&mut hdr).ok()?;
        let n = u16::from_le_bytes([hdr[0], hdr[1]]);
        if idx >= n {
            return None;
        }
        let entry = |k: usize| {
            let e = &hdr[4 + k * 12..16 + k * 12];
            (
                u32::from_le_bytes(e[0..4].try_into().unwrap()),
                i32::from_le_bytes(e[4..8].try_into().unwrap()),
                u16::from_le_bytes(e[8..10].try_into().unwrap()),
                i16::from_le_bytes(e[10..12].try_into().unwrap()),
            )
        };
        let (off, loop_start, rate, headroom) = entry(idx as usize);
        let end = if idx + 1 < n { entry(idx as usize + 1).0 } else { l.size };
        let len = end.saturating_sub(off) as usize;
        f.seek(SeekFrom::Start(l.offset as u64 + BANK_HEADER + off as u64)).ok()?;
        let mut raw = vec![0u8; len];
        f.read_exact(&mut raw).ok()?;
        let samples = raw.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        Some(Sound { samples, rate: rate as u32, loop_start, headroom_db: headroom as f32 * 0.01 })
    }

    /// Stream track `id`: the decrypted Ogg Vorbis bytes (the header's Ogg length, which is
    /// right for every track including 60–65).
    pub fn track_ogg(&self, id: u16) -> Option<Vec<u8>> {
        let l = *self.tracks.get(id as usize)?;
        let pak = self.stream_paks.get(l.pak as usize).filter(|n| !n.is_empty())?;
        let mut f = File::open(self.root.join("streams").join(pak)).ok()?;
        let read = |f: &mut File, at: u64, n: usize| -> Option<Vec<u8>> {
            f.seek(SeekFrom::Start(at)).ok()?;
            let mut b = vec![0u8; n];
            let got = f.read(&mut b).ok()?;
            b.truncate(got);
            for (k, x) in b.iter_mut().enumerate() {
                *x ^= STREAM_KEY[((at + k as u64) & 15) as usize];
            }
            Some(b)
        };
        let len_bytes = read(&mut f, l.offset as u64 + 8000, 4)?;
        let ogg_len = u32::from_le_bytes(len_bytes[..4].try_into().ok()?);
        let len = if ogg_len > 0 && ogg_len <= l.size { ogg_len } else { l.size };
        let ogg = read(&mut f, l.offset as u64 + TRACK_HEADER, len as usize)?;
        ogg.starts_with(b"OggS").then_some(ogg)
    }

    /// Mission audio id → (bank, sound) for speech ids ≥ 2000 (0x4D9CC0): bank
    /// `147 + (id−2000)/200`, sound `(id−2000) % 200`.
    pub fn mission_speech(id: u32) -> Option<(u16, u16)> {
        (id >= 2000).then(|| ((147 + (id - 2000) / 200) as u16, ((id - 2000) % 200) as u16))
    }

    /// EventVol volume of an audio event id in dB (None when unset).
    pub fn event_volume(&self, id: u32) -> Option<f32> {
        self.event_vol.get(id as usize).copied().filter(|&v| v != -128).map(|v| v as f32)
    }
}
