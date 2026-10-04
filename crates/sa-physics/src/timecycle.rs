//! `CTimeCycle` (0x5BBAC0 Initialise, 0x5603D0 CalcColoursForPoint): timecyc.dat and the
//! per-frame time-of-day / weather colours, the sun vector and the below-horizon grey.
//!
//! Not ported: time-cycle boxes (IPL `tcyc`), script extra colours, night vision /
//! infrared, the LOD multiplier, FogReduction (always 0 here).

use glam::Vec3;

/// Slot hours (0x8CDECC).
const HOURS: [f32; 9] = [0.0, 5.0, 6.0, 7.0, 12.0, 19.0, 20.0, 22.0, 24.0];
/// Below-horizon grey per slot (0x8CDED8).
const BELOW_HORIZON_GREY: [f32; 8] = [30.0, 30.0, 30.0, 50.0, 60.0, 60.0, 50.0, 35.0];
pub const NUM_WEATHERS: usize = 23;
const NUM_SLOTS: usize = 8;

/// One row of timecyc.dat as stored (§1.3 of timecycle.md).
#[derive(Debug, Clone, Copy, Default)]
struct Row {
    amb: [u8; 3],
    amb_obj: [u8; 3],
    sky_top: [u8; 3],
    sky_bot: [u8; 3],
    sun_core: [u8; 3],
    sun_corona: [u8; 3],
    sun_size: u8,
    sprite_size: u8,
    sprite_bright: u8,
    shadows: [u8; 3],
    far_clip: i16,
    fog_start: i16,
    light_on_ground: u8,
    low_clouds: [u8; 3],
    fluffy: [u8; 3],
    water: [u8; 4],
    pfx1: [u8; 4],
    pfx2: [u8; 4],
    cloud_alpha: u8,
    intensity_limit: u8,
    water_fog_alpha: u8,
    dir_mult: u8,
}

/// `CColourSet` (0xAC bytes): integer-typed fields hold whole numbers (they are truncated
/// on interpolation), colours are 0..255 except the ambients after `calc` (0..1).
#[derive(Debug, Clone, Copy, Default)]
pub struct ColourSet {
    pub ambient: Vec3,
    pub ambient_obj: Vec3,
    pub ambient_before_brightness: Vec3,
    pub sky_top: [f32; 3],
    pub sky_bottom: [f32; 3],
    pub sun_core: [f32; 3],
    pub sun_corona: [f32; 3],
    /// x10 (SunSz 1.0 = 10).
    pub sun_size: f32,
    pub sprite_size: f32,
    pub sprite_brightness: f32,
    pub shadows: [f32; 3],
    pub far_clip: f32,
    pub fog_start: f32,
    pub light_on_ground: f32,
    pub low_clouds: [f32; 3],
    pub fluffy_bottom: [f32; 3],
    pub water: [f32; 4],
    pub post_fx1: [f32; 4],
    pub post_fx2: [f32; 4],
    pub cloud_alpha: f32,
    pub intensity_limit: f32,
    pub water_fog_alpha: f32,
    pub dir_mult: f32,
    pub lod_mult: f32,
}

impl ColourSet {
    /// `CColourSet::CColourSet(slot, weather)` (0x55F4B0).
    fn from_row(r: &Row) -> Self {
        let f3 = |c: [u8; 3]| [c[0] as f32, c[1] as f32, c[2] as f32];
        let f4 = |c: [u8; 4]| [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32];
        Self {
            ambient: Vec3::from(f3(r.amb)),
            ambient_obj: Vec3::from(f3(r.amb_obj)),
            ambient_before_brightness: Vec3::ZERO,
            sky_top: f3(r.sky_top),
            sky_bottom: f3(r.sky_bot),
            sun_core: f3(r.sun_core),
            sun_corona: f3(r.sun_corona),
            sun_size: r.sun_size as i8 as f32,
            sprite_size: r.sprite_size as i8 as f32,
            sprite_brightness: r.sprite_bright as i8 as f32,
            shadows: f3(r.shadows),
            far_clip: r.far_clip as f32,
            fog_start: r.fog_start as f32,
            light_on_ground: r.light_on_ground as f32,
            low_clouds: f3(r.low_clouds),
            fluffy_bottom: f3(r.fluffy),
            water: f4(r.water),
            post_fx1: f4(r.pfx1),
            post_fx2: f4(r.pfx2),
            cloud_alpha: r.cloud_alpha as f32,
            intensity_limit: r.intensity_limit as f32,
            water_fog_alpha: r.water_fog_alpha as f32,
            dir_mult: r.dir_mult as f32 * 0.01,
            lod_mult: 1.0,
        }
    }

