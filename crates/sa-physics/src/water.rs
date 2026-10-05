//! `CWaterLevel` (water.dat, the 12×12 grid, GetWaterLevel with SA's wave function) and the
//! buoyancy of `cBuoyancy` (0xC1C890: ProcessBuoyancy, the 3×3 volume sampling, the
//! running-average centre of buoyancy, CalcBuoyancyForce).
//!
//! Not ported: boats (ProcessBuoyancyBoat), the boat-ground and swim-task branches of the
//! ped buoyancy, splash particles, water1.dat.

use glam::Vec3;

use crate::physical::Matrix;

/// `CWaterVertex` (`CRenPar` + integer position).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterVertex {
    pub x: i16,
    pub y: i16,
    pub z: f32,
    pub big_waves: f32,
    pub small_waves: f32,
    pub flow: [i8; 2],
}

/// Quad / triangle flags (u16 at quad+8 / triangle+6).
pub mod wflag {
    pub const INVISIBLE: u16 = 0x2;
    pub const LIMITED_DEPTH: u16 = 0x4;
}

#[derive(Debug, Clone, Copy)]
pub struct WaterQuad {
    /// (minX,minY), (maxX,minY), (minX,maxY), (maxX,maxY).
    pub v: [u16; 4],
    pub flags: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct WaterTri {
    /// v0, v1 share y (v0.x < v1.x); v2 is the third.
    pub v: [u16; 3],
    pub flags: u16,
}

/// A grid cell entry: 0 empty, quad, triangle, or a list.
#[derive(Debug, Clone, Default)]
pub enum Cell {
    #[default]
    Empty,
    Quad(u16),
    Tri(u16),
    List(Vec<(bool, u16)>),
}

#[derive(Debug, Clone, Default)]
pub struct WaterLevel {
    pub vertices: Vec<WaterVertex>,
    pub quads: Vec<WaterQuad>,
    pub tris: Vec<WaterTri>,
    /// `grid[gy + 12·gx]`.
    pub grid: Vec<Cell>,
}

/// One interpolated `CRenPar`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenPar {
    pub z: f32,
    pub big: f32,
    pub small: f32,
}

impl RenPar {
    fn of(v: &WaterVertex) -> Self {
        Self { z: v.z, big: v.big_waves, small: v.small_waves }
    }
    fn lerp3(a: Self, b: Self, c: Self, u: f32, v: f32) -> Self {
        // a + (b - a)·u + (c - a)·v
        Self {
            z: a.z + (b.z - a.z) * u + (c.z - a.z) * v,
            big: a.big + (b.big - a.big) * u + (c.big - a.big) * v,
            small: a.small + (b.small - a.small) * u + (c.small - a.small) * v,
        }
    }
}

/// `CMaths` sine table (256 entries).
fn sin_tab(phase: f32) -> f32 {
    let i = ((phase * 40.743_664) as i32) & 0xFF;
    (i as f32 * std::f32::consts::TAU / 256.0).sin()
}

const T1: [f32; 8] = [1.0, 0.85, 0.73, 0.77, 0.75, 0.80, 0.73, 0.80];
const T2: [f32; 8] = [0.75, 0.90, 0.95, 0.82, 0.70, 0.75, 0.90, 1.0];

/// `CalculateWavesOnlyForCoordinate` (0x6E7210): the wave height at an integer lattice point.
pub fn wave_height(x: i32, y: i32, big: f32, small: f32, wavyness: f32, t_ms: u32) -> f32 {
    let (x, y) = (x.abs(), y.abs());
    let a = T1[((x / 2) & 7) as usize] * T2[((y / 2) & 7) as usize] * wavyness;
    let (xf, yf) = (x as f32, y as f32);
    let mut h = 2.0 * a * big * sin_tab((t_ms % 5000) as f32 * 0.001_256_637 + (xf + yf) * 0.098_174_773);
    h += a * small * sin_tab((t_ms % 3500) as f32 * 0.001_795_195_9 + yf * 0.120_830_49 + xf * 0.241_660_98);
    h += 0.5 * a * small * sin_tab((t_ms % 3000) as f32 * 0.002_094_395_2 + yf * 0.314_159_27);
    h
}

