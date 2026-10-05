//! Population data (population.md §1): peds.ide `peds` section, pedstats.dat, popcycle.dat,
//! pedgrp.dat, info.zon navigation zones, the zone settings main.scm applies with opcodes
//! 0x0767 / 0x0874 / 0x076C, and the path files nodes0..63.dat.

use std::collections::HashMap;

use crate::ide::fields;

/// One peds.ide line.
#[derive(Debug, Clone)]
pub struct PedDef {
    pub id: u32,
    pub model: String,
    pub txd: String,
    pub ped_type: String,
    pub stat_type: String,
    pub anim_group: String,
    /// carsCanDriveMask (0x1000 = on foot).
    pub cars_mask: u32,
    pub flags: u32,
    pub anim_file: String,
}

impl PedDef {
    /// `FindPedRaceFromName` (0x5B6D40): the first of the first two characters that is
    /// B (1), W (2), I / O (3) or H (4); 0 = any.
    pub fn race(&self) -> u8 {
        for c in self.model.bytes().take(2).map(|c| c.to_ascii_uppercase()) {
            match c {
                b'B' => return 1,
                b'W' => return 2,
                b'I' | b'O' => return 3,
                b'H' => return 4,
                _ => {}
            }
        }
        0
    }
}

pub fn parse_peds_ide(text: &str) -> Vec<PedDef> {
    let mut out = Vec::new();
    let mut in_peds = false;
    for raw in text.lines() {
        let f = fields(raw);
        if f.len() == 1 {
            in_peds = f[0].eq_ignore_ascii_case("peds");
            if f[0].eq_ignore_ascii_case("end") {
                in_peds = false;
            }
            continue;
        }
        if !in_peds || f.len() < 9 {
            continue;
        }
        let Ok(id) = f[0].parse() else { continue };
        let hex = |s: &str| u32::from_str_radix(s.trim(), 16).unwrap_or(0);
        out.push(PedDef {
            id,
            model: f[1].to_ascii_lowercase(),
            txd: f[2].to_ascii_lowercase(),
            ped_type: f[3].to_ascii_uppercase(),
            stat_type: f[4].to_ascii_uppercase(),
            anim_group: f[5].to_ascii_lowercase(),
            cars_mask: hex(f[6]),
            flags: hex(f[7]),
            anim_file: f[8].to_ascii_lowercase(),
        });
    }
    out
}

/// `CPedType::FindPedType` (0x608790): table order.
pub const PED_TYPES: [&str; 32] = [
    "PLAYER1", "PLAYER2", "PLAYER_NETWORK", "PLAYER_UNUSED", "CIVMALE", "CIVFEMALE", "COP", "GANG1", "GANG2", "GANG3",
    "GANG4", "GANG5", "GANG6", "GANG7", "GANG8", "GANG9", "GANG10", "DEALER", "MEDIC", "FIREMAN", "CRIMINAL", "BUM",
    "PROSTITUTE", "SPECIAL", "MISSION1", "MISSION2", "MISSION3", "MISSION4", "MISSION5", "MISSION6", "MISSION7",
    "MISSION8",
];

pub fn ped_type_index(name: &str) -> u8 {
    PED_TYPES.iter().position(|t| t.eq_ignore_ascii_case(name)).unwrap_or(32) as u8
}

/// One pedstats.dat record (0x34 bytes), in file order.
#[derive(Debug, Clone)]
pub struct PedStat {
    pub name: String,
    pub flee_distance: f32,
    /// Degrees per frame (`ped+0x560`).
    pub heading_change_rate: f32,
    pub fear: u8,
    pub temper: u8,
    pub lawfulness: u8,
    pub sexiness: u8,
    pub attack_strength: f32,
    pub defend_weakness: f32,
    pub shooting_rate: u16,
    pub decision_maker: u8,
}

pub fn parse_pedstats(text: &str) -> Vec<PedStat> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 11 {
            continue;
        }
        let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        out.push(PedStat {
            name: f[0].to_ascii_uppercase(),
            flee_distance: n(1),
            heading_change_rate: n(2),
            fear: n(3) as u8,
            temper: n(4) as u8,
            lawfulness: n(5) as u8,
            sexiness: n(6) as u8,
            attack_strength: n(7),
            defend_weakness: n(8),
            shooting_rate: n(9) as u16,
            decision_maker: n(10) as u8,
        });
    }
    out
}

/// popcycle.dat: per (zone type 20, day type 2, 2-hour slot 12) the ped / car limits, the
/// dealer / gang / cop / other percentages and the 18 group percentages (normalised to 100).
#[derive(Debug, Clone)]
pub struct PopCycle {
    /// Index `slot*40 + day*20 + zone`.
    pub max_peds: Vec<u8>,
    pub max_cars: Vec<u8>,
    pub perc_dealers: Vec<u8>,
    pub perc_gang: Vec<u8>,
    pub perc_cops: Vec<u8>,
    pub perc_other: Vec<u8>,
    pub perc_group: Vec<[u8; 18]>,
}

