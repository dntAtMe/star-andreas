//! Blueprints as the game stores them after `FxManager_c::LoadFxProject` (0x5C2420):
//! key times as `u16 = ftol(t*256)`, values quantised per interp kind, CULLDIST x256,
//! LODSTART/LODEND x64, the prim matrix as i16/32767.

use anyhow::{Result, bail};
use glam::{Affine3A, Vec3};
use sa_formats::fxp;

/// FxInterpInfo kinds (ctors 0x4A8440 Float, 0x4A87D0 U256, 0x4A8990 S1000, 0x4A8B50 S128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Float,
    U256,
    S1000,
    S128,
}

fn quantise(kind: Kind, v: f32) -> f32 {
    match kind {
        Kind::Float => v,
        Kind::U256 => ((v * 256.0) as i32 as u16) as f32 * (1.0 / 256.0),
        Kind::S1000 => ((v * 1000.0) as i32 as i16) as f32 * 0.001,
        Kind::S128 => ((v * 128.0) as i32 as i16) as f32 * (1.0 / 128.0),
    }
}

/// A keyframed curve with one shared time array (`FxInterpInfo_c`).
#[derive(Debug, Clone, Default)]
pub struct Interp {
    pub looped: bool,
    /// Key times x256.
    pub times: Vec<u16>,
    /// Decoded values per track.
    pub tracks: Vec<Vec<f32>>,
}

impl Interp {
    fn time(&self, k: usize) -> f32 {
        self.times[k] as f32 / 256.0
    }

    fn lerp(&self, tr: usize, a: usize, b: usize, t: f32) -> f32 {
        let v = &self.tracks[tr];
        let (ta, tb) = (self.time(a), self.time(b));
        let r = (t - ta) / (tb - ta);
        (v[b] - v[a]) * r + v[a]
    }

    /// `GetVal` (0x4A8470 and siblings) for one track.
    pub fn val(&self, tr: usize, mut t: f32) -> f32 {
        let v = &self.tracks[tr];
        let n = self.times.len().min(v.len());
        if n == 0 {
            return 0.0;
        }
        if n == 1 {
            return v[0];
        }
        if self.looped {
            let period = self.time(n - 1);
            if period > 0.0 {
                t -= ((t / period) as i32) as f32 * period;
            }
        }
        for k in 1..n {
            if t < self.time(k) {
                return self.lerp(tr, k - 1, k, t);
            }
        }
        v[n - 1]
    }

    /// `FxInterpInfoFloat_c::GetValIntegral` (0x4A85C0): trapezoid area over [t-dt, t].
    /// Ignores LOOPED; the area past the last key is not counted (a quirk of the original).
    pub fn integral(&self, tr: usize, t: f32, dt: f32) -> f32 {
        let v = &self.tracks[tr];
        let n = self.times.len().min(v.len());
        if n == 0 {
            return 0.0;
        }
        if n == 1 {
            return dt * v[0];
        }
        let t0 = t - dt;
        let Some(k) = (0..n).find(|&k| t0 < self.time(k)) else {
            return dt * v[n - 1];
        };
        let mut cur_t = t0;
        let mut cur_v = if k == 0 { v[0] } else { self.lerp(tr, k - 1, k, t0) };
        let mut area = 0.0;
        for j in k..n {
            let tj = self.time(j);
            if tj == t {
                area += (tj - cur_t) * (cur_v + 0.5 * (v[j] - cur_v));
                break;
            }
            if tj < t {
                area += (tj - cur_t) * (cur_v + 0.5 * (v[j] - cur_v));
                cur_t = tj;
                cur_v = v[j];
            } else {
                let vt = self.lerp(tr, j - 1, j, t);
                area += (t - cur_t) * (cur_v + 0.5 * (vt - cur_v));
                break;
            }
        }
        area
    }
}