/// `CalculateWavesForCoordinate` (0x6E6EF0) render version: height and the colour multiplier.
pub fn wave_render(x: i32, y: i32, big: f32, small: f32, wavyness: f32, t_ms: u32) -> (f32, f32) {
    let (xa, ya) = (x.abs(), y.abs());
    let a = T1[((xa / 2) & 7) as usize] * T2[((ya / 2) & 7) as usize] * wavyness;
    let (xf, yf) = (xa as f32, ya as f32);
    let p1 = (t_ms % 5000) as f32 * 0.001_256_637 + (xf + yf) * 0.098_174_773;
    let p2 = (t_ms % 3500) as f32 * 0.001_795_195_9 + yf * 0.120_830_49 + xf * 0.241_660_98;
    let p3 = (t_ms % 3000) as f32 * 0.002_094_395_2 + yf * 0.314_159_27;
    let h = 2.0 * a * big * sin_tab(p1) + a * small * sin_tab(p2) + 0.5 * a * small * sin_tab(p3);
    let cos_tab = |p: f32| {
        let i = ((p * 40.743_664 + 64.0) as i32) & 0xFF;
        (i as f32 * std::f32::consts::TAU / 256.0).sin()
    };
    use std::f32::consts::PI;
    let mut n = Vec3::new(0.0, 0.0, 1.0);
    let k1 = -(2.0 * a * big * cos_tab(p1) * PI / 32.0);
    n.x = k1;
    n.y = k1;
    let k2 = a * small * cos_tab(p2) * PI / 13.0;
    n.x += k2;
    n.y += k2;
    n.x += 0.5 * a * small * cos_tab(p3) * PI / 10.0;
    let n = n.normalize();
    let s = ((n.z + n.y + n.x) * 0.577).max(0.0);
    (h, s * 0.65 + 0.27)
}

