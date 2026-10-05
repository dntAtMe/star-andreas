//! Car path recordings (`data/Paths/carrec.img`, carrec.md §1-2): `CVehicleStateEachFrame`
//! records of 32 bytes.

/// One recorded frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Record {
    /// ms since the start (smoothed at load).
    pub time: u32,
    pub vel: [i16; 3],
    pub right: [i8; 3],
    pub fwd: [i8; 3],
    pub steer: i8,
    pub gas: i8,
    pub brake: i8,
    pub handbrake: bool,
    pub pos: [f32; 3],
}

/// The recording number of an entry name (`carrec%d`, case-insensitive; 850 when it does not parse).
pub fn number(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    n.strip_prefix("carrec").and_then(|r| r.split('.').next()).and_then(|d| d.parse().ok()).unwrap_or(850)
}

/// Load (0x156F510): the records up to the first one (after record 0) with time 0, then
/// `SmoothRecording` (0x45A0F0): records 1..n-2 get the average time of their neighbours, left to
/// right, in place (using the already smoothed previous record), truncated.
pub fn parse(data: &[u8]) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    for (k, c) in data.chunks_exact(32).enumerate() {
        let time = u32::from_le_bytes(c[0..4].try_into().unwrap());
        if time == 0 && k != 0 {
            break;
        }
        let i16_at = |o: usize| i16::from_le_bytes([c[o], c[o + 1]]);
        let f32_at = |o: usize| f32::from_le_bytes(c[o..o + 4].try_into().unwrap());
        out.push(Record {
            time,
            vel: [i16_at(4), i16_at(6), i16_at(8)],
            right: [c[0xA] as i8, c[0xB] as i8, c[0xC] as i8],
            fwd: [c[0xD] as i8, c[0xE] as i8, c[0xF] as i8],
            steer: c[0x10] as i8,
            gas: c[0x11] as i8,
            brake: c[0x12] as i8,
            handbrake: c[0x13] != 0,
            pos: [f32_at(0x14), f32_at(0x18), f32_at(0x1C)],
        });
    }
    if out.len() >= 3 {
        for i in 1..out.len() - 1 {
            out[i].time = ((out[i - 1].time.wrapping_add(out[i + 1].time)) as f32 * 0.5) as u32;
        }
    }
    out
}