impl PopCycle {
    pub fn index(slot: usize, day: usize, zone: usize) -> usize {
        slot * 40 + day * 20 + zone
    }
}

/// `CPopCycle::Initialise` (0x5BC090): file order is zone × day × slot.
pub fn parse_popcycle(text: &str) -> PopCycle {
    let n = 12 * 2 * 20;
    let mut p = PopCycle {
        max_peds: vec![0; n],
        max_cars: vec![0; n],
        perc_dealers: vec![0; n],
        perc_gang: vec![0; n],
        perc_cops: vec![0; n],
        perc_other: vec![0; n],
        perc_group: vec![[0; 18]; n],
    };
    let mut k = 0usize;
    for raw in text.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('/') {
            continue;
        }
        let v: Vec<i32> = l.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        if v.len() < 24 || k >= n {
            continue;
        }
        let (zone, day, slot) = (k / 24, (k / 12) % 2, k % 12);
        k += 1;
        let i = PopCycle::index(slot, day, zone);
        p.max_peds[i] = v[0] as u8;
        p.max_cars[i] = v[1] as u8;
        p.perc_dealers[i] = v[2] as u8;
        p.perc_gang[i] = v[3] as u8;
        p.perc_cops[i] = v[4] as u8;
        p.perc_other[i] = v[5] as u8;
        // Normalise the groups to 100 %, the remainder to the largest (last on ties).
        let sum: i32 = v[6..24].iter().sum();
        let mut g = [0u8; 18];
        if sum > 0 {
            let k = 100.0f32 / sum as f32;
            for j in 0..18 {
                g[j] = (v[6 + j] as f32 * k) as i32 as u8;
            }
        }
        let mut largest = 0;
        for j in 0..18 {
            if g[largest] <= g[j] {
                largest = j;
            }
        }
        let total: u32 = g.iter().map(|&x| x as u32).sum();
        g[largest] = g[largest].wrapping_add((100u32.wrapping_sub(total)) as u8);
        p.perc_group[i] = g;
    }
    p
}

/// `CPopulation::LoadPedGroups` (0x5BCFE0): model names per group (≤ 21), unknown models
/// skipped by the caller.
pub fn parse_pedgrp(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let l = raw.split('#').next().unwrap_or("").replace([',', '\r'], " ");
        let names: Vec<String> = l.split_whitespace().take(21).map(|s| s.to_ascii_lowercase()).collect();
        if !names.is_empty() {
            out.push(names);
        }
    }
    out
}

/// `m_TranslationArray[33][3]` (0x8D2540): pop group × island → ped group.
pub fn ped_group_of(pop_group: usize, island: usize) -> usize {
    match pop_group {
        0..=2 => pop_group * 3 + island,
        3 => 9,
        4 => 10,
        5 => 11 + island,
        6 => 14 + island,
        7 => 17 + island,
        8 => 20 + island,
        9 => 23 + island,
        10 => 26 + island,
        11 => 29,
        12 => 30 + island,
        13 => 33 + island,
        14 => 36 + island,
        15 => 39,
        16 => 40,
        17 => 41,
        18..=27 => 42 + pop_group - 18,
        28 => 52,
        29 => 53,
        30 => 54,
        31 => 55,
        _ => 56,
    }
}

/// An info.zon navigation zone (`CZone`).
#[derive(Debug, Clone)]
pub struct Zone {
    pub label: String,
    pub ty: u8,
    pub min: [i16; 3],
    pub max: [i16; 3],
    pub level: u8,
    pub text: String,
}

pub fn parse_zones(text: &str) -> Vec<Zone> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in text.lines() {
        let f = fields(raw);
        if f.len() == 1 {
            inside = f[0].eq_ignore_ascii_case("zone");
            continue;
        }
        if !inside || f.len() < 10 {
            continue;
        }
        let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0) as i32 as i16;
        out.push(Zone {
            label: f[0].to_ascii_uppercase(),
            ty: f[1].parse().unwrap_or(0),
            min: [n(2), n(3), n(4)],
            max: [n(5), n(6), n(7)],
            level: f[8].parse().unwrap_or(0),
            text: f[9].to_ascii_uppercase(),
        });
    }
    out
}

/// The zone settings main.scm makes at start: popType (0x0767), race mask (0x0874) and
/// gang strengths (0x076C), keyed by zone label.
#[derive(Debug, Clone, Default)]
pub struct ScmZoneSettings {
    pub pop_type: HashMap<String, u8>,
    pub race: HashMap<String, u8>,
    pub gang: HashMap<String, [u8; 10]>,
}