/// Info type codes (`FxInfoManager_c::CreateInfo` 0x4A7B00).
pub mod ty {
    pub const EMRATE: u16 = 0x1001;
    pub const EMSIZE: u16 = 0x1004;
    pub const EMSPEED: u16 = 0x1008;
    pub const EMDIR: u16 = 0x1010;
    pub const EMANGLE: u16 = 0x1020;
    pub const EMLIFE: u16 = 0x1040;
    pub const EMPOS: u16 = 0x1080;
    pub const EMWEATHER: u16 = 0x1100;
    pub const EMROTATION: u16 = 0x1200;
    pub const NOISE: u16 = 0x2001;
    pub const FORCE: u16 = 0x2002;
    pub const FRICTION: u16 = 0x2004;
    pub const ATTRACTPT: u16 = 0x2008;
    pub const ATTRACTLINE: u16 = 0x2010;
    pub const GROUNDCOLLIDE: u16 = 0x2020;
    pub const WIND: u16 = 0x2040;
    pub const JITTER: u16 = 0x2080;
    pub const ROTSPEED: u16 = 0x2100;
    pub const FLOAT: u16 = 0x2200;
    pub const UNDERWATER: u16 = 0x2400;
    pub const COLOUR: u16 = 0x4001;
    pub const SIZE: u16 = 0x4002;
    pub const SPRITERECT: u16 = 0x4004;
    pub const HEATHAZE: u16 = 0x4008;
    pub const TRAIL: u16 = 0x4010;
    pub const FLAT: u16 = 0x4020;
    pub const DIR: u16 = 0x4040;
    pub const ANIMTEX: u16 = 0x4080;
    pub const COLOURRANGE: u16 = 0x4100;
    pub const SELFLIT: u16 = 0x4200;
    pub const COLOURBRIGHT: u16 = 0x4400;
    pub const SMOKE: u16 = 0x8001;
}

/// (fxp tag, type, interp kind, track count).
const INFO_TABLE: &[(&str, u16, Kind, usize)] = &[
    ("EMRATE", ty::EMRATE, Kind::Float, 1),
    ("EMSIZE", ty::EMSIZE, Kind::S1000, 7),
    ("EMSPEED", ty::EMSPEED, Kind::S1000, 2),
    ("EMDIR", ty::EMDIR, Kind::S1000, 3),
    ("EMANGLE", ty::EMANGLE, Kind::Float, 2),
    ("EMLIFE", ty::EMLIFE, Kind::U256, 2),
    ("EMPOS", ty::EMPOS, Kind::Float, 3),
    ("EMWEATHER", ty::EMWEATHER, Kind::Float, 4),
    ("EMROTATION", ty::EMROTATION, Kind::Float, 2),
    ("NOISE", ty::NOISE, Kind::S1000, 1),
    ("FORCE", ty::FORCE, Kind::S1000, 3),
    ("FRICTION", ty::FRICTION, Kind::S1000, 1),
    ("ATTRACTPT", ty::ATTRACTPT, Kind::S1000, 4),
    ("ATTRACTLINE", ty::ATTRACTLINE, Kind::S1000, 7),
    ("GROUNDCOLLIDE", ty::GROUNDCOLLIDE, Kind::S1000, 3),
    ("WIND", ty::WIND, Kind::S1000, 1),
    ("JITTER", ty::JITTER, Kind::S1000, 1),
    ("ROTSPEED", ty::ROTSPEED, Kind::Float, 4),
    ("FLOAT", ty::FLOAT, Kind::Float, 0),
    ("UNDERWATER", ty::UNDERWATER, Kind::U256, 0),
    ("COLOUR", ty::COLOUR, Kind::U256, 4),
    ("SIZE", ty::SIZE, Kind::S1000, 4),
    ("SPRITERECT", ty::SPRITERECT, Kind::S128, 4),
    ("HEATHAZE", ty::HEATHAZE, Kind::S128, 0),
    ("TRAIL", ty::TRAIL, Kind::U256, 2),
    ("FLAT", ty::FLAT, Kind::Float, 9),
    ("DIR", ty::DIR, Kind::Float, 3),
    ("ANIMTEX", ty::ANIMTEX, Kind::S1000, 1),
    ("COLOURRANGE", ty::COLOURRANGE, Kind::U256, 7),
    ("SELFLIT", ty::SELFLIT, Kind::U256, 0),
    ("COLOURBRIGHT", ty::COLOURBRIGHT, Kind::U256, 5),
    ("SMOKE", ty::SMOKE, Kind::U256, 8),
];

