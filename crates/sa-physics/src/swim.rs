//! Swimming (swim.md): `CTaskComplexInWater` → `CTaskSimpleSwim` for the player.
//!
//! The task replaces PlayerOnFoot while it runs. States: 0 tread, 1 breaststroke, 2 crawl,
//! 3 dive under, 4 underwater, 5 jump out. Movement is the swim anims' root shift blended
//! into the move speed, with a surface hold; pitch and roll are render-only.
//!
//! Not ported: climbing out (`TestForClimb` / CTaskSimpleClimb: the jump-out falls back into
//! the water), NPC swimmers (ProcessControlAI), the 2-player branch, stats, audio, the gasp,
//! the swim fx and the camera tilt.

use std::f32::consts::{FRAC_PI_4, PI, TAU};

use glam::Vec3;

use crate::{
    Ctx,
    anim::{AnimManager, Clump, af, anim_id, group},
    pedtask::{PedCore, PedTasks},
};

/// Swim anims (group 71 `swim`, partial over the group-0 Swim_Tread).
const GROUP_SWIM: usize = 71;
const TREAD: i16 = 14;
const BREAST: i16 = 311;
const CRAWL: i16 = 312;
const DIVE_UNDER: i16 = 313;
const UNDER: i16 = 314;
const GLIDE: i16 = 315;
const JUMPOUT: i16 = 316;
const CLIMB_JUMP: i16 = 128;
const NONE: i16 = 0xBF;

/// `CStats::GetFatAndMuscleModifier(8)` at zero lung capacity / stamina.
pub const MAX_BREATH: f32 = 1000.0;

fn wrap_pi(mut a: f32) -> f32 {
    if a > PI {
        a -= TAU;
    } else if a < -PI {
        a += TAU;
    }
    a
}

/// `CTaskSimpleSwim` (0x64 bytes).
#[derive(Debug, Clone)]
pub struct SwimTask {
    finished_blending: bool,
    pub state: i16,
    anim: i16,
    /// Render-time rotations (ApplyRollAndPitch) and the Spine1 torso angles.
    pub pitch: f32,
    pub roll: f32,
    pub torso_bend: f32,
    pub torso_twist: f32,
    state_changer: f32,
    /// +0x54: fed by ProcessBuoyancy (1000 out of the water, += ts standing in shallow
    /// water, 0 while deep).
    pub stop_time: f32,
}

impl Default for SwimTask {
    fn default() -> Self {
        Self {
            finished_blending: false,
            state: 0,
            anim: NONE,
            pitch: 0.0,
            roll: 0.0,
            torso_bend: 0.0,
            torso_twist: 0.0,
            state_changer: 0.0,
            stop_time: 0.0,
        }
    }
}

/// The water query the task needs (`GetWaterLevel` with waves, touching).
pub struct WaterQuery<'a> {
    pub water: &'a crate::water::WaterLevel,
    pub wavyness: f32,
    pub now_ms: u32,
}

impl WaterQuery<'_> {
    fn level(&self, p: Vec3) -> Option<f32> {
        self.water.level(p.x, p.y, p.z, true, self.wavyness, self.now_ms).map(|l| l.0)
    }
}

impl SwimTask {
    /// The finish of ProcessPed (not climbing): back to idle, or walk / run from the strokes.
    fn finish(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager) {
        let mut id = anim_id::IDLE;
        t.move_state = 1;
        if self.anim != NONE {
            if let Some(a) = c.clump.get_mut(self.anim) {
                if a.id == CLIMB_JUMP {
                    a.flags |= af::FADE_OUT_FINISHED;
                } else {
                    a.blend_delta = -4.0;
                }
            }
            if self.anim == BREAST {
                id = anim_id::WALK;
                t.move_state = 4;
            } else if self.anim == CRAWL {
                id = anim_id::RUN;
                t.move_state = 6;
            }
        }
        c.clump.blend_animation(m, t.anim_group, id, 4.0);
        *c.turn_rate = 9.0; // RestoreHeadingRate (STAT_PLAYER)
    }

