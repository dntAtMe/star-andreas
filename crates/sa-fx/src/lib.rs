//! A port of GTA San Andreas' particle FX runtime, from the PC 1.0 executable:
//! `FxManager_c` (0xA9AE80), `FxSystem_c`, `FxEmitter_c`, `FxEmitterBP_c` and the
//! `FxInfo*_c` behaviours, driven by `models/effects.fxp`.
//!
//! Everything is in the game's Z-up space, in metres and seconds (`dt = ts * 0.02`).
//! The renderer gets plain quads (`render`); textures and blending are its business.
//!
//! Not ported: audio (DoFxAudio), SMOKE secondary particles, GROUNDCOLLIDE, and water
//! (FLOAT is a no-op, UNDERWATER kills). Heat-haze prims are drawn by
//! `render_heat_haze` for the post effect's mask.

pub mod bp;

use std::collections::HashMap;

use anyhow::Result;
use glam::{Affine3A, Vec3};

pub use bp::{BlendFactor, PrimBp, SystemBp};
use bp::ty;

/// Particle pool size (`FxManager_c::Init`).
pub const MAX_PARTICLES: usize = 1000;

/// The CRT `rand()` LCG (the original shares it with the whole game).
#[derive(Debug, Clone)]
pub struct Rand(u32);

impl Rand {
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        (self.0 >> 16) & 0x7FFF
    }
    /// `(rand()%10000)*1e-4` in [0, 1).
    fn u(&mut self) -> f32 {
        (self.next() % 10000) as f32 * 1e-4
    }
    /// `(rand()%10000)*0.0002 - 1` in [-1, 1).
    fn r(&mut self) -> f32 {
        (self.next() % 10000) as f32 * 0.0002 - 1.0
    }
}

/// `CMaths` sine table: `sin(i * 2pi / 256)`.
fn sin_table() -> [f32; 256] {
    std::array::from_fn(|i| (i as f32 * std::f32::consts::TAU / 256.0).sin())
}

/// Camera as the FX code sees it, in game space. `right` is RenderWare's camera
/// right, which points to screen-left.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub pos: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub at: Vec3,
    /// Side planes with outward normals: a sphere is outside if `n.c - d > r`.
    pub planes: [(Vec3, f32); 4],
}

impl Camera {
    /// `FxFrustumInfo_c::IsSphereVisible` (0x4AA030), side planes only.
    pub fn sphere_visible(&self, c: Vec3, r: f32) -> bool {
        self.planes.iter().all(|(n, d)| n.dot(c) - d <= r)
    }
}

/// Weather inputs: `CWeather::WindDir`, `CWeather::Wind`, `CWeather::Rain`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Env {
    pub wind_dir: Vec3,
    pub wind: f32,
    pub rain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SysId(pub u32);

/// `FxPrtMult_c` (0x4AB290): colour, size, spin and life multipliers.
#[derive(Debug, Clone, Copy)]
pub struct PrtMult {
    pub rgba: [f32; 4],
    pub size: f32,
    pub ang_change: f32,
    pub life: f32,
}