impl WaterLevel {
    /// `ReadWaterLevel` (0x6EAE80) on the text of data/water.dat.
    pub fn parse(text: &str) -> Self {
        let mut w = WaterLevel::default();
        for line in text.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with([';', '*', 'p']) {
                continue;
            }
            let nums: Vec<f32> = line.split_whitespace().map_while(|s| s.parse::<f32>().ok()).collect();
            if nums.len() >= 28 {
                let flag = if nums.len() >= 29 { nums[28] as i32 } else { 1 };
                let v: Vec<u16> = (0..4).map(|k| w.add_vertex(&nums[k * 7..k * 7 + 7])).collect();
                w.add_quad([v[0], v[1], v[2], v[3]], flag);
            } else if nums.len() >= 21 {
                let flag = if nums.len() >= 22 { nums[21] as i32 } else { 1 };
                let v: Vec<u16> = (0..3).map(|k| w.add_vertex(&nums[k * 7..k * 7 + 7])).collect();
                w.add_tri([v[0], v[1], v[2]], flag);
            }
        }
        w.fill_grid();
        w
    }

    /// `AddWaterLevelVertex` (0x6E5A40).
    fn add_vertex(&mut self, f: &[f32]) -> u16 {
        let mut x = f[0] as i32;
        let mut y = f[1] as i32;
        let mut rp = (f[2], f[5], f[6], [(f[3] * 64.0) as i8, (f[4] * 64.0) as i8]);
        for c in [&mut x, &mut y] {
            if *c <= -3000 {
                *c = -3000;
                rp = (0.0, 1.0, 0.0, [0, 0]);
            }
            if *c >= 3000 {
                *c = 3000;
                rp = (0.0, 1.0, 0.0, [0, 0]);
            }
        }
        if let Some(i) = self.vertices.iter().position(|v| v.x as i32 == x && v.y as i32 == y && v.z == rp.0) {
            return i as u16;
        }
        self.vertices.push(WaterVertex { x: x as i16, y: y as i16, z: rp.0, big_waves: rp.1, small_waves: rp.2, flow: rp.3 });
        (self.vertices.len() - 1) as u16
    }

    fn file_flags(flag: i32) -> u16 {
        let mut f = 0;
        if flag & 1 == 0 {
            f |= wflag::INVISIBLE;
        }
        if flag & 2 != 0 {
            f |= wflag::LIMITED_DEPTH;
        }
        f
    }

    /// `AddWaterLevelQuad` (0x6E7EF0): sorted into an axis-aligned rectangle.
    fn add_quad(&mut self, v: [u16; 4], flag: i32) {
        let p = |i: u16| (self.vertices[i as usize].x, self.vertices[i as usize].y);
        if v.iter().all(|&i| p(i).0 == p(v[0]).0) || v.iter().all(|&i| p(i).1 == p(v[0]).1) {
            return;
        }
        let mut s = v;
        s.sort_by_key(|&i| (p(i).1, p(i).0));
        let (mut lo, mut hi) = ([s[0], s[1]], [s[2], s[3]]);
        lo.sort_by_key(|&i| p(i).0);
        hi.sort_by_key(|&i| p(i).0);
        self.quads.push(WaterQuad { v: [lo[0], lo[1], hi[0], hi[1]], flags: Self::file_flags(flag) });
    }

    /// `AddWaterLevelTriangle` (0x6E7D40).
    fn add_tri(&mut self, v: [u16; 3], flag: i32) {
        let p = |i: u16| (self.vertices[i as usize].x, self.vertices[i as usize].y);
        if v.iter().all(|&i| p(i).0 == p(v[0]).0) || v.iter().all(|&i| p(i).1 == p(v[0]).1) {
            return;
        }
        // The two vertices sharing y first (sorted by x), then the third.
        let mut best = None;
        for a in 0..3 {
            for b in 0..3 {
                if a != b && p(v[a]).1 == p(v[b]).1 && p(v[a]).0 < p(v[b]).0 {
                    best = Some((v[a], v[b], v[3 - a - b]));
                }
            }
        }
        if let Some((a, b, c)) = best {
            self.tris.push(WaterTri { v: [a, b, c], flags: Self::file_flags(flag) });
        }
    }

    /// `FillQuadsAndTrianglesList` (0x6E7B30).
    fn fill_grid(&mut self) {
        self.grid = vec![Cell::Empty; 144];
        for gx in 0..12 {
            for gy in 0..12 {
                let (cx0, cy0) = (gx as i32 * 500 - 3000, gy as i32 * 500 - 3000);
                let (cx1, cy1) = (cx0 + 500, cy0 + 500);
                let mut items = Vec::new();
                for (qi, q) in self.quads.iter().enumerate() {
                    let (v0, v1, v2) = (self.vertices[q.v[0] as usize], self.vertices[q.v[1] as usize], self.vertices[q.v[2] as usize]);
                    if cx0 < v1.x as i32 && (v0.x as i32) < cx1 && cy0 < v2.y as i32 && (v0.y as i32) < cy1 {
                        items.push((true, qi as u16));
                    }
                }
                for (ti, t) in self.tris.iter().enumerate() {
                    let (v0, v1, v2) = (self.vertices[t.v[0] as usize], self.vertices[t.v[1] as usize], self.vertices[t.v[2] as usize]);
                    let (ymin, ymax) = (v0.y.min(v2.y) as i32, v0.y.max(v2.y) as i32);
                    if cx0 < v1.x as i32 && (v0.x as i32) < cx1 && cy0 < ymax && ymin < cy1 {
                        items.push((false, ti as u16));
                    }
                }
                self.grid[gy + 12 * gx] = match items.len() {
                    0 => Cell::Empty,
                    1 => {
                        if items[0].0 {
                            Cell::Quad(items[0].1)
                        } else {
                            Cell::Tri(items[0].1)
                        }
                    }
                    _ => Cell::List(items),
                };
            }
        }
    }

    fn quad_level(&self, q: &WaterQuad, x: f32, y: f32, z: f32) -> Option<RenPar> {
        let v = |k: usize| &self.vertices[q.v[k] as usize];
        let (v0, v1, v2, v3) = (v(0), v(1), v(2), v(3));
        if x < v0.x as f32 || (v1.x as f32) < x || y < v0.y as f32 || (v2.y as f32) < y {
            return None;
        }
        let u = (x - v0.x as f32) / (v1.x - v0.x) as f32;
        let w = (y - v0.y as f32) / (v2.y - v0.y) as f32;
        let f = if u + w <= 1.0 {
            RenPar::lerp3(RenPar::of(v0), RenPar::of(v1), RenPar::of(v2), u, w)
        } else {
            RenPar::lerp3(RenPar::of(v3), RenPar::of(v2), RenPar::of(v1), 1.0 - u, 1.0 - w)
        };
        Self::depth_window(f, z, q.flags)
    }

    fn tri_level(&self, t: &WaterTri, x: f32, y: f32, z: f32) -> Option<RenPar> {
        let v = |k: usize| &self.vertices[t.v[k] as usize];
        let (v0, v1, v2) = (v(0), v(1), v(2));
        if x < v0.x as f32 || (v1.x as f32) < x {
            return None;
        }
        if y < v0.y.min(v2.y) as f32 || y > v0.y.max(v2.y) as f32 {
            return None;
        }
        let u = (x - v0.x as f32) / (v1.x - v0.x) as f32;
        let w = (y - v0.y as f32) / (v2.y - v0.y) as f32;
        let f = if v0.x == v2.x {
            if u + w > 1.0 {
                return None;
            }
            RenPar::lerp3(RenPar::of(v0), RenPar::of(v1), RenPar::of(v2), u, w)
        } else {
            if u < w {
                return None;
            }
            RenPar::lerp3(RenPar::of(v1), RenPar::of(v2), RenPar::of(v0), w, 1.0 - u)
        };
        Self::depth_window(f, z, t.flags)
    }

    fn depth_window(f: RenPar, z: f32, flags: u16) -> Option<RenPar> {
        if f.z - 6.0 > z && flags & wflag::LIMITED_DEPTH != 0 {
            return None;
        }
        if z > f.z + 20.0 {
            return None;
        }
        Some(f)
    }

    /// `GetWaterLevelNoWaves` (0x6E8580).
    pub fn level_no_waves(&self, x: f32, y: f32, z: f32) -> Option<RenPar> {
        let gx = (x * 0.002 + 6.0).floor() as i32;
        let gy = (y * 0.002 + 6.0).floor() as i32;
        if !(0..12).contains(&gx) || !(0..12).contains(&gy) {
            return Some(RenPar { z: 0.0, big: 1.0, small: 0.0 });
        }
        match &self.grid[(gy + 12 * gx) as usize] {
            Cell::Empty => None,
            Cell::Quad(q) => self.quad_level(&self.quads[*q as usize], x, y, z),
            Cell::Tri(t) => self.tri_level(&self.tris[*t as usize], x, y, z),
            Cell::List(l) => l.iter().find_map(|&(is_quad, i)| {
                if is_quad {
                    self.quad_level(&self.quads[i as usize], x, y, z)
                } else {
                    self.tri_level(&self.tris[i as usize], x, y, z)
                }
            }),
        }
    }

    /// `GetWaterLevel` (0x6EB690) with `AddWaveToResult` (0x6E81E0): (level, normal).
    pub fn level(&self, x: f32, y: f32, z: f32, touching: bool, wavyness: f32, t_ms: u32) -> Option<(f32, Vec3)> {
        let rp = self.level_no_waves(x, y, z)?;
        if !touching && rp.z - z > 3.0 {
            return None;
        }
        let hx = x * 0.5;
        let fx = hx - hx.floor();
        let ix = (hx.floor() * 2.0) as i32;
        let hy = y * 0.5;
        let fy = hy - hy.floor();
        let iy = (hy.floor() * 2.0) as i32;
        let w = |a: i32, b: i32| wave_height(a, b, rp.big, rp.small, wavyness, t_ms);
        let (level, n) = if fx + fy <= 1.0 {
            let (h00, h20, h02) = (w(ix, iy), w(ix + 2, iy), w(ix, iy + 2));
            (rp.z + h00 + fx * (h20 - h00) + fy * (h02 - h00), Vec3::new(-(h20 - h00), -(h02 - h00), 2.0))
        } else {
            let (h22, h02, h20) = (w(ix + 2, iy + 2), w(ix, iy + 2), w(ix + 2, iy));
            (rp.z + h22 + (1.0 - fx) * (h02 - h22) + (1.0 - fy) * (h20 - h22), Vec3::new(h02 - h22, h20 - h22, 2.0))
        };
        Some((level, n.normalize()))
    }
}