    /// `CColourSet::Interpolate` (0x55F870): floats blend, integer fields truncate.
    pub fn interpolate(a: &Self, b: &Self, fa: f32, fb: f32, ignore_sky: bool, base: &Self) -> Self {
        let lf = |x: f32, y: f32| x * fa + y * fb;
        let li = |x: f32, y: f32| ((x * fa + y * fb) as i32) as f32;
        let i3 = |x: [f32; 3], y: [f32; 3]| [li(x[0], y[0]), li(x[1], y[1]), li(x[2], y[2])];
        let f4 = |x: [f32; 4], y: [f32; 4]| [lf(x[0], y[0]), lf(x[1], y[1]), lf(x[2], y[2]), lf(x[3], y[3])];
        let mut o = Self {
            ambient: a.ambient * fa + b.ambient * fb,
            ambient_obj: a.ambient_obj * fa + b.ambient_obj * fb,
            ambient_before_brightness: a.ambient_before_brightness * fa + b.ambient_before_brightness * fb,
            sprite_size: lf(a.sprite_size, b.sprite_size),
            sprite_brightness: lf(a.sprite_brightness, b.sprite_brightness),
            shadows: i3(a.shadows, b.shadows),
            far_clip: lf(a.far_clip, b.far_clip),
            fog_start: lf(a.fog_start, b.fog_start),
            light_on_ground: lf(a.light_on_ground, b.light_on_ground),
            water: f4(a.water, b.water),
            post_fx1: f4(a.post_fx1, b.post_fx1),
            post_fx2: f4(a.post_fx2, b.post_fx2),
            cloud_alpha: lf(a.cloud_alpha, b.cloud_alpha),
            intensity_limit: li(a.intensity_limit, b.intensity_limit),
            water_fog_alpha: li(a.water_fog_alpha, b.water_fog_alpha),
            dir_mult: lf(a.dir_mult, b.dir_mult),
            lod_mult: lf(a.lod_mult, b.lod_mult),
            ..*base
        };
        if !ignore_sky {
            o.sky_top = i3(a.sky_top, b.sky_top);
            o.sky_bottom = i3(a.sky_bottom, b.sky_bottom);
            o.sun_core = i3(a.sun_core, b.sun_core);
            o.sun_corona = i3(a.sun_corona, b.sun_corona);
            o.sun_size = lf(a.sun_size, b.sun_size);
            o.low_clouds = i3(a.low_clouds, b.low_clouds);
            o.fluffy_bottom = i3(a.fluffy_bottom, b.fluffy_bottom);
        }
        o
    }

    fn lerp(a: &Self, b: &Self, fa: f32, fb: f32) -> Self {
        Self::interpolate(a, b, fa, fb, false, a)
    }
}

/// A `%d` / `%f` field as sscanf leaves it in its stack variable.
#[derive(Debug, Clone, Copy)]
enum Val {
    I(i32),
    F(f32),
}

/// The 52-field format (0x86A6D8): true = %f.
fn field_is_float(k: usize) -> bool {
    matches!(k, 21..=23 | 27..=29 | 36..=48 | 51)
}