impl Default for PrtMult {
    /// 0x4AB270: all 1.0.
    fn default() -> Self {
        Self { rgba: [1.0; 4], size: 1.0, ang_change: 1.0, life: 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayStatus {
    Playing,
    Stopped,
    Paused,
}

/// `FxSystem_c`.
#[derive(Debug, Clone)]
pub struct System {
    bp: usize,
    /// Key of the parent matrix (resolved every update), with its last value.
    parent: Option<u64>,
    parent_mat: Option<Affine3A>,
    local: Affine3A,
    play: PlayStatus,
    /// 0 alive, 1 kill when finished, 2 killed, 3 awaiting destroy.
    kill: u8,
    use_const: bool,
    curr_time: f32,
    cam_dist: f32,
    const_time: u16,
    rate_mult: u16,
    time_mult: u16,
    local_particles: bool,
    in_loop_interval: bool,
    was_culled: bool,
    loop_interval: f32,
    vel_add: Vec3,
    /// FxEmitter_c per prim: (enabled, accumulator).
    emitters: Vec<(bool, f32)>,
}

impl System {
    /// `GetCompositeMatrix`: local, then parent.
    fn world(&self) -> Affine3A {
        match self.parent_mat {
            Some(p) => p * self.local,
            None => self.local,
        }
    }
}

/// `FxEmitterPrt_c`.
#[derive(Debug, Clone)]
pub struct Particle {
    sys: SysId,
    lifetime: f32,
    age: f32,
    pos: Vec3,
    vel: Vec3,
    colour_mult: [u8; 4],
    size_mult: u8,
    spin_mult: u8,
    rnd: [u8; 3],
    brightness: u8,
    forced_rot: u8,
    local: bool,
    rotation: f32,
}

/// Emission parameters (`ProcessEmissionInfo` 0x4A4960 defaults).
#[derive(Debug, Clone, Copy)]
struct Emission {
    count: f32,
    radius: f32,
    min: Vec3,
    max: Vec3,
    speed: f32,
    speed_bias: f32,
    dir: Vec3,
    angle_min: f32,
    angle_max: f32,
    life: f32,
    life_bias: f32,
    pos: Vec3,
    rot_min: f32,
    rot_max: f32,
    wind_min: f32,
    wind_max: f32,
    rain_min: f32,
    rain_max: f32,
}

fn process_emission(p: &PrimBp, time: f32, dt: f32, len: f32, use_const: bool) -> Emission {
    let mut e = Emission {
        count: dt * 10.0,
        radius: 0.0,
        min: Vec3::ZERO,
        max: Vec3::ZERO,
        speed: 0.0,
        speed_bias: 0.0,
        dir: Vec3::Z,
        angle_min: 0.0,
        angle_max: 0.0,
        life: 1.0,
        life_bias: 0.0,
        pos: Vec3::ZERO,
        rot_min: 0.0,
        rot_max: 0.0,
        wind_min: 0.0,
        wind_max: 2.0,
        rain_min: 0.0,
        rain_max: 2.0,
    };
    for i in &p.infos[..p.first_movement] {
        if (i.ty >> 8) & 0x10 == 0 {
            continue;
        }
        let v = |k| i.val(k, time);
        match i.ty {
            ty::EMRATE => {
                let c = &i.interp;
                e.count = if use_const {
                    c.val(0, time) * dt
                } else if time - dt < 0.0 {
                    c.integral(0, len, dt - time) + c.integral(0, time, time)
                } else {
                    c.integral(0, time, dt)
                };
            }
            ty::EMSIZE => {
                e.radius = v(0);
                e.min = Vec3::new(v(1), v(3), v(5));
                e.max = Vec3::new(v(2), v(4), v(6));
            }
            ty::EMSPEED => (e.speed, e.speed_bias) = (v(0), v(1)),
            ty::EMDIR => e.dir = Vec3::new(v(0), v(1), v(2)),
            ty::EMANGLE => (e.angle_min, e.angle_max) = (v(0), v(1)),
            ty::EMLIFE => (e.life, e.life_bias) = (v(0), v(1)),
            ty::EMPOS => e.pos = Vec3::new(v(0), v(1), v(2)),
            ty::EMWEATHER => (e.wind_min, e.wind_max, e.rain_min, e.rain_max) = (v(0), v(1), v(2), v(3)),
            ty::EMROTATION => (e.rot_min, e.rot_max) = (v(0), v(1)),
            _ => {}
        }
    }
    e
}

/// Render parameters (`ProcessRenderInfo` 0x4A4A80 defaults).
#[derive(Debug, Clone, Copy)]
struct RenderInfo {
    /// r, g, b, a, then rmax/gmax/bmax (COLOURRANGE) or bias (COLOURBRIGHT, [7]).
    c: [u8; 8],
    colour_mode: u8,
    size: [f32; 2],
    size_bias: [f32; 2],
    rect: [f32; 4],
    anim_tex: bool,
    tex_id: i32,
    self_lit: bool,
    flat: Option<(Vec3, Vec3, Vec3)>,
    dir: Option<Vec3>,
    /// TRAIL: (trail time, screen-space mode 2).
    trail: Option<(f32, bool)>,
}

fn process_render(p: &PrimBp, time: f32, ratio: f32, len: f32) -> RenderInfo {
    let mut r = RenderInfo {
        c: [255, 255, 255, 255, 0, 0, 0, 0],
        colour_mode: 0,
        size: [0.1, 0.1],
        size_bias: [0.0, 0.0],
        rect: [0.5, -0.5, 0.5, -0.5],
        anim_tex: false,
        tex_id: 0,
        self_lit: false,
        flat: None,
        dir: None,
        trail: None,
    };
    for i in &p.infos[p.first_render..] {
        let tm = if i.prt { ratio } else { time / len };
        let v = |k| i.val(k, tm);
        let b = |k| v(k) as i32 as u8;
        match i.ty {
            ty::COLOUR => r.c[..4].copy_from_slice(&[b(0), b(1), b(2), b(3)]),
            ty::COLOURBRIGHT => {
                r.c[..4].copy_from_slice(&[b(0), b(1), b(2), b(3)]);
                r.c[7] = b(4);
                r.colour_mode = 2;
            }
            ty::COLOURRANGE => {
                r.c[0] = b(0);
                r.c[4] = b(1);
                r.c[1] = b(2);
                r.c[5] = b(3);
                r.c[2] = b(4);
                r.c[6] = b(5);
                r.c[3] = b(6);
                r.colour_mode = 1;
            }
            ty::SIZE => {
                r.size = [v(0), v(1)];
                r.size_bias = [v(2), v(3)];
            }
            ty::SPRITERECT => r.rect = [v(0), v(1), v(2), v(3)],
            ty::FLAT => {
                r.flat = Some((Vec3::new(v(0), v(1), v(2)), Vec3::new(v(3), v(4), v(5)), Vec3::new(v(6), v(7), v(8))))
            }
            ty::DIR => r.dir = Some(Vec3::new(v(0), v(1), v(2))),
            ty::ANIMTEX => {
                r.anim_tex = true;
                r.tex_id = v(0) as i32;
            }
            ty::SELFLIT => r.self_lit = true,
            ty::TRAIL => r.trail = Some((v(0), v(1) > 0.1)),
            _ => {}
        }
    }
    r
}

/// Per-system values a particle update needs.
#[derive(Clone, Copy)]
struct SysView {
    time_mult: u16,
    curr_time: f32,
    length: f32,
}

/// `FxEmitterBP_c::UpdateParticle` (0x4A21D0). Returns true when the particle dies.
fn update_particle(p: &PrimBp, s: SysView, dt: f32, prt: &mut Particle, env: &Env, rng: &mut Rand) -> bool {
    let dts = s.time_mult as f32 / 1000.0 * dt;
    prt.age += dts;
    if !(prt.age < prt.lifetime) {
        return true;
    }
    prt.pos += prt.vel * dts;
    let ratio = prt.age / prt.lifetime;
    let (mut min_cw, mut max_cw, mut min_ccw, mut max_ccw) = (0.0, 0.0, 0.0, 0.0);
    let mut underwater = false;
    for i in &p.infos[p.first_movement..p.first_render] {
        let tm = if i.prt { ratio } else { s.curr_time / s.length };
        let v = |k| i.val(k, tm);
        match i.ty {
            ty::FORCE => prt.vel += Vec3::new(v(0), v(1), v(2)) * dts,
            ty::FRICTION => prt.vel *= v(0).powf(dts * 50.0),
            ty::WIND => prt.vel += env.wind_dir * env.wind * v(0) * dts,
            ty::NOISE => {
                let n = Vec3::new(rng.r(), rng.r(), rng.r()).normalize_or_zero() * v(0) * dts;
                let l = prt.vel.length();
                prt.vel += n;
                if l > 0.0 {
                    prt.vel = prt.vel.normalize_or_zero() * l;
                }
            }
            ty::JITTER => {
                prt.pos += Vec3::new(rng.r(), rng.r(), rng.r()).normalize_or_zero() * v(0) * dts;
            }
            ty::ATTRACTPT | ty::ATTRACTLINE => {
                let (target, force) = if i.ty == ty::ATTRACTPT {
                    (Vec3::new(v(0), v(1), v(2)), v(3))
                } else {
                    let (a, b) = (Vec3::new(v(0), v(1), v(2)), Vec3::new(v(3), v(4), v(5)));
                    let ab = b - a;
                    let t = if ab.length_squared() > 0.0 { ((prt.pos - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                    (a + ab * t, v(6))
                };
                let d = target - prt.pos;
                let l = d.length();
                if l > 0.1 {
                    prt.vel += d / l * force * dts;
                }
            }
            ty::ROTSPEED => (min_cw, max_cw, min_ccw, max_ccw) = (v(0), v(1), v(2), v(3)),
            ty::UNDERWATER => underwater = true,
            _ => {}
        }
    }
    // No water anywhere yet: GetWaterLevel fails, so UNDERWATER particles die.
    if underwater {
        return true;
    }
    let b = prt.rnd[0] as f32;
    let k = prt.spin_mult as f32 * dts / 255.0;
    let cw = min_cw > 0.0 || max_cw > 0.0;
    let ccw = min_ccw > 0.0 || max_ccw > 0.0;
    if cw && ccw {
        if b < 128.0 {
            prt.rotation += (min_cw + (max_cw - min_cw) * b / 128.0) * k;
        } else {
            prt.rotation -= (min_ccw + (max_ccw - min_ccw) * (b - 128.0) / 128.0) * k;
        }
    } else if cw {
        prt.rotation += (min_cw + (max_cw - min_cw) * b / 255.0) * k;
    } else if ccw {
        prt.rotation -= (min_ccw + (max_ccw - min_ccw) * b / 255.0) * k;
    }
    false
}

/// One textured, blended run of particle quads in draw order.
#[derive(Debug, Clone)]
pub struct Batch {
    pub texture: String,
    pub src: BlendFactor,
    pub dst: BlendFactor,
    pub alpha_on: bool,
    pub verts: Vec<Vertex>,
}

#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub pos: Vec3,
    pub uv: [f32; 2],
    pub rgba: [u8; 4],
}

pub struct FxManager {
    pub bps: Vec<SystemBp>,
    by_name: HashMap<String, usize>,
    systems: HashMap<u32, System>,
    /// Systems list (head insertion: newest first).
    order: Vec<u32>,
    next_id: u32,
    /// Live particles per [bp][prim], oldest first (iterate reversed = newest first).
    particles: Vec<Vec<Vec<Particle>>>,
    live: usize,
    pub rng: Rand,
    sin: [f32; 256],
    /// Camera of the last update (CreateFxSystem's frustum test).
    camera: Option<Camera>,
    /// FX quality (`g_fx+0x54`): 0..3.
    pub quality: u8,
    /// Set by render when a heat-haze prim has particles.
    pub heat_haze_needed: bool,
    /// dt of the last update (screen-space trails use `ts * 0.02`).
    last_dt: f32,
}

impl FxManager {
    pub fn new(project: &sa_formats::fxp::Project) -> Result<Self> {
        let bps = project.systems.iter().map(SystemBp::from_fxp).collect::<Result<Vec<_>>>()?;
        let mut by_name = HashMap::new();
        // FindFxSystemBP walks the list head-first (reverse file order): the last
        // definition of a name wins.
        for (i, b) in bps.iter().enumerate() {
            by_name.insert(b.name.to_ascii_uppercase(), i);
        }
        let particles = bps.iter().map(|b| vec![Vec::new(); b.prims.len()]).collect();
        Ok(Self {
            bps,
            by_name,
            systems: HashMap::new(),
            order: Vec::new(),
            next_id: 0,
            particles,
            live: 0,
            rng: Rand(1),
            sin: sin_table(),
            camera: None,
            quality: 3,
            heat_haze_needed: false,
            last_dt: 0.0,
        })
    }

    pub fn live_particles(&self) -> usize {
        self.live
    }

    pub fn system_count(&self) -> usize {
        self.systems.len()
    }

    fn sin(&self, x: f32) -> f32 {
        self.sin[((x * 40.743_664) as i32 & 0xFF) as usize]
    }

    fn cos(&self, x: f32) -> f32 {
        self.sin[((x * 40.743_664 + 64.0) as i32 & 0xFF) as usize]
    }

    /// `FxManager_c::CreateFxSystem(name, pos, parent, ignoreBB)` (0x4A9BE0). `parent` is a
    /// key the caller resolves in `update`; `parent_mat` its current value. Returns None
    /// for unknown names, or when the bounding sphere is off-screen (the caller retries).
    pub fn create(&mut self, name: &str, pos: Vec3, parent: Option<(u64, Affine3A)>, ignore_bb: bool) -> Option<SysId> {
        let &bi = self.by_name.get(&name.to_ascii_uppercase())?;
        let bp = &self.bps[bi];
        let local = Affine3A::from_translation(pos);
        if !ignore_bb {
            if let (Some((c, r)), Some(cam)) = (bp.sphere, self.camera) {
                let w = match parent {
                    Some((_, p)) => p * local,
                    None => local,
                };
                if !cam.sphere_visible(w.transform_point3(c), r) {
                    return None;
                }
            }
        }
        self.next_id += 1;
        let id = self.next_id;
        let sys = System {
            bp: bi,
            parent: parent.map(|p| p.0),
            parent_mat: parent.map(|p| p.1),
            local,
            play: PlayStatus::Stopped,
            kill: 0,
            use_const: false,
            curr_time: 0.0,
            cam_dist: 0.0,
            const_time: 0,
            rate_mult: 1000,
            time_mult: 1000,
            local_particles: false,
            in_loop_interval: false,
            was_culled: false,
            loop_interval: 0.0,
            vel_add: Vec3::ZERO,
            emitters: vec![(true, 0.0); bp.prims.len()],
        };
        self.systems.insert(id, sys);
        self.order.insert(0, id);
        let q = self.quality;
        self.set_rate_mult(SysId(id), match q {
            0 => 0.5,
            1 => 0.75,
            _ => 1.0,
        });
        Some(SysId(id))
    }

    fn sys(&mut self, id: SysId) -> Option<&mut System> {
        self.systems.get_mut(&id.0)
    }

    /// `Play` (0x4AA2F0).
    pub fn play(&mut self, id: SysId) {
        let r = self.rng.next();
        let Some(s) = self.systems.get_mut(&id.0) else { return };
        let (min, max) = (self.bps[s.bp].loop_interval_min, self.bps[s.bp].loop_interval_max);
        if s.play != PlayStatus::Paused {
            s.curr_time = 0.0;
            for e in &mut s.emitters {
                e.1 = 0.0;
            }
        }
        s.kill = 0;
        s.play = PlayStatus::Playing;
        s.loop_interval = min + (max - min) * (r % 10000) as f32 * 1e-4;
        s.in_loop_interval = false;
        s.was_culled = false;
    }

    /// `Stop` (0x4AA390): live particles keep living.
    pub fn stop(&mut self, id: SysId) {
        if let Some(s) = self.sys(id) {
            s.play = PlayStatus::Stopped;
            s.curr_time = 0.0;
            for e in &mut s.emitters {
                e.1 = 0.0;
            }
        }
    }

    /// `Pause` (0x4AA370): toggles; stopped systems ignore it.
    pub fn pause(&mut self, id: SysId) {
        if let Some(s) = self.sys(id) {
            s.play = match s.play {
                PlayStatus::Stopped => PlayStatus::Stopped,
                PlayStatus::Paused => PlayStatus::Playing,
                PlayStatus::Playing => PlayStatus::Paused,
            };
        }
    }

    /// `PlayAndKill` (0x4AA3D0): only PLAYMODE 0 systems are freed when finished.
    pub fn play_and_kill(&mut self, id: SysId) {
        self.play(id);
        if let Some(s) = self.systems.get_mut(&id.0) {
            if self.bps[s.bp].play_mode == 0 {
                s.kill = 1;
            }
        }
    }

    /// `Kill` (0x4AA3F0): stop, then free once the last particle has died.
    pub fn kill(&mut self, id: SysId) {
        self.stop(id);
        if let Some(s) = self.sys(id) {
            s.kill = 2;
        }
    }

    /// `SetConstTime` (0x4AA6C0).
    pub fn set_const_time(&mut self, id: SysId, on: bool, t: f32) {
        if let Some(s) = self.sys(id) {
            s.use_const = on;
            s.const_time = (t * 256.0) as i32 as u16;
        }
    }

    pub fn set_rate_mult(&mut self, id: SysId, m: f32) {
        if let Some(s) = self.sys(id) {
            s.rate_mult = (m * 1000.0) as i32 as u16;
        }
    }

    pub fn set_time_mult(&mut self, id: SysId, m: f32) {
        if let Some(s) = self.sys(id) {
            s.time_mult = (m * 1000.0) as i32 as u16;
        }
    }

    /// `SetVelAdd` (0x4AA730): added to every new particle's velocity (m/s).
    pub fn set_vel_add(&mut self, id: SysId, v: Vec3) {
        if let Some(s) = self.sys(id) {
            s.vel_add = v;
        }
    }

    /// `SetOffsetPos` (0x4AA660).
    pub fn set_offset_pos(&mut self, id: SysId, p: Vec3) {
        if let Some(s) = self.sys(id) {
            s.local.translation = p.into();
        }
    }

    pub fn set_local_particles(&mut self, id: SysId, on: bool) {
        if let Some(s) = self.sys(id) {
            s.local_particles = on;
        }
    }

    /// `FxSystem_c::EnablePrim` (0x4AA610).
    pub fn enable_prim(&mut self, id: SysId, prim: usize, on: bool) {
        if let Some(e) = self.sys(id).and_then(|s| s.emitters.get_mut(prim)) {
            e.0 = on;
        }
    }

    /// `FxSystem_c::AddParticle` (0x4AA440) → `FxEmitter_c::AddParticle(pos)` (0x4A3EA0) on
    /// every enabled prim: emission infos at time 0, `M = parent · translate(pos) · prim`,
    /// explicit velocity, multipliers from `mult`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_particle(
        &mut self,
        id: SysId,
        pos: Vec3,
        vel: Vec3,
        time_since: f32,
        mult: &PrtMult,
        z_rot: f32,
        light_mult: f32,
        light_mult_limit: f32,
        local: bool,
        env: &Env,
    ) {
        let q = ((self.rng.next() & 0xFFFF) as f32 * 3.051_757_8e-5 * 100.0) as i32;
        if (self.quality == 0 && q < 50) || (self.quality == 1 && q < 25) {
            return;
        }
        let brightness = if light_mult < light_mult_limit { (1.0 - light_mult_limit) + light_mult } else { 1.0 };
        let Some(s) = self.systems.get(&id.0) else { return };
        let bi = s.bp;
        let parent = s.parent_mat.unwrap_or(Affine3A::IDENTITY);
        let use_const = s.use_const;
        let enabled: Vec<bool> = s.emitters.iter().map(|e| e.0).collect();
        for (pi, on) in enabled.into_iter().enumerate() {
            if !on {
                continue;
            }
            let prim = &self.bps[bi].prims[pi];
            let em = process_emission(prim, 0.0, 0.0, self.bps[bi].length, use_const);
            let m = parent * Affine3A::from_translation(pos) * prim.matrix.unwrap_or(Affine3A::IDENTITY);
            let created =
                self.create_particle(id.0, bi, pi, &em, m, time_since, brightness, local, env, Some(vel), mult);
            if created && z_rot >= 0.0 {
                if let Some(p) = self.particles[bi][pi].last_mut() {
                    p.forced_rot = (z_rot * 0.5) as i32 as u8;
                }
            }
        }
    }

    pub fn is_alive(&self, id: SysId) -> bool {
        self.systems.contains_key(&id.0)
    }

    /// `DestroyFxSystem` (0x4A9810): drops the system and all its particles now.
    fn destroy(&mut self, id: u32) {
        if let Some(s) = self.systems.remove(&id) {
            for list in &mut self.particles[s.bp] {
                let before = list.len();
                list.retain(|p| p.sys.0 != id);
                self.live -= before - list.len();
            }
        }
        self.order.retain(|&x| x != id);
    }

    /// `FxManager_c::Update` (0x4A9A80). `dt` in seconds (= ts * 0.02). `parents`
    /// resolves the parent keys given to `create` (None keeps the last matrix).
    pub fn update(&mut self, cam: &Camera, dt: f32, env: &Env, parents: impl Fn(u64) -> Option<Affine3A>) {
        self.camera = Some(*cam);
        self.last_dt = dt;
        for s in self.systems.values_mut() {
            if let Some(m) = s.parent.and_then(&parents) {
                s.parent_mat = Some(m);
            }
        }
        // 1. All particles of every prim (BPs in reverse file order).
        for bi in (0..self.bps.len()).rev() {
            for pi in 0..self.bps[bi].prims.len() {
                let mut list = std::mem::take(&mut self.particles[bi][pi]);
                let mut k = list.len();
                while k > 0 {
                    k -= 1;
                    let sid = list[k].sys.0;
                    let Some(s) = self.systems.get_mut(&sid) else { continue };
                    if s.kill == 3 {
                        s.kill = 2;
                    }
                    if s.play == PlayStatus::Paused {
                        continue;
                    }
                    let view = SysView { time_mult: s.time_mult, curr_time: s.curr_time, length: self.bps[bi].length };
                    if update_particle(&self.bps[bi].prims[pi], view, dt, &mut list[k], env, &mut self.rng) {
                        list.remove(k);
                        self.live -= 1;
                    }
                }
                self.particles[bi][pi] = list;
            }
        }
        // 2. Every system (newest first); destroy finished ones.
        for id in self.order.clone() {
            if self.update_system(id, cam, dt, env) {
                self.destroy(id);
            }
        }
    }

    /// `FxSystem_c::Update` (0x4AAF70). Returns true when the system should be destroyed.
    fn update_system(&mut self, id: u32, cam: &Camera, dt: f32, env: &Env) -> bool {
        let Some(s) = self.systems.get_mut(&id) else { return false };
        if s.kill == 3 {
            return true;
        }
        if s.kill == 2 {
            s.kill = 3;
            return false;
        }
        let bp = &self.bps[s.bp];
        let w = s.world();
        let prev = s.cam_dist;
        s.cam_dist = (cam.pos - Vec3::from(w.translation)).length();
        let cull = bp.cull_dist as f32 / 256.0;
        let visible = match bp.sphere {
            Some((c, r)) => cam.sphere_visible(w.transform_point3(c), r),
            None => true,
        };
        let culled = bp.play_mode != 0 && !(s.cam_dist < cull && visible);
        let mut emit: Option<f32> = None;
        if !culled && s.play == PlayStatus::Playing {
            let mut dts = s.time_mult as f32 / 1000.0 * dt;
            s.curr_time = if s.use_const { s.const_time as f32 / 256.0 } else { s.curr_time + dts };
            let mut stop = false;
            if bp.play_mode == 2 && bp.loop_interval_min > 0.0 {
                if s.curr_time > bp.length {
                    s.in_loop_interval = true;
                }
                if s.curr_time > bp.length + s.loop_interval {
                    s.curr_time -= bp.length + s.loop_interval;
                    let r = self.rng.next();
                    s.loop_interval = bp.loop_interval_min
                        + (bp.loop_interval_max - bp.loop_interval_min) * (r % 10000) as f32 * 1e-4;
                    s.in_loop_interval = false;
                }
            } else if s.curr_time > bp.length {
                match bp.play_mode {
                    0 | 3 => stop = true,
                    1 => s.curr_time = bp.length,
                    _ => s.curr_time -= bp.length,
                }
            }
            if stop {
                let kill_after = bp.play_mode == 0 && s.kill == 1;
                self.stop(SysId(id));
                if kill_after {
                    self.systems.get_mut(&id).unwrap().kill = 2;
                }
            }
            let s = self.systems.get_mut(&id).unwrap();
            if s.was_culled {
                dts += 0.25;
            }
            emit = Some(dts);
        } else if s.play == PlayStatus::Stopped && s.cam_dist < cull && prev >= cull && bp.play_mode == 3 {
            self.play(SysId(id));
        }
        if let Some(s) = self.systems.get_mut(&id) {
            s.was_culled = culled;
        }
        if let Some(dts) = emit {
            for pi in 0..self.bps[self.systems[&id].bp].prims.len() {
                self.update_emitter(id, pi, dts, env);
            }
        }
        false
    }

    /// `FxEmitter_c::Update` (0x4A41E0).
    fn update_emitter(&mut self, id: u32, pi: usize, dt: f32, env: &Env) {
        let s = &self.systems[&id];
        let (enabled, _) = s.emitters[pi];
        if !enabled || s.in_loop_interval {
            return;
        }
        let bi = s.bp;
        let bp = &self.bps[bi];
        let prim = &bp.prims[pi];
        let em = process_emission(prim, s.curr_time, dt, bp.length, s.use_const);
        let (ls, le, d) = (prim.lod_start as f32 / 64.0, prim.lod_end as f32 / 64.0, s.cam_dist);
        let mut lod = if d < ls {
            1.0
        } else if d > le {
            0.0
        } else {
            1.0 - (d - ls) / (le - ls)
        };
        if lod.is_nan() {
            lod = 0.0; // ls == le == d in the original yields NaN; keep the accumulator sane
        }
        let rate_mult = s.rate_mult as f32 / 1000.0;
        let s = self.systems.get_mut(&id).unwrap();
        s.emitters[pi].1 += em.count * lod * rate_mult;
        let accum = s.emitters[pi].1;
        if !(em.wind_min <= env.wind && env.wind <= em.wind_max && em.rain_min <= env.rain && env.rain <= em.rain_max) {
            return;
        }
        if accum < 1.0 {
            return;
        }
        let w = s.world();
        let m = match prim.matrix {
            Some(pm) => w * pm,
            None => w,
        };
        let local = s.local_particles;
        let n = accum as i32;
        for i in 0..n {
            let since = (i as f32 / accum) * dt;
            self.create_particle(id, bi, pi, &em, m, since, 1.2, local, env, None, &PrtMult::default());
        }
        let s = self.systems.get_mut(&id).unwrap();
        s.emitters[pi].1 -= (s.emitters[pi].1 as i32) as f32;
    }

    /// `FxEmitter_c::CreateParticle` (0x4A2580), emitter path (no explicit velocity, mult 1.0).
    #[allow(clippy::too_many_arguments)]
    fn create_particle(
        &mut self,
        id: u32,
        bi: usize,
        pi: usize,
        em: &Emission,
        m: Affine3A,
        since: f32,
        brightness: f32,
        local: bool,
        env: &Env,
        vel_in: Option<Vec3>,
        mult: &PrtMult,
    ) -> bool {
        if self.live >= MAX_PARTICLES {
            return false; // no SetMustCreatePrts users here
        }
        let r = &mut self.rng;
        let lifetime = (r.r() * em.life_bias + em.life) * mult.life;
        let rnd = [0, 1, 2].map(|_| (r.u() * 255.0) as i32 as u8);
        let rotation = em.rot_min + (em.rot_max - em.rot_min) * r.u();
        let prim = &self.bps[bi].prims[pi];
        let m = if local { prim.matrix.unwrap_or(Affine3A::IDENTITY) } else { m };
        let o = if em.radius.abs() < 0.001 {
            Vec3::new(
                em.min.x + (em.max.x - em.min.x) * r.u(),
                em.min.y + (em.max.y - em.min.y) * r.u(),
                em.min.z + (em.max.z - em.min.z) * r.u(),
            )
        } else {
            let v = Vec3::new(r.r(), r.r(), r.r());
            let mut k = 1.0 / v.length();
            if em.radius >= 0.0 {
                k *= r.u();
            }
            v * k * em.radius
        } + em.pos;
        let pos = m.transform_point3(o);
        let vel = match vel_in {
            // An explicit velocity is copied as is: no cone, no rand().
            Some(v) => v,
            None => {
                let theta = r.u() * 6.283_180_2;
                let phi_min = em.angle_min * 0.017_453_279;
                let phi = phi_min + (em.angle_max * 0.017_453_279 - phi_min) * r.u();
                let speed_r = r.r();
                let local_dir =
                    Vec3::new(self.cos(theta) * self.sin(phi), self.cos(phi), self.sin(theta) * self.sin(phi));
                let axis = if em.dir.x > 10.0 { pos } else { em.dir.normalize_or_zero() };
                let axis = m.transform_vector3(axis);
                align_to_axis(local_dir, axis) * (speed_r * em.speed_bias + em.speed)
            }
        };
        let s = &self.systems[&id];
        let vel = vel + s.vel_add;
        let b = |x: f32| (x * 255.0) as i32 as u8;
        let mut p = Particle {
            sys: SysId(id),
            lifetime,
            age: 0.0,
            pos,
            vel,
            colour_mult: mult.rgba.map(b),
            size_mult: b(mult.size),
            spin_mult: b(mult.ang_change),
            rnd,
            brightness: (brightness * 100.0) as i32 as u8,
            forced_rot: 0xFF,
            local,
            rotation,
        };
        let view = SysView { time_mult: s.time_mult, curr_time: s.curr_time, length: self.bps[bi].length };
        update_particle(&self.bps[bi].prims[pi], view, since, &mut p, env, &mut self.rng);
        self.particles[bi][pi].push(p);
        self.live += 1;
        true
    }

    /// `FxManager_c::Render` (0x4A92A0), normal pass: quads in the original draw order.
    /// `brightness` = `(1 - DNBalance) * 0.6 + 0.4` (1.0 by day, 0.4 at night).
    pub fn render(&mut self, cam: &Camera, brightness: f32) -> Vec<Batch> {
        self.heat_haze_needed = false;
        let mut out = Vec::new();
        for bi in (0..self.bps.len()).rev() {
            for pi in 0..self.bps[bi].prims.len() {
                self.render_prim(bi, pi, cam, brightness, false, &mut out);
            }
        }
        out
    }

    /// `FxManager_c::Render(cam, heatHazePass = true)`: only HEATHAZE prims, as black quads
    /// with the render alpha (`RenderHeatHaze` 0x4A1940) for the heat-haze mask.
    pub fn render_heat_haze(&mut self, cam: &Camera) -> Vec<Batch> {
        let mut out = Vec::new();
        for bi in (0..self.bps.len()).rev() {
            for pi in 0..self.bps[bi].prims.len() {
                if self.bps[bi].prims[pi].has_heat_haze {
                    self.render_prim(bi, pi, cam, 1.0, true, &mut out);
                }
            }
        }
        out
    }

    /// `FxEmitterBP_c::Render` (0x4A2C40).
    fn render_prim(&mut self, bi: usize, pi: usize, cam: &Camera, mut brightness: f32, haze: bool, out: &mut Vec<Batch>) {
        let bp = &self.bps[bi];
        let prim = &bp.prims[pi];
        let list = &mut self.particles[bi][pi];
        let dt = self.last_dt;
        if prim.has_heat_haze && !haze {
            if !list.is_empty() {
                self.heat_haze_needed = true;
            }
            return;
        }
        if list.is_empty() {
            return;
        }
        let tex0 = prim.textures[0].clone().unwrap_or_default();
        let new_batch = |texture: String| Batch {
            texture,
            src: if prim.alpha_on { prim.src_blend } else { BlendFactor::One },
            dst: if prim.alpha_on { prim.dst_blend } else { BlendFactor::Zero },
            alpha_on: prim.alpha_on,
            verts: Vec::new(),
        };
        let mut batch = new_batch(tex0.clone());
        let mut tex = tex0.clone();
        for p in list.iter_mut().rev() {
            let Some(s) = self.systems.get(&p.sys.0) else { continue };
            let pos = if p.local { s.world().transform_point3(p.pos) } else { p.pos };
            let mut ri = process_render(prim, s.curr_time, p.age / p.lifetime, bp.length);
            // Orientation.
            let (b_right, b_up, axis) = if let Some((time, screen)) = ri.trail {
                // TRAIL (sparks.md 4.1). Mode 2's camera vector (TheCamera+0x9EC) is taken as camPos.
                let trail = if screen { p.vel * dt * time } else { p.vel * time };
                let d = if trail == Vec3::ZERO { Vec3::Z } else { trail.normalize() };
                let v = (pos - cam.pos).normalize_or_zero();
                let right = d.cross(v);
                ri.rect[0] = trail.length();
                ri.rect[1] = 0.0;
                (right, d, d.cross(right))
            } else if let Some(dv) = ri.dir {
                let mut d = if dv.length() < 0.001 { p.vel } else { dv };
                if d == Vec3::ZERO {
                    d = Vec3::Z;
                }
                let d = d.normalize();
                let v = (pos - cam.pos).normalize_or_zero();
                let right = d.cross(v);
                (right, d, d.cross(right))
            } else if let Some((r, u, a)) = ri.flat {
                (r, u, a)
            } else {
                (cam.right, cam.up, cam.at)
            };
            if p.forced_rot != 0xFF {
                p.rotation = p.forced_rot as f32 * 2.0;
            }
            p.rotation = p.rotation.rem_euclid(360.0);
            let (right, up) = if p.rotation > 0.0 {
                let r = rotate_about_axis(&self.sin, b_right, axis, p.rotation * 0.017_453_279);
                (r, axis.cross(r))
            } else {
                (b_right, b_up)
            };
            // Size.
            let mut sx = ri.size[0] + (p.rnd[0] as f32 / 255.0 - 0.5) * ri.size_bias[0];
            let mut sy = ri.size[1] + (p.rnd[1] as f32 / 255.0 - 0.5) * ri.size_bias[1];
            if p.size_mult < 255 {
                sx *= p.size_mult as f32 / 255.0;
                sy *= p.size_mult as f32 / 255.0;
            }
            // Colour.
            let mut c = [ri.c[0] as f32, ri.c[1] as f32, ri.c[2] as f32, ri.c[3] as f32];
            match ri.colour_mode {
                1 => {
                    for k in 0..3 {
                        c[k] += (ri.c[4 + k] as f32 - c[k]) * p.rnd[k] as f32 / 255.0;
                    }
                }
                2 => {
                    let k = (p.rnd[0] as f32 / 128.0 - 1.0) * ri.c[7] as f32;
                    for ch in &mut c[..3] {
                        *ch = (*ch + k).clamp(0.0, 255.0);
                    }
                }
                _ => {}
            }
            for k in 0..4 {
                if p.colour_mult[k] < 255 {
                    c[k] *= p.colour_mult[k] as f32 / 255.0;
                }
            }
            let rgb = if ri.self_lit {
                [c[0] as i32 as u8, c[1] as i32 as u8, c[2] as i32 as u8]
            } else {
                if p.brightness <= 100 {
                    brightness = p.brightness as f32 * 0.01; // sticks for the rest of the prim
                }
                [(c[0] * brightness) as i32 as u8, (c[1] * brightness) as i32 as u8, (c[2] * brightness) as i32 as u8]
            };
            let rgba = if haze { [0, 0, 0, ri.c[3]] } else { [rgb[0], rgb[1], rgb[2], c[3] as i32 as u8] };
            // Texture.
            if ri.anim_tex {
                let pick = |i: usize| prim.textures[i].clone().unwrap_or_else(|| tex0.clone());
                tex = match ri.tex_id {
                    1 => tex0.clone(),
                    2 => pick(1),
                    3 => pick(2),
                    4 => pick(3),
                    _ => tex.clone(),
                };
            } else {
                tex = tex0.clone();
            }
            if tex != batch.texture {
                let done = std::mem::replace(&mut batch, new_batch(tex.clone()));
                if !done.verts.is_empty() {
                    out.push(done);
                }
            }
            let rect = ri.rect;
            let v = |a: usize, b: usize| pos + right * (rect[a] * sx) + up * (rect[b] * sy);
            let quad = [
                (v(2, 0), [0.0, 0.0]),
                (v(3, 1), [1.0, 1.0]),
                (v(3, 0), [1.0, 0.0]),
                (v(3, 1), [1.0, 1.0]),
                (v(2, 0), [0.0, 0.0]),
                (v(2, 1), [0.0, 1.0]),
            ];
            batch.verts.extend(quad.iter().map(|&(pos, uv)| Vertex { pos, uv, rgba }));
        }
        if !batch.verts.is_empty() {
            out.push(batch);
        }
    }
}

/// `AlignToAxis` (0x4A1660): the cone frame around `a` (result scales with |a|).
#[allow(clippy::approx_constant)] // the binary's reference vector (0x85A77C)
fn align_to_axis(d: Vec3, a: Vec3) -> Vec3 {
    let reference = Vec3::new(0.4243, 0.5657, 0.7071);
    let n = a.cross(reference).normalize_or_zero();
    d.x * n + d.y * a + d.z * a.cross(n)
}

/// `RotateVecAboutAxis` (0x4A1780), sine-table Rodrigues.
fn rotate_about_axis(sin: &[f32; 256], v: Vec3, k: Vec3, angle: f32) -> Vec3 {
    let c = sin[((angle * 40.743_664 + 64.0) as i32 & 0xFF) as usize];
    let s = sin[((angle * 40.743_664) as i32 & 0xFF) as usize];
    v * c + k.cross(v) * s + k * k.dot(v) * (1.0 - c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> sa_formats::fxp::Project {
        let path = r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas\models\effects.fxp";
        sa_formats::fxp::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn camera(pos: Vec3) -> Camera {
        let far = (Vec3::ZERO, 1e9);
        Camera { pos, right: -Vec3::X, up: Vec3::Z, at: Vec3::Y, planes: [far; 4] }
    }

    #[test]
    #[ignore = "needs the game's effects.fxp"]
    fn overheat_car_emits_two_white_puffs_per_frame_at_health_450() {
        let mut fx = FxManager::new(&project()).unwrap();
        let cam = camera(Vec3::new(0.0, -5.0, 1.0));
        let env = Env::default();
        let id = fx.create("overheat_car", Vec3::ZERO, None, false).unwrap();
        fx.play(id);
        fx.set_const_time(id, true, 0.5);
        let dt = 1.0 / 30.0;
        fx.update(&cam, dt, &env, |_| None);
        let bi = fx.by_name["OVERHEAT_CAR"];
        assert_eq!(fx.particles[bi][0].len(), 2, "WhiteSmoke");
        assert_eq!(fx.particles[bi][1].len(), 0, "BlackSmoke 0.578 accumulated");
        fx.update(&cam, dt, &env, |_| None);
        assert_eq!(fx.particles[bi][1].len(), 1);
        let batches = fx.render(&cam, 1.0);
        assert!(batches.iter().all(|b| b.texture == "bullethitsmoke"));
    }

    #[test]
    #[ignore = "needs the game's effects.fxp"]
    fn explosion_large_first_frame_and_cleanup() {
        let mut fx = FxManager::new(&project()).unwrap();
        let cam = camera(Vec3::new(0.0, -30.0, 5.0));
        let env = Env::default();
        let id = fx.create("explosion_large", Vec3::ZERO, None, false).unwrap();
        fx.play_and_kill(id);
        fx.update(&cam, 1.0 / 30.0, &env, |_| None);
        let bi = fx.by_name["EXPLOSION_LARGE"];
        let counts: Vec<usize> = fx.particles[bi].iter().map(|l| l.len()).collect();
        assert_eq!(counts, [4, 9, 44], "smoke, debris, explosion");
        for _ in 0..30 * 6 {
            fx.update(&cam, 1.0 / 30.0, &env, |_| None);
        }
        assert!(!fx.is_alive(id));
        assert_eq!(fx.live_particles(), 0);
    }
}