/// Inputs of `cBuoyancy::ProcessBuoyancy` for one body.
pub struct BuoyancyIn<'a> {
    pub matrix: &'a Matrix,
    pub bbox_min: Vec3,
    pub bbox_max: Vec3,
    pub is_ped: bool,
    pub touching: bool,
    /// m_fBuoyancy (B).
    pub b: f32,
    pub mass: f32,
    pub move_z: f32,
    pub ts: f32,
}

/// `ProcessBuoyancy` result: (lever arm in world axes, force); None when not in water.
pub fn process_buoyancy(w: &WaterLevel, i: &BuoyancyIn, wavyness: f32, t_ms: u32) -> Option<(Vec3, Vec3, f32)> {
    let pos = i.matrix.pos;
    let (level, _) = w.level(pos.x, pos.y, pos.z, i.touching, wavyness, t_ms)?;
    let (mut immersion, mut move_force, in_water);
    if i.is_ped {
        immersion = ((level - pos.z + 1.0) * 0.526_315_8).min(1.0);
        in_water = immersion >= 0.0;
        immersion = immersion.max(0.0);
        move_force = Vec3::ZERO;
    } else {
        // PreCalcSetup + SimpleCalcBuoyancy: a 3×3 grid over the bounding box.
        let (min, max) = (i.bbox_min, i.bbox_max);
        let h = (max - min) * 0.5;
        let m = h.max_element();
        let hn = if m > 0.0 { h / m } else { Vec3::ONE };
        let mut checked = 1.0f32;
        immersion = 0.0;
        move_force = Vec3::ZERO;
        let mut any = false;
        for ix in 0..3 {
            let x = min.x + ix as f32 * h.x;
            for iy in 0..3 {
                let y = min.y + iy as f32 * h.y;
                // FindWaterLevel: water height relative to the entity centre at that column.
                let r = i.matrix.rotate(Vec3::new(x, y, 0.0));
                let Some((wl, _)) = w.level(pos.x + r.x, pos.y + r.y, pos.z, true, wavyness, t_ms) else { continue };
                let mut pz = wl - (r.z + pos.z);
                let state = if pz > max.z {
                    pz = max.z;
                    2
                } else if pz < min.z {
                    pz = min.z;
                    0
                } else {
                    1
                };
                if state == 0 {
                    continue;
                }
                // SimpleSumBuoyancyData (volMult 1.0).
                let v = (pz - min.z).abs();
                if v < 0.0 {
                    continue;
                }
                immersion += v;
                let arm = Vec3::new(hn.x * x, hn.y * y, (pz + min.z) * 0.5 * hn.z);
                let k = 1.0 / checked;
                move_force = move_force * (1.0 - k) + arm * k * v;
                checked += 1.0;
                any = true;
            }
        }
        in_water = any;
        immersion /= (max.z - min.z) * 9.0;
    }
    if !in_water {
        return None;
    }
    // CalcBuoyancyForce (0x6C2750).
    let turn = i.matrix.rotate(move_force);
    let mut fz = immersion * i.b * i.ts;
    let p = i.mass * i.move_z;
    if p > 4.0 * fz {
        fz = (fz - p).max(0.0);
    }
    Some((turn, Vec3::new(0.0, 0.0, fz), level))
}