/// MSVC sscanf over `line` into `v`, stopping at the first failed conversion; fields
/// after it keep their previous values.
fn scan(line: &str, v: &mut [Val; 52]) {
    let b = line.as_bytes();
    let mut p = 0;
    for (k, slot) in v.iter_mut().enumerate() {
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        let start = p;
        if p < b.len() && (b[p] == b'-' || b[p] == b'+') {
            p += 1;
        }
        let digits_start = p;
        while p < b.len() && b[p].is_ascii_digit() {
            p += 1;
        }
        let mut any = p > digits_start;
        if field_is_float(k) {
            if p < b.len() && b[p] == b'.' {
                p += 1;
                let fs = p;
                while p < b.len() && b[p].is_ascii_digit() {
                    p += 1;
                }
                any |= p > fs;
            }
            if any && p < b.len() && (b[p] == b'e' || b[p] == b'E') {
                let save = p;
                p += 1;
                if p < b.len() && (b[p] == b'-' || b[p] == b'+') {
                    p += 1;
                }
                let es = p;
                while p < b.len() && b[p].is_ascii_digit() {
                    p += 1;
                }
                if p == es {
                    p = save;
                }
            }
            if !any {
                return;
            }
            *slot = Val::F(line[start..p].parse().unwrap_or(0.0));
        } else {
            if !any {
                return;
            }
            *slot = Val::I(line[start..p].parse().unwrap_or(0));
        }
    }
}

/// The time cycle and its current colours.
#[derive(Debug, Clone)]
pub struct TimeCycle {
    rows: Vec<Row>,
    pub current: ColourSet,
    pub vector_to_sun: Vec3,
    pub below_horizon_grey: [f32; 3],
    /// `CCoronas::LightsMult` (sun dazzle; 1.0 here).
    pub lights_mult: f32,
    /// `FrontEndMenuManager.m_PrefsBrightness` (256 = neutral).
    pub brightness: i32,
}

