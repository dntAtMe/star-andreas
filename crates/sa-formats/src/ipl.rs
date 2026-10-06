//! IPL item placement. Text files (`inst` section) plus the binary
//! `*_streamN.ipl` files stored inside gta3.img.

use anyhow::{Result, bail};

use crate::{bin::Reader, ide::fields};

#[derive(Debug, Clone)]
pub struct Instance {
    pub id: u32,
    pub model: String,
    pub interior: i32,
    pub pos: [f32; 3],
    /// Quaternion (x, y, z, w) exactly as stored. SA stores the *inverse*
    /// rotation, so consumers must conjugate it.
    pub rot: [f32; 4],
    /// Index of the LOD instance within the owning *text* IPL, or -1.
    pub lod: i32,
}

pub fn parse_text(text: &str) -> Result<Vec<Instance>> {
    let mut out = Vec::new();
    let mut in_inst = false;
    for raw in text.lines() {
        let f = fields(raw);
        if f.is_empty() {
            continue;
        }
        if f.len() == 1 {
            in_inst = f[0].eq_ignore_ascii_case("inst");
            continue;
        }
        if !in_inst || f.len() < 11 {
            continue;
        }
        let num = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        out.push(Instance {
            id: f[0].parse().unwrap_or(0),
            model: f[1].to_ascii_lowercase(),
            interior: f[2].parse().unwrap_or(0),
            pos: [num(3), num(4), num(5)],
            rot: [num(6), num(7), num(8), num(9)],
            lod: f[10].parse().unwrap_or(-1),
        });
    }
    Ok(out)
}

/// Binary IPL ("bnry"). Model names are not stored; resolve via the id.
pub fn parse_binary(data: &[u8]) -> Result<Vec<Instance>> {
    let mut r = Reader::new(data);
    if r.bytes(4)? != b"bnry" {
        bail!("not a binary IPL");
    }
    let count = r.u32()? as usize;
    r.seek(28)?;
    let offset = r.u32()? as usize;
    r.seek(offset)?;
    (0..count)
        .map(|_| {
            let pos = r.vec3()?;
            let rot = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
            let id = r.i32()? as u32;
            let interior = r.i32()?;
            let lod = r.i32()?;
            Ok(Instance { id, model: String::new(), interior, pos, rot, lod })
        })
        .collect()
}

/// One IPL `enex` line (`CFileLoader::LoadEntryExit` 0x5B8030): entrance rect, exit point,
/// area, flags, name, sky colour, ped count (default 2) and time window (default 0..24).
#[derive(Debug, Clone)]
pub struct EntryExit {
    pub pos: [f32; 3],
    /// Stored in radians in the files.
    pub rot: f32,
    pub size: [f32; 2],
    pub exit: [f32; 3],
    /// Degrees.
    pub exit_rot: f32,
    pub area: u8,
    pub flags: u16,
    pub name: String,
    pub sky: u8,
    pub num_peds: u8,
    pub time_on: u8,
    pub time_off: u8,
}

/// The `enex` sections of a text IPL.
pub fn parse_entry_exits(text: &str) -> Vec<EntryExit> {
    let mut out = Vec::new();
    let mut in_enex = false;
    for raw in text.lines() {
        let f = fields(raw);
        if f.is_empty() {
            continue;
        }
        if f.len() == 1 {
            in_enex = f[0].eq_ignore_ascii_case("enex");
            continue;
        }
        if !in_enex || f.len() < 14 {
            continue;
        }
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        let int = |i: usize, d: i32| f.get(i).and_then(|v| v.parse::<i32>().ok()).unwrap_or(d);
        // The token's quotes are cut: `"NAME"` → NAME; no quote → no name.
        let tok = f[13];
        let name = if tok.starts_with('"') { tok.trim_matches('"').to_string() } else { String::new() };
        out.push(EntryExit {
            pos: [num(0), num(1), num(2)],
            rot: num(3),
            size: [num(4), num(5)],
            exit: [num(7), num(8), num(9)],
            exit_rot: num(10),
            area: int(11, 0) as u8,
            flags: int(12, 0) as u16,
            name,
            sky: int(14, 0) as u8,
            num_peds: int(15, 2) as u8,
            time_on: int(16, 0) as u8,
            time_off: int(17, 24) as u8,
        });
    }
    out
}