/// `PreCalcSetup` (0x6C2B90) bounding-box tweaks for boats (vehicle class 5).
pub fn boat_buoyancy_bbox(model: u16, min: Vec3, max: Vec3) -> (Vec3, Vec3) {
    let (mut min, mut max) = (min, max);
    match model {
        446 => {
            max.y *= 0.9;
            min.y *= 0.9;
        }
        452 => {
            max.y *= 1.25;
            min.y *= 0.83;
        }
        453 | 493 => min.y *= 0.9,
        454 => {
            max.y *= 1.3;
            min.y *= 0.82;
            min.z -= 0.2;
        }
        472 => {
            max.y *= 1.1;
            min.y *= 0.9;
            min.z -= 0.3;
        }
        473 => {
            max.y *= 1.3;
            min.y *= 0.9;
            min.z -= 0.2;
        }
        484 => {
            max.y *= 1.1;
            min.y *= 0.9;
        }
        595 => {
            max.y *= 1.25;
            min.y *= 0.8;
            min.z -= 0.1;
        }
        _ => {
            max.y *= 1.05;
            min.y *= 0.9;
        }
    }
    (min, max)
}

/// Per-column volume multipliers of `ProcessBuoyancyBoat` (index `ix + 3·iy`, iy 0 = rear).
fn boat_volume_table(model: u16) -> [f32; 9] {
    match model {
        446 | 452 | 493 | 595 => [0.7, 0.9, 0.7, 0.95, 1.0, 0.95, 0.6, 0.7, 0.6],
        472 | 473 => [0.65, 0.85, 0.65, 0.85, 1.1, 0.85, 0.65, 0.95, 0.65],
        484 => [0.55, 0.95, 0.55, 0.75, 1.1, 0.75, 0.3, 0.8, 0.3],
        _ => [0.75, 0.9, 0.75, 0.95, 1.0, 0.95, 0.4, 0.7, 0.4],
    }
}