impl TimeCycle {
    /// `CTimeCycle::Initialise` (0x5BBAC0): 23 weather blocks x 8 rows, `/` comments.
    pub fn parse(text: &str) -> Self {
        // Uninitialised stack before the first line: zeros (so the missing PC DirectionalMult
        // column stores 0, timecycle.md §1.4).
        let mut v = [Val::I(0); 52];
        for (k, s) in v.iter_mut().enumerate() {
            if field_is_float(k) {
                *s = Val::F(0.0);
            }
        }
        let mut rows = vec![Row::default(); NUM_SLOTS * NUM_WEATHERS];
        let mut lines = text.lines().map(|l| {
            // LoadLine: control bytes and commas become spaces, leading blanks skipped.
            let l: String = l.chars().map(|c| if (c as u32) < 0x20 || c == ',' { ' ' } else { c }).collect();
            l.trim_start().to_string()
        });
        'outer: for w in 0..NUM_WEATHERS {
            for s in 0..NUM_SLOTS {
                let line = loop {
                    match lines.next() {
                        Some(l) if l.is_empty() || l.starts_with('/') => continue,
                        Some(l) => break l,
                        None => break 'outer,
                    }
                };
                scan(&line, &mut v);
                rows[s * NUM_WEATHERS + w] = store(&v);
            }
        }
        Self {
            rows,
            current: ColourSet::default(),
            vector_to_sun: Vec3::Z,
            below_horizon_grey: [30.0; 3],
            lights_mult: 1.0,
            brightness: 256,
        }
    }

    fn set(&self, slot: usize, weather: usize) -> ColourSet {
        ColourSet::from_row(&self.rows[slot * NUM_WEATHERS + weather.min(NUM_WEATHERS - 1)])
    }

    /// `CTimeCycle::CalcColoursForPoint` (0x5603D0) for the camera.
    #[allow(clippy::too_many_arguments)]
    pub fn calc(
        &mut self,
        hours: u8,
        minutes: u8,
        seconds: u16,
        old: i16,
        new: i16,
        interp: f32,
        cam_pos: Vec3,
        under_waterness: f32,
        in_tunnelness: f32,
    ) {
        let t = (hours as f32 + minutes as f32 * (1.0 / 60.0) + (seconds as u8) as f32 * (1.0 / 3600.0)).min(23.999);
        let i = (0..8).rev().find(|&k| HOURS[k] <= t).unwrap_or(0);
        let j = (i + 1) % 8;
        let f = (t - HOURS[i]) / (HOURS[i + 1] - HOURS[i]);
        let (old, new) = (old.max(0) as usize, new.max(0) as usize);
        let mut oc = self.set(i, old);
        let mut on = self.set(j, old);
        let mut nc = self.set(i, new);
        let mut nn = self.set(j, new);
        // Smog weathers fade to their clean version with camera height.
        let h = ((cam_pos.z - 20.0) * 0.005).clamp(0.0, 1.0);
        if h > 0.0 {
            for (w, c, n) in [(old, &mut oc, &mut on), (new, &mut nc, &mut nn)] {
                let clean = match w {
                    2 => Some(0),
                    3 => Some(1),
                    _ => None,
                };
                if let Some(cw) = clean {
                    *c = ColourSet::lerp(c, &self.set(i, cw), 1.0 - h, h);
                    *n = ColourSet::lerp(n, &self.set(j, cw), 1.0 - h, h);
                }
            }
        }
        let o = ColourSet::lerp(&oc, &on, 1.0 - f, f);
        let n = ColourSet::lerp(&nc, &nn, 1.0 - f, f);
        let mut out = ColourSet::lerp(&o, &n, 1.0 - interp, interp);
        // Sky brightening while dazzled by the sun.
        let k = (1.0 / self.lights_mult + 3.0) * 0.25;
        for c in out.sky_top.iter_mut().chain(out.sky_bottom.iter_mut()) {
            *c = ((*c * k) as i32).min(255) as f32;
        }
        // Sun vector.
        let a = ((hours as u32 * 60 + minutes as u32) as f32 + seconds as f32 * (1.0 / 60.0)) * 0.004_363_323;
        self.vector_to_sun = Vec3::new(a.sin() + 0.7, -0.7, 0.2 - a.cos()).normalize();
        // Underwater and tunnels.
        if under_waterness > 0.0 {
            let u = ColourSet::lerp(&self.set(i, 20), &self.set(j, 20), 1.0 - f, f);
            out = ColourSet::interpolate(&out, &u, 1.0 - under_waterness, under_waterness, false, &out);
        }
        if in_tunnelness > 0.0 {
            let tset = self.set(1, 22);
            let ignore = tset.sky_top == [0.0; 3];
            out = ColourSet::interpolate(&out, &tset, 1.0 - in_tunnelness, in_tunnelness, ignore, &out);
        }
        // Ambient quantised to 0..1.
        let q = |v: Vec3| Vec3::new((v.x as i32) as f32, (v.y as i32) as f32, (v.z as i32) as f32) * (1.0 / 255.0);
        out.ambient = q(out.ambient);
        out.ambient_obj = q(out.ambient_obj);
        // Altitude cap of the far clip.
        let z = cam_pos.z;
        if z >= 200.0 {
            if z > 500.0 {
                out.far_clip = out.far_clip.min(1000.0);
            } else if out.far_clip > 1000.0 {
                let t2 = (z - 200.0) * (1.0 / 300.0);
                out.far_clip = t2 * 1000.0 + (1.0 - t2) * out.far_clip;
            }
        }
        // Grey below the horizon.
        let g = ((BELOW_HORIZON_GREY[i] * (1.0 - f) + BELOW_HORIZON_GREY[j] * f) as i32) as f32;
        self.below_horizon_grey =
            std::array::from_fn(|c| ((g * (1.0 - under_waterness) + out.sky_bottom[c] * under_waterness) as i32) as f32);
        // Menu brightness.
        out.ambient_before_brightness = out.ambient;
        let b = self.brightness as f32;
        if b < 256.0 {
            out.ambient *= b * (1.0 / 256.0) * 0.8 + 0.2;
        } else {
            let m = (b - 256.0) * (1.0 / 128.0) + 1.0;
            let mx = out.ambient.max_element();
            out.ambient += Vec3::splat(mx * m - mx);
        }
        self.current = out;
    }
}

