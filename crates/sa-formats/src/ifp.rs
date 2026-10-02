//! IFP animation packages, San Andreas `ANP3` format.
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
    if magic != b"ANP3" {
        bail!("unsupported IFP {:?} (only ANP3)", String::from_utf8_lossy(magic));
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
