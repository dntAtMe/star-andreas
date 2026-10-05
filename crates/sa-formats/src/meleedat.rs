//! `data/melee.dat`: `CTaskSimpleFight::LoadMeleeData` (0x5BEDC0).
//!
//! Rows inside a combo are positional (the keyword is never compared): ANIMGROUP, RANGES,
//! ATTACK1..3, AGROUND, AMOVING, ABLOCK, FLAGS. Times are in frames and stored as seconds
//! (`× 0.033333335`).

/// One `m_aComboData` entry (0x88 bytes in the exe).
#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    /// ANIMGROUP name (resolved to an anim group by the caller).
    pub anim_group: String,
    pub range: f32,
    /// Per move: ATTACK1, ATTACK2, ATTACK3, AGROUND, AMOVING.
    pub hit_time: [f32; 5],
    pub chain_time: [f32; 5],
    pub hit_radius: [f32; 5],
    /// Index into the hit offsets, 7 = none.
    pub hit_level: [u8; 5],
    pub damage: [u8; 5],
    /// Ped audio event ids.
    pub hit_sound: [i32; 5],
    pub alt_hit_sound: [i32; 5],
    pub ground_loop_time: f32,
    pub block_hold_time: f32,
    pub block_alt_hold_time: f32,
    pub flags: u16,
}

impl Default for Combo {
    fn default() -> Self {
        Self {
            anim_group: "melee_1".into(),
            range: 1.5,
            hit_time: [100.0; 5],
            chain_time: [100.0; 5],
            hit_radius: [1.0; 5],
            hit_level: [7; 5],
            damage: [0; 5],
            hit_sound: [0; 5],
            alt_hit_sound: [0; 5],
            ground_loop_time: 0.0,
            block_hold_time: 100.0,
            block_alt_hold_time: 100.0,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MeleeDat {
    /// 13 combos, index = combo type − 4 (UNARMED_1 = 4 … PISTOL_WHIP = 16).
    pub combos: Vec<Combo>,
    /// `m_aHitOffset[7]` (H, L, G, B, HL, LL, GL).
    pub hit_offsets: [[f32; 3]; 7],
}

const FRAME: f32 = 0.033_333_335;

/// 0x5BD360: only the first character is tested, so HL/LL/GL read as H/L/G.
fn hit_level(s: &str) -> u8 {
    match s.as_bytes().first() {
        Some(b'H') => 0,
        Some(b'L') => 1,
        Some(b'G') => 2,
        Some(b'B') => 3,
        _ => 7,
    }
}

/// 0x5BD3B0: melee.dat sound numbers to ped audio event ids.
fn hit_sound(n: i32) -> i32 {
    match n {
        1 => 0x3D,
        3 => 0x3F,
        4 => 0x40,
        5 => 0x41,
        6 => 0x42,
        7 => 0x43,
        8 => 0x44,
        _ => 0x3E,
    }
}

pub fn parse(text: &str) -> MeleeDat {
    let mut out = MeleeDat { combos: vec![Combo::default(); 13], hit_offsets: [[0.0, 0.75, 0.0]; 7] };
    let (mut in_combo, mut in_levels) = (false, false);
    let (mut line, mut combo) = (0usize, 0usize);
    for raw in text.lines() {
        let s = raw.trim_start();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if s.starts_with("END_MELEE_DATA") {
            break;
        }
        let f: Vec<&str> = s.split_whitespace().collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok());
        if in_combo || in_levels {
            if s.starts_with("END_COMBO") {
                if in_combo {
                    combo += 1;
                }
                in_combo = false;
                in_levels = false;
                line = 0;
                continue;
            }
            if in_levels {
                if line < 7 {
                    if let (Some(x), Some(y), Some(z)) = (num(1), num(2), num(3)) {
                        out.hit_offsets[line] = [x, y, z];
                    }
                }
                line += 1;
                continue;
            }
            let Some(c) = out.combos.get_mut(combo) else {
                line += 1;
                continue;
            };
            match line {
                0 => {
                    if let Some(g) = f.get(1) {
                        c.anim_group = g.to_ascii_lowercase();
                    }
                }
                1 => {
                    if let Some(r) = num(1) {
                        c.range = r;
                    }
                }
                2..=6 => {
                    let m = line - 2;
                    // sscanf stops at the first field that fails; earlier ones are kept.
                    if let Some(v) = num(1) {
                        c.hit_time[m] = v * FRAME;
                    }
                    if let Some(v) = num(2) {
                        c.chain_time[m] = v * FRAME;
                    }
                    if let Some(v) = num(3) {
                        c.hit_radius[m] = v;
                    }
                    if let Some(l) = f.get(4) {
                        c.hit_level[m] = hit_level(l);
                    }
                    if let Some(d) = f.get(5).and_then(|v| v.parse::<i32>().ok()) {
                        c.damage[m] = d as u8;
                    }
                    if let Some(v) = f.get(6).and_then(|v| v.parse::<i32>().ok()) {
                        c.hit_sound[m] = hit_sound(v);
                    }
                    if let Some(v) = f.get(7).and_then(|v| v.parse::<i32>().ok()) {
                        c.alt_hit_sound[m] = hit_sound(v);
                    }
                    if let Some(gl) = num(8).filter(|g| *g > 0.0) {
                        c.ground_loop_time = gl * FRAME;
                    }
                }
                7 => {
                    if let Some(v) = num(1) {
                        c.block_hold_time = v * FRAME;
                    }
                    if let Some(v) = num(2) {
                        c.block_alt_hold_time = v * FRAME;
                    }
                }
                8 => {
                    if let Some(v) = f.get(1) {
                        let h = v.trim_start_matches("0x").trim_start_matches("0X");
                        c.flags = u16::from_str_radix(h, 16).unwrap_or(0);
                    }
                }
                _ => {}
            }
            line += 1;
        } else if s.starts_with("START_COMBO") {
            in_combo = true;
        } else if s.starts_with("START_LEVELS") {
            in_levels = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_combo() {
        let d = super::parse(
            "START_LEVELS\n\tLEVEL_H 0.0 0.7 0.5\nEND_COMBO\nSTART_COMBO\tUNARMED_1\n\tANIMGROUP\tmelee_1\n\tRANGES\t1.6\n\t#c\n\
             \tATTACK1\t5.0\t10.0\t0.4\tH\t6\t5\t6\n\tATTACK2 8 17 0.4 H 9 5 6\n\tATTACK3 10 20 0.4 H 15 1 2\n\
             \tAGROUND 10.5 20 0.4 G 25 4 4 8.5\n\tAMOVING 7 10 0.4 HL 5 5 6\n\tABLOCK 6.0 11.0\n\tFLAGS 0x60f\nEND_COMBO\n",
        );
        assert_eq!(d.hit_offsets[0], [0.0, 0.7, 0.5]);
        let c = &d.combos[0];
        assert_eq!(c.anim_group, "melee_1");
        assert!((c.hit_time[0] - 0.166_666_67).abs() < 1e-6);
        assert_eq!(c.hit_level, [0, 0, 0, 2, 0]);
        assert_eq!(c.damage[3], 25);
        assert_eq!(c.hit_sound[2], 0x3D);
        assert!((c.ground_loop_time - 8.5 * 0.033_333_335).abs() < 1e-6);
        assert_eq!(c.flags, 0x60F);
    }
}