/// Convert the stack values of one line to the stored row (timecycle.md §1.3).
fn store(v: &[Val; 52]) -> Row {
    let i = |k: usize| match v[k] {
        Val::I(x) => x,
        Val::F(x) => x as i32,
    };
    let f = |k: usize| match v[k] {
        Val::F(x) => x,
        Val::I(x) => x as f32,
    };
    let u = |k: usize| i(k) as u8;
    let x10 = |k: usize| ((f(k) * 10.0 + 0.5) as i32) as u8;
    let fu = |k: usize| (f(k) as i32) as u8;
    Row {
        amb: [u(0), u(1), u(2)],
        amb_obj: [u(3), u(4), u(5)],
        sky_top: [u(9), u(10), u(11)],
        sky_bot: [u(12), u(13), u(14)],
        sun_core: [u(15), u(16), u(17)],
        sun_corona: [u(18), u(19), u(20)],
        sun_size: x10(21),
        sprite_size: x10(22),
        sprite_bright: x10(23),
        shadows: [u(24), u(25), u(26)],
        far_clip: f(27) as i32 as i16,
        fog_start: f(28) as i32 as i16,
        light_on_ground: x10(29),
        low_clouds: [u(30), u(31), u(32)],
        fluffy: [u(33), u(34), u(35)],
        water: [fu(36), fu(37), fu(38), fu(39)],
        // Alpha stored as ftol(2a) & 0xFF (255 -> 254).
        pfx1: [fu(41), fu(42), fu(43), ((f(40) + f(40)) as i32) as u8],
        pfx2: [fu(45), fu(46), fu(47), ((f(44) + f(44)) as i32) as u8],
        cloud_alpha: fu(48),
        intensity_limit: u(49),
        water_fog_alpha: u(50),
        dir_mult: (((fu(51)) as f32 * 100.0) as i32) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs the game's timecyc.dat"]
    fn loads_the_pc_timecyc_with_its_quirks() {
        let path = r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas\data\timecyc.dat";
        let tc = TimeCycle::parse(&std::fs::read_to_string(path).unwrap());
        // RAINY_COUNTRYSIDE 8PM row is malformed (timecycle.md §1.4 item 3).
        let r = &tc.rows[6 * NUM_WEATHERS + 16];
        assert_eq!(r.amb, [255, 167, 198]);
        assert_eq!(r.amb_obj, [223, 255, 255]);
        assert_eq!(r.sun_corona, [0, 2, 0]);
        assert_eq!(r.far_clip, 650);
        // PostFx alpha 255 -> 254, DirMult 0.
        let n = &tc.rows[4 * NUM_WEATHERS];
        assert_eq!(n.pfx1[3], 254);
        assert_eq!(n.dir_mult, 0);
    }

    #[test]
    fn sun_is_up_between_0514_and_1846() {
        let mut tc = TimeCycle {
            rows: vec![Row::default(); NUM_SLOTS * NUM_WEATHERS],
            current: ColourSet::default(),
            vector_to_sun: Vec3::Z,
            below_horizon_grey: [0.0; 3],
            lights_mult: 1.0,
            brightness: 256,
        };
        let mut up = |h, m| {
            tc.calc(h, m, 0, 0, 0, 0.0, Vec3::ZERO, 0.0, 0.0);
            tc.vector_to_sun.z > 0.0
        };
        assert!(!up(5, 13) && up(5, 15) && up(12, 0) && up(18, 45) && !up(18, 47));
    }
}
