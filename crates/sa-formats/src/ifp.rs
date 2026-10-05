//! IFP animation packages: San Andreas `ANP3` and the older chunked `ANPK` (cutscene
//! animations in cuts.img, cutscene.md §3.3; their quaternions are stored conjugated).
//!
//! Rotations are ordinary local bone rotations (unlike GTA III's ANPK, where
//! they are stored conjugated); verified visually against ped.ifp.

use anyhow::{Result, bail};

use crate::bin::Reader;

#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub time: f32,
    /// Quaternion (x, y, z, w), local to the parent bone, GTA space.
    pub rot: [f32; 4],
    pub pos: Option<[f32; 3]>,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub bone_name: String,
    pub bone_id: i32,
    pub keys: Vec<Key>,
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    pub tracks: Vec<Track>,
    pub duration: f32,
}

pub fn parse(data: &[u8]) -> Result<Vec<Animation>> {
    let mut r = Reader::new(data);
    let magic = r.bytes(4)?;
    if magic == b"ANPK" {
        return parse_anpk(data);
    }
    if magic != b"ANP3" {
        bail!("unsupported IFP {:?}", String::from_utf8_lossy(magic));
    }
    r.u32()?; // size
    r.fixed_str(24)?; // package name
    let num_anims = r.u32()? as usize;
    let mut anims = Vec::with_capacity(num_anims);
    for _ in 0..num_anims {
        let name = r.fixed_str(24)?;
        let num_tracks = r.u32()? as usize;
        r.u32()?; // frame data size
        let compressed = r.u32()? != 0;
        let mut tracks = Vec::with_capacity(num_tracks);
        let mut duration = 0f32;
        for _ in 0..num_tracks {
            let bone_name = r.fixed_str(24)?;
            let frame_type = r.u32()?;
            let num_keys = r.u32()? as usize;
            let bone_id = r.i32()?;
            let has_pos = frame_type == 2 || frame_type == 4;
            let mut keys = Vec::with_capacity(num_keys);
            for _ in 0..num_keys {
                let (rot, time, pos);
                if compressed {
                    let q = |r: &mut Reader| -> Result<f32> { Ok(r.u16()? as i16 as f32 / 4096.0) };
                    rot = [q(&mut r)?, q(&mut r)?, q(&mut r)?, q(&mut r)?];
                    time = r.u16()? as i16 as f32 / 60.0;
                    pos = if has_pos {
                        let t = |r: &mut Reader| -> Result<f32> { Ok(r.u16()? as i16 as f32 / 1024.0) };
                        Some([t(&mut r)?, t(&mut r)?, t(&mut r)?])
                    } else {
                        None
                    };
                } else {
                    rot = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                    time = r.f32()?;
                    pos = if has_pos { Some(r.vec3()?) } else { None };
                }
                keys.push(Key { time, rot, pos });
                duration = duration.max(time);
            }
            tracks.push(Track { bone_name, bone_id, keys });
        }
        anims.push(Animation { name, tracks, duration });
    }
    Ok(anims)
}

/// The chunked GTA III / VC format (`ANPK` → `INFO` → per anim `NAME`, `DGAN` → `INFO`, per
/// sequence `CPAN` → `ANIM` + `KR00` / `KRT0` / `KRTS`). Chunks are 4-byte aligned; key times
/// are absolute seconds; quaternions are conjugated (the loader negates x, y, z).
pub fn parse_anpk(data: &[u8]) -> Result<Vec<Animation>> {
    let rd_u32 = |at: usize| -> Result<u32> {
        data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| anyhow::anyhow!("ANPK: truncated"))
    };
    let rd_f32 = |at: usize| -> Result<f32> { Ok(f32::from_bits(rd_u32(at)?)) };
    let align = |n: usize| (n + 3) & !3;
    let cstr = |b: &[u8]| {
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..n]).into_owned()
    };
    // ANPK header, INFO {numAnims, name}.
    let mut p = 8;
    if &data[p..p + 4] != b"INFO" {
        bail!("ANPK: no INFO");
    }
    let info_size = rd_u32(p + 4)? as usize;
    let num_anims = rd_u32(p + 8)? as usize;
    p += 8 + align(info_size);
    let mut anims = Vec::with_capacity(num_anims);
    for _ in 0..num_anims {
        if data.get(p..p + 4) != Some(b"NAME") {
            bail!("ANPK: expected NAME at {p}");
        }
        let n = rd_u32(p + 4)? as usize;
        let name = cstr(&data[p + 8..p + 8 + n]);
        p += 8 + align(n);
        if data.get(p..p + 4) != Some(b"DGAN") {
            bail!("ANPK: expected DGAN at {p}");
        }
        let dgan_end = p + 8 + align(rd_u32(p + 4)? as usize);
        p += 8;
        // INFO {numSeqs, unknown}
        let isz = rd_u32(p + 4)? as usize;
        let num_seqs = rd_u32(p + 8)? as usize;
        p += 8 + align(isz);
        let mut tracks = Vec::with_capacity(num_seqs);
        let mut duration = 0f32;
        for _ in 0..num_seqs {
            if data.get(p..p + 4) != Some(b"CPAN") {
                bail!("ANPK: expected CPAN at {p}");
            }
            let cpan_end = p + 8 + align(rd_u32(p + 4)? as usize);
            p += 8;
            // ANIM {name[28], numFrames, unk, lastFrame, next, prev, [boneId]}
            let asz = rd_u32(p + 4)? as usize;
            let bone_name = cstr(&data[p + 8..p + 8 + 28]);
            let num_frames = rd_u32(p + 8 + 28)? as usize;
            let bone_id = if asz >= 48 { rd_u32(p + 8 + 44)? as i32 } else { -1 };
            p += 8 + align(asz);
            let mut keys = Vec::with_capacity(num_frames);
            if num_frames > 0 && p + 8 <= cpan_end {
                let kind = &data[p..p + 4];
                let (has_pos, has_scale) = match kind {
                    b"KR00" => (false, false),
                    b"KRT0" => (true, false),
                    b"KRTS" => (true, true),
                    _ => bail!("ANPK: unknown key chunk {:?}", String::from_utf8_lossy(kind)),
                };
                let mut q = p + 8;
                for _ in 0..num_frames {
                    let rot = [-rd_f32(q)?, -rd_f32(q + 4)?, -rd_f32(q + 8)?, rd_f32(q + 12)?];
                    q += 16;
                    let pos = if has_pos {
                        let v = [rd_f32(q)?, rd_f32(q + 4)?, rd_f32(q + 8)?];
                        q += 12;
                        Some(v)
                    } else {
                        None
                    };
                    if has_scale {
                        q += 12;
                    }
                    let time = rd_f32(q)?;
                    q += 4;
                    duration = duration.max(time);
                    keys.push(Key { time, rot, pos });
                }
            }
            tracks.push(Track { bone_name, bone_id, keys });
            p = cpan_end;
        }
        anims.push(Animation { name, tracks, duration });
        p = dgan_end;
    }
    Ok(anims)
}