    /// `ProcessPed` (0x68B1C0). Returns true when finished.
    pub fn process_ped(&mut self, t: &mut PedTasks, c: &mut PedCore, ctx: &Ctx, m: &AnimManager, w: &WaterQuery) -> bool {
        if self.stop_time > 15.0 {
            self.finish(t, c, m);
            return true;
        }
        self.process_control_input(t, c, ctx, w);
        let mut under = false;
        let mut rate = 1.0;
        if self.state == 4 {
            under = true;
            if let Some(u) = c.clump.get(UNDER) {
                rate = u.speed * u.blend + 1.0;
            }
        }
        t.breath_request = Some((under, rate));
        t.move_state = 0;
        self.process_swim_anims(t, c, m, w);
        self.process_swimming_resurfacing(c, ctx, w);
        false
    }

    /// `ProcessControlInput` (0x688A90), mouse-camera path.
    fn process_control_input(&mut self, t: &mut PedTasks, c: &mut PedCore, ctx: &Ctx, w: &WaterQuery) {
        let ts = ctx.ts;
        let y = t.pad.walk_ud / 128.0;
        if !self.finished_blending {
            t.pd.mbr = 0.0;
            return;
        }
        if self.state < 2 {
            if t.pad.jump_just_down {
                self.state = 5;
            } else if t.pad.fire_just_down {
                self.state = 3;
                t.pd.mbr = 0.0;
            }
        }
        let f = ctx.cam.front;
        match self.state {
            0..=2 => {
                // m_bUseMouse3rdPerson: face the camera, forward stick only.
                *c.aim_rot = (-f.x).atan2(f.y);
                let target = -y;
                t.pd.mbr = if target - t.pd.mbr > ts * 0.07 { t.pd.mbr + ts * 0.07 } else { target };
                let turn = (wrap_pi(-(*c.aim_rot - *c.cur_rot)) * 10.0).clamp(-1.0, 1.0);
                self.torso_twist += ts * 0.08 * turn;
                if self.state == 2 {
                    self.roll += ts * 0.04 * turn;
                } else if self.state == 1 {
                    self.torso_bend += turn.abs() * ts * 0.04;
                }
                if self.state == 2 {
                    let p = c.p.matrix.pos;
                    let fwd = c.p.matrix.fwd;
                    if let (Some(lf), Some(lb)) = (w.level(p + fwd), w.level(p - fwd)) {
                        self.pitch = (lf - lb).atan2(2.0);
                    }
                }
                let s = t.control_button_sprint_row(ts, 1.0, 0.3, 0.3);
                self.state = if s >= 1.0 {
                    2
                } else if t.pd.mbr > 0.5 {
                    1
                } else {
                    0
                };
            }
            3 | 4 => {
                if self.state == 3 && self.state_changer > 0.0 {
                    self.state_changer = 0.0;
                }
                if self.state == 4 {
                    // Mouse: steer by the camera front (looking forward).
                    *c.aim_rot = (-f.x).atan2(f.y);
                    let turn = (wrap_pi(-(*c.aim_rot - *c.cur_rot)) * 10.0).clamp(-1.0, 1.0);
                    self.roll += ts * 0.04 * turn;
                    self.torso_twist += ts * 0.08 * turn;
                    let d = ((f.z.clamp(-1.0, 1.0).asin() - self.pitch) * 10.0).clamp(-1.0, 1.0);
                    if self.state_changer == 0.0 || d > 0.0 {
                        self.pitch += ts * 0.02 * d;
                    }
                    let r = (self.roll / 0.5).clamp(-1.0, 1.0);
                    let v = (r * -0.08 * turn + d).clamp(-1.0, 1.0);
                    self.torso_bend += ts * -0.08 * v;
                    self.pitch = (self.pitch + ts * 0.001).clamp(-1.396_263_4, 1.396_263_4);
                    if t.pd.time_can_run <= 0.0 {
                        t.pd.time_can_run = 0.1;
                    }
                    t.control_button_sprint_row(ts, 0.0, 0.0, 1.0);
                }
            }
            _ => {}
        }
        // Decay.
        let s = self.state;
        let k = 0.95f32.powf(ts);
        self.roll = if self.roll.abs() <= 0.01 { 0.0 } else { self.roll * k };
        if !matches!(s, 2 | 4) {
            self.pitch = if self.pitch.abs() <= 0.01 { 0.0 } else { self.pitch * k };
        }
        let k2 = if matches!(s, 3 | 5) { 0.95f32 } else { 0.92 }.powf(ts);
        if self.torso_twist.abs() <= 0.01 && self.torso_bend.abs() <= 0.01 {
            self.torso_twist = 0.0;
            self.torso_bend = 0.0;
        } else {
            self.torso_twist *= k2;
            self.torso_bend *= k2;
        }
        // HandleSprintEnergy(false, rate): stamina regeneration.
        if s == 1 {
            t.regen_sprint_energy(ts, 0.5);
        } else if s != 2 {
            t.regen_sprint_energy(ts, 1.0);
        }
    }