/// Inputs of `cBuoyancy::ProcessBuoyancyBoat`.
pub struct BoatBuoyancyIn<'a> {
    pub matrix: &'a Matrix,
    /// The col model's bounding box (the boat tweaks are applied here).
    pub bbox_min: Vec3,
    pub bbox_max: Vec3,
    pub model: u16,
    pub touching: bool,
    pub b: f32,
    /// handling fSuspensionDampingLevel (+0xB0): the water damping multiplier.
    pub damping: f32,
    pub ts: f32,
    /// bNoTurn: the per-point turn forces are not produced.
    pub no_turn: bool,
}

/// `ProcessBuoyancyBoat` result.
pub struct BoatBuoyancy {
    pub turn_point: Vec3,
    pub force: Vec3,
    /// m_fEntityWaterImmersion.
    pub immersion: f32,
    /// ApplyTurnForce(force, offset) calls made during the sampling.
    pub turn_forces: Vec<(Vec3, Vec3)>,
}

/// `cBuoyancy::ProcessBuoyancyBoat` (0x6C3030). `speed_at(offset)` = CPhysical::GetSpeed.
pub fn process_buoyancy_boat(
    w: &WaterLevel,
    i: &BoatBuoyancyIn,
    speed_at: impl Fn(Vec3) -> Vec3,
    wavyness: f32,
    t_ms: u32,
) -> Option<BoatBuoyancy> {
    let pos = i.matrix.pos;
    w.level(pos.x, pos.y, pos.z, i.touching, wavyness, t_ms)?;
    let (min, max) = boat_buoyancy_bbox(i.model, i.bbox_min, i.bbox_max);
    let h = (max - min) * 0.5;
    // Largest axis normalised (ties: z only if strictly largest, then y, else x).
    let m = if h.z > h.x && h.z > h.y {
        h.z
    } else if h.y >= h.x {
        h.y
    } else {
        h.x
    };
    let hn = if m > 0.0 { h / m } else { Vec3::ONE };
    let inv_norm = 1.0 / ((max.z - min.z) * 9.0);
    let table = boat_volume_table(i.model);
    let mut out = BoatBuoyancy { turn_point: Vec3::ZERO, force: Vec3::ZERO, immersion: 0.0, turn_forces: Vec::new() };
    let mut move_force = Vec3::ZERO;
    let mut checked = 1.0f32;
    for ix in 0..3 {
        let x = min.x + ix as f32 * h.x;
        for iy in 0..3 {
            let y = min.y + iy as f32 * h.y;
            // FindWaterLevelNorm.
            let r = i.matrix.rotate(Vec3::new(x, y, 0.0));
            let Some((wl, n)) = w.level(pos.x + r.x, pos.y + r.y, pos.z, true, wavyness, t_ms) else { continue };
            let mut pz = wl - (r.z + pos.z);
            let state = if pz > max.z {
                pz = max.z;
                2
            } else if pz < min.z {
                pz = min.z;
                0
            } else {
                1
            };
            let n2 = Vec3::new(n.x, n.y, n.z + 2.0) / 3.0;
            let vol = table[ix + 3 * iy];
            if state == 0 {
                continue;
            }
            // SimpleSumBuoyancyData with the boat's volume multiplier.
            let mut v = (pz - min.z).abs() - (1.0 - vol);
            let mut f = 0.0;
            if v >= 0.0 {
                v = (vol * v) * (vol * v);
                out.immersion += v;
                let arm = Vec3::new(hn.x * x, hn.y * y, (pz + min.z) * 0.5 * hn.z);
                let k = 1.0 / checked;
                move_force = move_force * (1.0 - k) + arm * k * v;
                checked += 1.0;
                f = v;
            }
            let fz = f * inv_norm * i.ts * i.b;
            let vp = speed_at(r);
            let d = (1.0 - vp.dot(n2) * i.damping).max(0.0);
            out.force.z += fz * d;
            if !i.no_turn {
                out.turn_forces.push((n2 * fz * d, i.matrix.rotate(Vec3::new(x, y, pz))));
            }
        }
    }
    out.immersion *= inv_norm;
    out.turn_point = i.matrix.rotate(move_force);
    Some(out)
}

/// Helper for the renderer: the 2-unit lattice heights of a rectangle are computed by the app.
pub fn lattice_coord(v: f32) -> i32 {
    ((v * 0.5).floor() * 2.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_level_and_ocean() {
        let w = WaterLevel::parse(
            "processed\n\
             0 0 1 0 0 1 0  100 0 1 0 0 1 0  0 100 1 0 0 1 0  100 100 1 0 0 1 0  1\n",
        );
        assert_eq!(w.quads.len(), 1);
        let r = w.level_no_waves(50.0, 50.0, 1.0).unwrap();
        assert!((r.z - 1.0).abs() < 1e-6);
        assert!(w.level_no_waves(50.0, 50.0, 30.0).is_none(), "more than 20 above");
        let o = w.level_no_waves(5000.0, 0.0, 0.0).unwrap();
        assert_eq!((o.z, o.big), (0.0, 1.0));
    }
}