/// Byte search of main.scm for `u16 opcode, 0x09 + 8-byte name, typed ints`.
pub fn scan_scm_zone_settings(scm: &[u8]) -> ScmZoneSettings {
    let mut s = ScmZoneSettings::default();
    // Typed int: 1 i32, 4 i8, 5 i16.
    let int_at = |i: usize| -> Option<(i32, usize)> {
        match *scm.get(i)? {
            1 => Some((i32::from_le_bytes(scm.get(i + 1..i + 5)?.try_into().ok()?), 5)),
            4 => Some((*scm.get(i + 1)? as i8 as i32, 2)),
            5 => Some((i16::from_le_bytes(scm.get(i + 1..i + 3)?.try_into().ok()?) as i32, 3)),
            _ => None,
        }
    };
    let mut i = 0;
    while i + 12 < scm.len() {
        let op = u16::from_le_bytes([scm[i], scm[i + 1]]);
        if matches!(op, 0x0767 | 0x0874 | 0x076C) && scm[i + 2] == 0x09 {
            let raw = &scm[i + 3..i + 11];
            let end = raw.iter().position(|&b| b == 0).unwrap_or(8);
            let name = String::from_utf8_lossy(&raw[..end]).to_ascii_uppercase();
            let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if valid {
                if let Some((a, la)) = int_at(i + 11) {
                    match op {
                        0x0767 => {
                            s.pop_type.insert(name, (a & 0x1F) as u8);
                        }
                        0x0874 => {
                            s.race.insert(name, (a & 0xF) as u8);
                        }
                        _ => {
                            if let Some((b, _)) = int_at(i + 11 + la) {
                                if (0..10).contains(&a) {
                                    s.gang.entry(name).or_insert([0; 10])[a as usize] = b as u8;
                                }
                            }
                        }
                    }
                    i += 11;
                    continue;
                }
            }
        }
        i += 1;
    }
    s
}

/// `CPathNode` (0x1C bytes).
#[derive(Debug, Clone, Copy)]
pub struct PathNode {
    /// x, y, z × 8.
    pub pos: [i16; 3],
    pub base_link: i16,
    pub area: u16,
    pub node: u16,
    /// Path width × 8.
    pub width: u8,
    pub flood: u8,
    pub flags: u32,
}

impl PathNode {
    pub fn num_links(&self) -> usize {
        (self.flags & 0xF) as usize
    }
    /// Spawn probability 0..15 (bits 16..19).
    pub fn spawn_prob(&self) -> u8 {
        ((self.flags >> 16) & 0xF) as u8
    }
}

/// One nodesN.dat.
#[derive(Debug, Clone, Default)]
pub struct PathArea {
    pub num_veh_nodes: usize,
    pub nodes: Vec<PathNode>,
    /// (area, node).
    pub links: Vec<(u16, u16)>,
    pub link_lengths: Vec<u8>,
    /// bit0 road crossing, bit1 ped traffic light.
    pub intersections: Vec<u8>,
}

pub fn parse_nodes(d: &[u8]) -> Option<PathArea> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let (n, nv, _np, nn, nl) = (u32_at(0)? as usize, u32_at(4)? as usize, u32_at(8)?, u32_at(12)? as usize, u32_at(16)? as usize);
    let mut o = 20;
    let i16_at = |o: usize| i16::from_le_bytes([d[o], d[o + 1]]);
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
    if d.len() < 20 + 28 * n + 14 * nn + 8 * nl + 1152 {
        return None;
    }
    let mut a = PathArea { num_veh_nodes: nv, ..Default::default() };
    for _ in 0..n {
        a.nodes.push(PathNode {
            pos: [i16_at(o + 8), i16_at(o + 10), i16_at(o + 12)],
            base_link: i16_at(o + 16),
            area: u16_at(o + 18),
            node: u16_at(o + 20),
            width: d[o + 22],
            flood: d[o + 23],
            flags: u32::from_le_bytes(d[o + 24..o + 28].try_into().unwrap()),
        });
        o += 28;
    }
    o += 14 * nn;
    for i in 0..nl {
        a.links.push((u16_at(o + 4 * i), u16_at(o + 4 * i + 2)));
    }
    o += 4 * nl + 768; // links + the 768-byte filler
    o += 2 * nl; // navi links
    a.link_lengths = d[o..o + nl].to_vec();
    o += nl + 192;
    a.intersections = d[o..o + nl].to_vec();
    Some(a)
}

#[cfg(test)]
mod tests {
    #[test]
    fn race_from_name() {
        let p = |m: &str| super::PedDef {
            id: 0,
            model: m.into(),
            txd: String::new(),
            ped_type: String::new(),
            stat_type: String::new(),
            anim_group: String::new(),
            cars_mask: 0,
            flags: 0,
            anim_file: String::new(),
        };
        assert_eq!(p("sbfyri").race(), 1);
        assert_eq!(p("vwfypro").race(), 2);
        assert_eq!(p("dnb1").race(), 0);
    }

    #[test]
    fn popcycle_normalised() {
        let mut t = String::new();
        for _ in 0..480 {
            t += "3 9 100 100 100 100 0 0 15 0 0 0 0 0 35 35 15 0 0 0 0 0 0 0\n";
        }
        let p = super::parse_popcycle(&t);
        assert_eq!(p.perc_group[0].iter().map(|&x| x as u32).sum::<u32>(), 100);
        assert_eq!(p.max_peds[0], 3);
    }
}