    fn blend(clump: &mut Clump, m: &AnimManager, g: usize, id: i16, delta: f32) -> Option<usize> {
        clump.blend_animation(m, g, id, delta)
    }

    /// `ProcessSwimAnims` (0x6899F0).
    fn process_swim_anims(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager, w: &WaterQuery) {
        let clump = &mut *c.clump;
        if !self.finished_blending {
            let i = clump.index_of(TREAD).or_else(|| Self::blend(clump, m, group::DEFAULT, TREAD, 8.0));
            if i.is_some_and(|i| clump.assocs[i].blend >= 1.0) {
                self.finished_blending = true;
            }
            // Fade the partial (weapon) anims.
            for a in &mut clump.assocs {
                if a.has(af::PARTIAL) {
                    a.blend_delta = -8.0;
                }
            }
            let p = c.p.matrix.pos;
            if self.state == 0 && w.level(p).is_some_and(|l| p.z < l - 2.0) {
                self.state = 4;
            }
        } else if clump.get(TREAD).is_some_and(|a| a.blend < 1.0 && a.blend_delta <= 0.0) {
            Self::blend(clump, m, group::DEFAULT, TREAD, 8.0);
        }
        if !self.finished_blending {
            return;
        }
        match self.state {
            0 => {
                if self.anim != TREAD {
                    for (id, d) in [(BREAST, -1.0), (CRAWL, -1.0), (DIVE_UNDER, -4.0), (UNDER, -2.0), (GLIDE, -2.0), (JUMPOUT, -4.0), (CLIMB_JUMP, -4.0)] {
                        if let Some(a) = clump.get_mut(id) {
                            a.blend_delta = d;
                            a.flags |= af::DELETE_BLENDED_OUT;
                        }
                    }
                    self.anim = TREAD;
                }
            }
            1 => {
                if self.anim != BREAST {
                    Self::blend(clump, m, GROUP_SWIM, BREAST, 2.0);
                    self.anim = BREAST;
                } else if let Some(a) = clump.get_mut(BREAST) {
                    a.speed = t.pd.mbr;
                } else {
                    self.state = 0;
                }
            }
            2 => {
                if self.anim != CRAWL {
                    Self::blend(clump, m, GROUP_SWIM, CRAWL, 2.0);
                    self.anim = CRAWL;
                } else if let Some(a) = clump.get_mut(CRAWL) {
                    a.speed = t.button_sprint_results_row(0.3).max(1.0);
                } else {
                    self.state = 0;
                }
            }
            3 => {
                if self.anim != DIVE_UNDER {
                    Self::blend(clump, m, GROUP_SWIM, DIVE_UNDER, 8.0);
                    self.anim = DIVE_UNDER;
                } else if let Some(a) = clump.get(DIVE_UNDER) {
                    if a.time == a.hier.total_length {
                        self.state = 4;
                    }
                } else {
                    self.state = 0;
                }
            }
            4 => {
                if !matches!(self.anim, UNDER | GLIDE) || self.state_changer < 0.0 {
                    match clump.get(UNDER).map(|a| a.blend) {
                        None => {
                            Self::blend(clump, m, GROUP_SWIM, UNDER, 1000.0);
                            self.state_changer = if matches!(self.anim, TREAD | NONE) { -2.0 } else { -1.0 };
                            self.anim = UNDER;
                        }
                        Some(b) if self.state_changer < 0.0 && b >= 0.99 => {
                            self.pitch = if self.state_changer > -2.0 { (-35.0f32).to_radians() } else { 1.396_263_4 };
                            self.state_changer = 0.0;
                        }
                        _ => {}
                    }
                } else if t.button_sprint_results_row(1.0) < 1.0 {
                    // Glide.
                    if clump.get(GLIDE).is_none_or(|g| g.blend_delta < 0.0 || g.blend == 0.0) {
                        Self::blend(clump, m, GROUP_SWIM, GLIDE, 4.0);
                    }
                    self.anim = GLIDE;
                } else {
                    // Stroke (sprint tapping).
                    let i = match clump.index_of(UNDER) {
                        Some(i) if !(clump.assocs[i].blend_delta < 0.0 || clump.assocs[i].blend == 0.0) => Some(i),
                        _ => Self::blend(clump, m, GROUP_SWIM, UNDER, 4.0),
                    };
                    if let Some(i) = i {
                        let a = &mut clump.assocs[i];
                        if a.time == a.hier.total_length {
                            a.start(0.0);
                            a.speed = t.button_sprint_results_row(1.0).max(0.7);
                        }
                    }
                    self.anim = UNDER;
                }
            }
            5 => {
                if self.anim == JUMPOUT {
                    match clump.get(JUMPOUT) {
                        None => self.state = 0,
                        Some(a) => {
                            if a.time + a.time_step() >= a.hier.total_length * 0.25 {
                                if let Some(i) = Self::blend(clump, m, group::DEFAULT, CLIMB_JUMP, 8.0) {
                                    clump.assocs[i].flags |= af::FADE_OUT_FINISHED;
                                }
                                self.anim = CLIMB_JUMP;
                            }
                        }
                    }
                } else if self.anim != CLIMB_JUMP {
                    Self::blend(clump, m, GROUP_SWIM, JUMPOUT, 8.0);
                    self.anim = JUMPOUT;
                    c.p.move_speed.z = 8.0 / c.p.mass;
                    // TestForClimb: not ported (no climb target).
                } else if clump.get(CLIMB_JUMP).is_none() {
                    self.state = 0;
                } else {
                    let p = c.p.matrix.pos;
                    if w.level(p).is_some_and(|l| l > p.z + 0.5) {
                        self.state = 0;
                    }
                }
            }
            _ => {}
        }
    }