#[derive(Debug, Clone)]
pub struct Info {
    pub ty: u16,
    /// TIMEMODEPRT (movement and render infos; 1 when absent).
    pub prt: bool,
    pub interp: Interp,
}

impl Info {
    pub fn val(&self, tr: usize, t: f32) -> f32 {
        self.interp.val(tr, t)
    }

    fn from_fxp(i: &fxp::Info) -> Result<Self> {
        let Some(&(_, ty, kind, ntracks)) = INFO_TABLE.iter().find(|e| e.0 == i.kind) else {
            bail!("unknown FX info {}", i.kind);
        };
        let mut interp = Interp::default();
        // The ctor fixes the track count; tracks are read by position.
        for (k, (_, c)) in i.fields.iter().take(ntracks).enumerate() {
            if k == 0 {
                interp.times = c.keys.iter().map(|&(t, _)| (t * 256.0) as i32 as u16).collect();
            }
            // LOOPED / NUM_KEYS are overwritten by every track; times by every track too.
            interp.looped = c.looped;
            for (slot, &(t, _)) in interp.times.iter_mut().zip(&c.keys) {
                *slot = (t * 256.0) as i32 as u16;
            }
            interp.tracks.push(c.keys.iter().map(|&(_, v)| quantise(kind, v)).collect());
        }
        Ok(Self { ty, prt: i.time_mode_prt.unwrap_or(true), interp })
    }
}

/// RenderWare blend factors (`rwBlend = id + 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    InvSrcColor,
    SrcAlpha,
    InvSrcAlpha,
    DestAlpha,
    InvDestAlpha,
    DestColor,
    InvDestColor,
    SrcAlphaSat,
}

impl BlendFactor {
    fn from_id(id: u32) -> Self {
        use BlendFactor::*;
        [Zero, One, SrcColor, InvSrcColor, SrcAlpha, InvSrcAlpha, DestAlpha, InvDestAlpha, DestColor, InvDestColor, SrcAlphaSat]
            .get(id as usize)
            .copied()
            .unwrap_or(One)
    }
}

/// `FxEmitterBP_c`.
#[derive(Debug, Clone)]
pub struct PrimBp {
    pub name: String,
    pub src_blend: BlendFactor,
    pub dst_blend: BlendFactor,
    pub alpha_on: bool,
    /// Quantised prim matrix (i16/32767, translation included); None = identity.
    pub matrix: Option<Affine3A>,
    pub textures: [Option<String>; 4],
    pub infos: Vec<Info>,
    pub first_movement: usize,
    pub first_render: usize,
    /// LODSTART / LODEND x64.
    pub lod_start: u16,
    pub lod_end: u16,
    pub has_flat: bool,
    pub has_heat_haze: bool,
}

impl PrimBp {
    pub fn has_info(&self, t: u16) -> bool {
        self.infos.iter().any(|i| i.ty == t)
    }
}

/// `FxSystemBP_c`.
#[derive(Debug, Clone)]
pub struct SystemBp {
    pub name: String,
    pub length: f32,
    pub loop_interval_min: f32,
    pub loop_interval_max: f32,
    /// CULLDIST x256.
    pub cull_dist: u16,
    pub play_mode: u8,
    /// Bounding sphere (centre, radius), only when the radius is > 0.
    pub sphere: Option<(Vec3, f32)>,
    pub prims: Vec<PrimBp>,
}