    /// `ProcessSwimmingResurfacing` (0x68A1D0): velocity and the surface hold.
    fn process_swimming_resurfacing(&mut self, c: &mut PedCore, ctx: &Ctx, w: &WaterQuery) {
        let ts = ctx.ts;
        let clump = &*c.clump;
        let sh = clump.velocity; // raw root shift (x right, y forward)
        let mat = c.p.matrix;
        let mut vel = Vec3::ZERO;
        let mut off = -1.0f32;
        match self.state {
            0..=2 => {
                let b = clump.get(BREAST);
                let cr = clump.get(CRAWL);
                let mut wt = b.map_or(1.0, |b| 1.0 - b.blend);
                off = b.map_or(0.0, |b| 0.4 * b.blend);
                if let Some(cr) = cr {
                    off += 0.2 * cr.blend;
                    wt -= cr.blend;
                }
                off += wt.max(0.0) * 0.55;
                vel = mat.right * sh.x + mat.fwd * sh.y;
            }
            3 => {
                vel = mat.right * sh.x + mat.fwd * sh.y;
                if let Some(d) = clump.get(DIVE_UNDER) {
                    vel.z = d.time / d.hier.total_length * -0.1;
                }
            }
            4 => {
                let (s, co) = self.pitch.sin_cos();
                vel.x = mat.right.x * sh.x + mat.fwd.x * co * sh.y;
                vel.y = mat.right.y * sh.x + mat.fwd.y * co * sh.y;
                vel.z = mat.right.z * sh.x + mat.fwd.z * co * sh.y + s * sh.y + 0.01;
            }
            _ => {
                let a = clump.get(CLIMB_JUMP).or_else(|| clump.get(JUMPOUT));
                let Some(a) = a else { return };
                if a.time >= a.hier.total_length || (a.blend < 1.0 && a.blend_delta <= 0.0) {
                    return;
                }
                let mass = c.p.mass;
                c.p.apply_move_force(Vec3::new(0.0, 0.0, ts * mass * 0.3 * 0.008));
                return;
            }
        }
        let k = 0.9f32.powf(ts);
        let p = &mut *c.p;
        p.move_speed = p.move_speed * k + vel * (1.0 - k);
        let pos = p.matrix.pos;
        if let Some(l) = w.level(Vec3::new(pos.x + ts * p.move_speed.x, pos.y + ts * p.move_speed.y, pos.z)) {
            if self.state == 4 && self.state_changer >= 0.0 {
                if pos.z + 0.65 > l && self.pitch > FRAC_PI_4 {
                    self.state = 0;
                    self.state_changer = 0.0;
                } else if self.pitch >= 0.0 {
                    if pos.z + 0.65 > l {
                        let lim = 0.05 * 0.5;
                        if self.state_changer > lim {
                            self.state_changer *= 0.95;
                        }
                        if self.state_changer < lim {
                            self.state_changer = (self.state_changer + ts * 0.002).min(lim);
                        }
                        self.pitch += ts * self.state_changer;
                        off = self.pitch * 4.0 / PI * (0.55 - 0.2) * 0.75 + 0.2;
                    } else {
                        self.state_changer = if self.state_changer > 0.001 { self.state_changer * 0.95 } else { 0.0 };
                    }
                } else {
                    if pos.z - self.pitch.sin() + 0.65 > l {
                        self.state_changer = (self.state_changer + ts * 0.002).min(0.05);
                    } else if self.state_changer > 0.001 {
                        self.state_changer *= 0.95;
                    } else {
                        self.state_changer = 0.0;
                    }
                    self.pitch += ts * self.state_changer;
                }
            }
            if off > 0.0 {
                let dz = ((l - (off + pos.z)) / ts.max(1e-5)).clamp(-ts * 0.1, ts * 0.1);
                p.move_speed.z += (dz - p.move_speed.z).clamp(-ts * 0.02, ts * 0.02);
            }
        }
        if p.matrix.pos.z < -69.0 {
            p.matrix.pos.z = -69.0;
            p.move_speed.z = p.move_speed.z.max(0.0);
        }
    }