impl SystemBp {
    pub fn from_fxp(s: &fxp::SystemBp) -> Result<Self> {
        let prims = s
            .prims
            .iter()
            .map(|p| {
                let infos = p.infos.iter().map(Info::from_fxp).collect::<Result<Vec<_>>>()?;
                let first_render = infos.iter().position(|i| i.ty & 0xC000 != 0).unwrap_or(infos.len());
                let first_movement = infos.iter().position(|i| i.ty & 0x2000 != 0).unwrap_or(first_render);
                let m = &p.matrix;
                let matrix = if m[0] == 1.0 && m[1] == 0.0 && m[2] == 0.0 && m[3] == 0.0 {
                    None
                } else {
                    let q = |k: usize| ((m[k] * 32767.0) as i32 as i16) as f32 / 32767.0;
                    let c = |k: usize| Vec3::new(q(k), q(k + 1), q(k + 2));
                    Some(Affine3A::from_cols(c(0).into(), c(3).into(), c(6).into(), c(9).into()))
                };
                let has = |t: u16| infos.iter().any(|i| i.ty == t);
                Ok(PrimBp {
                    name: p.name.clone(),
                    src_blend: BlendFactor::from_id(p.src_blend),
                    dst_blend: BlendFactor::from_id(p.dst_blend),
                    alpha_on: p.alpha_on,
                    matrix,
                    textures: p.textures.clone(),
                    has_flat: has(ty::FLAT),
                    has_heat_haze: has(ty::HEATHAZE),
                    infos,
                    first_movement,
                    first_render,
                    lod_start: (p.lod_start * 64.0) as i32 as u16,
                    lod_end: (p.lod_end * 64.0) as i32 as u16,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let [x, y, z, r] = s.bounding_sphere;
        Ok(Self {
            name: s.name.clone(),
            length: s.length,
            loop_interval_min: s.loop_interval_min,
            loop_interval_max: s.loop_length,
            cull_dist: (s.cull_dist * 256.0) as i32 as u16,
            play_mode: s.play_mode as u8,
            sphere: (r > 0.0).then(|| (Vec3::new(x, y, z), r)),
            prims,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(times: &[f32], vals: &[f32]) -> Interp {
        Interp {
            looped: false,
            times: times.iter().map(|t| (t * 256.0) as u16).collect(),
            tracks: vec![vals.to_vec()],
        }
    }

    #[test]
    fn quantisation_matches_the_loader() {
        assert_eq!(quantise(Kind::U256, 0.4), 0.3984375);
        assert_eq!(quantise(Kind::S1000, 0.96), 960.0 * 0.001); // stored 960, not 959
        assert_eq!(quantise(Kind::U256, 255.0), 255.0);
    }

    #[test]
    fn overheat_rates_at_health_450() {
        // WhiteSmoke RATE [0,60,0] at t=[0,.5,1]; BlackSmoke [0,0,60] at t=[0,.3,1].
        let white = curve(&[0.0, 0.5, 1.0], &[0.0, 60.0, 0.0]);
        let black = curve(&[0.0, 0.3, 1.0], &[0.0, 0.0, 60.0]);
        assert_eq!(white.val(0, 0.5), 60.0);
        assert!((black.val(0, 0.5) - 17.3333).abs() < 1e-3);
        assert_eq!(white.val(0, 1.0), 0.0);
    }

    #[test]
    fn explosion_large_first_frame_counts() {
        let dt = 1.0f32 / 30.0;
        let fire = curve(&[0.0, 0.1], &[1600.0, 0.0]);
        assert!((fire.integral(0, dt, dt) - 44.231).abs() < 0.01);
        let debris = curve(&[0.0, 0.0078125], &[2500.0, 0.0]);
        assert!((debris.integral(0, dt, dt) - 9.765625).abs() < 1e-4);
        // Frame 2 starts past the last key: dt * last value = 0.
        assert_eq!(debris.integral(0, 2.0 * dt, dt), 0.0);
    }
}