    /// `ApplyRollAndPitch` (0x68A8E0): the render rotation (local roll about Y, then pitch about
    /// X) applied to the ped frame.
    pub fn render_rotation(&self) -> glam::Quat {
        glam::Quat::from_rotation_y(self.roll) * glam::Quat::from_rotation_x(self.pitch)
    }
}

impl PedTasks {
    /// `ControlButtonSprint(type)` with that type's row: tap +4 (max 10), held −0.7·ts (min 1),
    /// released −0.2·ts; stamina use slow/fast (0 = none); result scale.
    pub(crate) fn control_button_sprint_row(&mut self, ts: f32, slow_rate: f32, fast_rate: f32, scale: f32) -> f32 {
        let pd = &mut self.pd;
        let can = pd.sprint_counter > 0.0 || pd.time_can_run > 0.0;
        if self.pad.sprint_just_down && can {
            pd.sprint_counter = (pd.sprint_counter + 4.0).min(10.0);
        } else if self.pad.sprint && can {
            pd.sprint_counter = (pd.sprint_counter - ts * 0.7).max(1.0);
        } else if pd.sprint_counter > 0.0 {
            pd.sprint_counter = (pd.sprint_counter - ts * 0.2).max(0.0);
        }
        let (r, rate) = if pd.sprint_counter > 5.0 {
            (pd.sprint_counter / 5.0, fast_rate)
        } else if pd.sprint_counter > 0.0 && can {
            (1.0, slow_rate)
        } else {
            return 0.0;
        };
        if rate != 0.0 {
            if pd.time_can_run <= -150.0 {
                pd.sprint_counter = 0.0;
                return 0.0;
            }
            pd.time_can_run = (pd.time_can_run - ts * rate).max(-150.0);
        }
        (r - 1.0f32).max(0.0) * scale + 1.0
    }

    /// `GetButtonSprintResults` with that row's scale.
    pub(crate) fn button_sprint_results_row(&self, scale: f32) -> f32 {
        let c = self.pd.sprint_counter;
        if c > 5.0 {
            (c / 5.0 - 1.0).max(0.0) * scale + 1.0
        } else if c > 0.0 {
            1.0
        } else {
            0.0
        }
    }

    /// `HandleSprintEnergy(false, rate)`: `T += ts·rate·0.5` up to the stamina maximum.
    pub(crate) fn regen_sprint_energy(&mut self, ts: f32, rate: f32) {
        let max = crate::pedtask::TIME_CAN_RUN_MAX;
        if self.pd.time_can_run < max {
            self.pd.time_can_run = (self.pd.time_can_run + ts * rate * 0.5).min(max);
        }
    }

    /// CEventInWater → CTaskComplexInWater: the player starts swimming; the on-foot tasks end.
    pub fn start_swimming(&mut self, clump: &mut Clump, m: &AnimManager) {
        if self.swim.is_some() {
            return;
        }
        self.abort_fight(clump, m);
        self.gun = None;
        self.throw = None;
        self.duck = None;
        self.ducking = false;
        self.air = crate::pedtask::AirTask::None;
        self.swim = Some(SwimTask::default());
    }
}
