//! `CBoat` (boat.md): control inputs, propeller, damage, the wake trail, splashes, and
//! `CVehicle::ProcessBoatControl` (0x6DBCE0) with `ProcessBuoyancyBoat` and
//! `ApplyBoatWaterResistance`.
//!
//! Not ported: boat AI (SIMPLE / PHYSICS statuses), anchoring, the far-away freeze of
//! abandoned boats, the marquis boom (CDoor), the flying radar after BlowUpCar, occupant
//! drowning when capsized, the fire FX, audio, mouse steering, the skimmer (model 460).

use glam::Vec3;

use crate::{
    Ctx,
    automobile::{CarInput, VehicleHandling},
    collision::ColModel,
    effects::{ExplosionType, FrameFx, PrtMult, WorldRequest},
    physical::{Physical, Status, ef, pf},
    water::{BoatBuoyancyIn, WaterLevel, process_buoyancy_boat},
    world::{BodyLogic, EntityId, LineHits},
};

/// `tBoatHandlingData` (no unit conversion).
#[derive(Debug, Clone)]
pub struct BoatHandling {
    pub thrust_y: f32,
    pub thrust_z: f32,
    pub thrust_app_z: f32,
    pub aq_plane_force: f32,
    pub aq_plane_limit: f32,
    pub aq_plane_offset: f32,
    pub wave_audio_mult: f32,
    pub move_res: Vec3,
    pub turn_res: Vec3,
    pub look_lr_behind_cam_height: f32,
}

impl BoatHandling {
    pub fn from_raw(b: &sa_formats::vehicle::BoatHandling) -> Self {
        Self {
            thrust_y: b.thrust_y,
            thrust_z: b.thrust_z,
            thrust_app_z: b.thrust_app_z,
            aq_plane_force: b.aq_plane_force,
            aq_plane_limit: b.aq_plane_limit,
            aq_plane_offset: b.aq_plane_offset,
            wave_audio_mult: b.wave_audio_mult,
            move_res: Vec3::from(b.move_res),
            turn_res: Vec3::from(b.turn_res),
            look_lr_behind_cam_height: b.look_lr_behind_cam_height,
        }
    }
}

/// flags5AC.
pub mod bf {
    pub const IN_WATER: u8 = 0x1;
    pub const PROP_IN_WATER: u8 = 0x2;
    pub const ANCHORED: u8 = 0x4;
}

const WAKE_LIFETIME: f32 = 150.0;

pub struct Boat {
    pub h: VehicleHandling,
    pub bh: BoatHandling,
    pub model: u16,
    pub input: CarInput,
    pub gas: f32,
    pub brake: f32,
    /// +0x58C smoothed steering input, +0x494 steer angle (rad).
    pub steer_in: f32,
    pub steer_angle: f32,
    /// +0x5A4 / +0x5A8 propeller speed and rotation, +0x5A0 radar spin.
    pub prop_speed: f32,
    pub prop_rot: f32,
    pub moving_hi_rot: f32,
    pub flags: u8,
    /// +0xA0 buoyancy constant (decays after BlowUpCar).
    pub buoyancy: f32,
    pub health: f32,
    /// +0x608 burning timer (ms).
    pub burn_ms: f32,
    pub destroyed: bool,
    blow_up: bool,
    /// +0x630 last buoyancy force, +0x640 last immersion.
    pub water_damping: Vec3,
    last_imm: f32,
    /// bOnGround / bHitWall sampled before CPhysical::ProcessControl.
    on_ground: bool,
    hit_wall: bool,
    /// Wake trail (newest first): points, lifetimes, intensities.
    pub wake: [(glam::Vec2, f32, u8); 32],
    pub wake_count: u16,
    /// createdBy == mission (the wake length).
    pub mission: bool,
    /// The col model's bounding box minimum (thrust point).
    col_min: Vec3,
    col_max: Vec3,
}

impl Boat {
    pub fn new(h: VehicleHandling, bh: BoatHandling, model: u16, col: &ColModel) -> Self {
        let buoyancy = h.buoyancy_constant;
        Self {
            h,
            bh,
            model,
            input: CarInput::default(),
            gas: 0.0,
            brake: 0.0,
            steer_in: 0.0,
            steer_angle: 0.0,
            prop_speed: 0.0,
            prop_rot: 0.0,
            moving_hi_rot: 0.0,
            flags: 7,
            buoyancy,
            health: 1000.0,
            burn_ms: 0.0,
            destroyed: false,
            blow_up: false,
            water_damping: Vec3::ZERO,
            last_imm: 7.0,
            on_ground: false,
            hit_wall: false,
            wake: [(glam::Vec2::ZERO, 0.0, 0); 32],
            wake_count: 0,
            mission: false,
            col_min: col.bbox_min,
            col_max: col.bbox_max,
        }
    }

    /// The CBoat ctor's physical setup: turnMass ×0.5, elasticity 0.1, in water.
    pub fn setup_physical(&self, p: &mut Physical) {
        p.mass = self.h.mass;
        p.turn_mass = self.h.turn_mass * 0.5;
        p.com = self.h.centre_of_mass;
        p.elasticity = 0.1;
        p.air_resistance = self.h.air_resistance();
        p.flags |= pf::IN_WATER | pf::TOUCHING_WATER;
    }

    /// `ProcessControlInputs` (0x6F0A10), pad path.
    fn process_control_inputs(&mut self) {
        let i = self.input;
        self.brake += (i.brake - self.brake) * 0.1;
        self.brake = self.brake.clamp(0.0, 1.0);
        if self.brake < 0.05 {
            self.brake = 0.0;
            self.gas = i.accelerate;
        } else {
            self.gas = -0.3 * self.brake;
        }
        self.steer_in = (self.steer_in + (i.steer - self.steer_in) * 0.2).clamp(-1.0, 1.0);
        self.steer_angle = self.h.steering_lock.to_radians() * self.steer_in * self.steer_in * self.steer_in.signum();
    }

    /// `PruneWakeTrail` (0x6F0E20).
    fn prune_wake_trail(&mut self, ts: f32) {
        for i in 0..32 {
            let life = &mut self.wake[i].1;
            if *life <= 0.0 {
                self.wake_count = i as u16;
                return;
            }
            if *life <= ts {
                *life = 0.0;
                self.wake_count = i as u16;
                return;
            }
            *life -= ts;
        }
    }

    /// `AddWakePoint` (0x6F2550).
    fn add_wake_point(&mut self, p: &Physical, at: Vec3) {
        let intensity = (p.move_speed.length() * 100.0) as i32 as u8;
        if self.wake[0].1 <= 0.0 {
            self.wake[0] = (at.truncate(), WAKE_LIFETIME, intensity);
            self.wake_count = 1;
            return;
        }
        if (p.matrix.pos.truncate() - self.wake[0].0).length_squared() <= 4.0 {
            return;
        }
        let max = if p.status == Status::Player {
            31
        } else if self.mission {
            20
        } else {
            15
        };
        let n = (self.wake_count as usize).min(max);
        for i in (1..=n).rev() {
            self.wake[i] = self.wake[i - 1];
        }
        self.wake[0] = (at.truncate(), WAKE_LIFETIME, intensity);
        if self.wake_count < 32 {
            self.wake_count += 1;
        }
    }

    /// `CVehicle::ProcessBoatControl` (0x6DBCE0) with the water. Called by the world after
    /// `CPhysical::ProcessControl`.
    pub fn process_boat_control(&mut self, p: &mut Physical, w: &WaterLevel, wavyness: f32, now_ms: u32, ts: f32) {
        let (on_ground, hit_wall) = (self.on_ground, self.hit_wall);
        let speed_p = p.clone();
        let input = BoatBuoyancyIn {
            matrix: &p.matrix,
            bbox_min: self.col_min,
            bbox_max: self.col_max,
            model: self.model,
            touching: p.flags & pf::TOUCHING_WATER != 0,
            b: self.buoyancy,
            damping: self.h.susp_damping,
            ts,
            no_turn: hit_wall,
        };
        let Some(r) = process_buoyancy_boat(w, &input, |o| speed_p.get_speed(o), wavyness, now_ms) else {
            p.flags &= !pf::IN_WATER;
            self.flags &= !bf::IN_WATER;
            return;
        };
        for (f, at) in &r.turn_forces {
            p.apply_turn_force(*f, *at);
        }
        let imm = r.immersion;
        let force = r.force;
        let turn_point = r.turn_point;
        let m = p.mass;
        let j = p.turn_mass;
        // In water / capsize.
        let in_water = force.z > ts * m * 0.0008;
        if in_water {
            p.flags |= pf::IN_WATER;
        } else {
            p.flags &= !pf::IN_WATER;
        }
        p.apply_move_force(force);
        if hit_wall {
            p.apply_turn_force(force * 0.4, turn_point);
        }
        let (right, fwd, up) = (p.matrix.right, p.matrix.fwd, p.matrix.up);
        // Aquaplaning.
        if !on_ground && in_water && up.z > 0.0 {
            let mut a = p.move_speed.length_squared() * self.bh.aq_plane_force * ts * force.z * 0.5;
            a = if self.gas > 0.05 { a * self.gas } else { 0.0 };
            a = a.min(ts * self.bh.aq_plane_limit * m * 0.008);
            p.apply_move_force(up * a);
            p.apply_turn_force(up * a, turn_point - fwd * self.bh.aq_plane_offset);
        }
        // Steering block.
        let player = p.status == Status::Player;
        let mut s2 = 1.0f32;
        let moving = self.gas.abs() > 0.05 || {
            s2 = (p.move_speed.x * p.move_speed.x + p.move_speed.y * p.move_speed.y).sqrt();
            s2 > 0.01
        };
        if up.z > -0.6 && moving {
            if in_water && s2 > 0.05 {
                let at = p.matrix.pos;
                self.add_wake_point(p, at);
            }
            let mut f = 1.0f32;
            if player {
                f = p.move_speed.dot(fwd) * self.h.traction_bias;
                if self.input.handbrake {
                    f *= 0.5;
                }
                f = (1.0 - f).clamp(0.0, 1.0);
            }
            let tp = p.matrix.rotate(Vec3::new(0.0, self.col_min.y * self.bh.thrust_y, self.col_min.z * self.bh.thrust_z));
            let th = -f * self.steer_angle;
            let (sth, cth) = th.sin_cos();
            let at = p.matrix.pos + tp;
            match w.level(at.x, at.y, at.z, true, wavyness, now_ms).map(|l| l.0).filter(|l| *l > at.z - 0.5) {
                Some(lvl) => {
                    let d = lvl - at.z + 0.5;
                    let depth = if d > 1.0 { 1.0 } else { d * d };
                    self.flags |= bf::PROP_IN_WATER;
                    let mut rudder = true;
                    if self.gas.abs() > 0.01 {
                        rudder = self.gas.abs() < 0.5;
                        let mut t = p.matrix.rotate(Vec3::new(-sth, cth, -self.steer_angle.abs()))
                            * (depth * self.gas * 40.0 * self.h.trans.engine_accel * m);
                        if t.z > 0.2 {
                            t.z = (1.2 - t.z) * (1.2 - t.z) + 0.2;
                        }
                        if !on_ground {
                            p.apply_move_force(t * ts);
                            p.apply_turn_force(t * ts, tp - up * self.bh.thrust_app_z);
                            p.apply_turn_force(right * (-(t.dot(right)) * self.h.traction_mult * ts), up);
                        } else {
                            if self.gas < 0.0 {
                                t.x *= 5.0;
                                t.y *= 5.0;
                            }
                            t.z = t.z.max(0.0);
                            p.apply_move_force(t * ts);
                        }
                    }
                    if !on_ground && rudder {
                        let mut x = (p.move_speed.dot(fwd) * self.h.traction_loss).min(j * 0.01);
                        if self.gas.abs() > 0.01 {
                            x *= (0.55 - self.gas.abs()) * if player { 2.6 } else { 5.0 };
                        }
                        if (self.gas < 0.0 && x > 0.0) || (self.gas > 0.0 && x < 0.0) {
                            x = -x;
                        }
                        let fr = right * (-sth * x * depth);
                        p.apply_move_force(fr * ts);
                        p.apply_turn_force(fr * ts, tp);
                        if f > 0.0 {
                            p.apply_turn_force(right * (ts.max(0.01) / f * x * sth * -0.75), up);
                        }
                    }
                }
                None => self.flags &= !bf::PROP_IN_WATER,
            }
        }
        // Sideslip.
        if self.h.susp_bias != 0.0 {
            let s = Vec3::new(fwd.y, -fwd.x, 0.0);
            let k = -(s.dot(p.move_speed) * self.h.susp_bias * ts * imm * m * 0.1);
            p.apply_move_force(Vec3::new(s.x - 0.3 * s.y, s.y + 0.3 * s.x, 0.0) * k);
        }
        // Handbrake drag.
        if player && self.input.handbrake {
            let fs = p.move_speed.dot(fwd);
            if fs > 0.0 {
                p.apply_move_force(fwd * (fs * self.h.susp_lower * ts * imm * m * -0.1));
            }
        }
        if in_water && !on_ground && !hit_wall {
            self.apply_water_resistance(p, imm, ts);
        }
        // Turn resistance.
        if !hit_wall {
            let tr = self.bh.turn_res;
            let (px, py, pz) = (tr.x.powf(ts), tr.y.powf(ts), tr.z.powf(ts));
            let mt = p.matrix;
            let w = p.turn_speed;
            let mut wl = Vec3::new(w.dot(mt.right), w.dot(mt.fwd), w.dot(mt.up));
            let pitch_imp = px * wl.x / (wl.x * wl.x * 1000.0 + 1.0) - wl.x;
            wl.y *= py;
            wl.z *= pz;
            p.turn_speed = mt.rotate(wl);
            let com = mt.rotate(p.com);
            p.apply_turn_force(mt.up * (pitch_imp * j), mt.fwd + com);
        }
        // Wave slam.
        let di = ((imm - self.last_imm) * 10000.0) as i32 as i16 as f32;
        if !on_ground && in_water && p.matrix.up.z > 0.0 && di > 200.0 {
            let v = p.move_speed;
            let mut z = v.length_squared() * di * 0.001;
            if z + v.z > self.h.brake_decel {
                z = self.h.brake_decel - v.z;
            }
            z = z.max(0.0);
            let wv = (Vec3::new(0.0, 0.0, z) + p.matrix.fwd * (di * self.h.brake_bias * -0.01 * v.dot(p.matrix.fwd))) * m;
            p.apply_move_force(wv);
            p.apply_turn_force(wv, turn_point);
        }
        self.water_damping = force;
        self.last_imm = imm;
        if in_water {
            self.flags |= bf::IN_WATER;
        } else {
            self.flags &= !bf::IN_WATER;
        }
    }

    /// `ApplyBoatWaterResistance` (0x6D2740): the immersion is used squared (as coded).
    fn apply_water_resistance(&self, p: &mut Physical, imm: f32, ts: f32) {
        let m = p.mass;
        let k = imm * self.h.susp_force * imm * m * 0.001;
        let mt = p.matrix;
        let fs = p.move_speed.dot(mt.fwd);
        let r = (k * (fs * fs + 0.05) + 1.0).abs();
        let q = 1.0 / r;
        let t = ts * 0.5;
        let mr = self.bh.move_res;
        let (px, py, pz) = ((q * mr.x).powf(t), (q * mr.y).powf(t), (q * mr.z).powf(t));
        let v = p.move_speed;
        let mut vl = Vec3::new(v.dot(mt.right), v.dot(mt.fwd), v.dot(mt.up));
        vl.x *= px;
        vl.y *= py;
        vl.z *= pz;
        p.move_speed = mt.rotate(vl);
        p.apply_turn_force(mt.fwd * ((py - 1.0) * vl.y * m), -mt.up);
        p.move_speed.z *= if p.move_speed.z > 0.0 { pz } else { (1.0 + pz) * 0.5 };
    }
}

impl BodyLogic for Boat {
    /// `CBoat::ProcessControl` (0x6F1770); ProcessBoatControl runs in the world's water step.
    fn process_control(&mut self, p: &mut Physical, _col: &mut ColModel, ctx: &Ctx, _lines: &LineHits) {
        let ts = ctx.ts;
        self.prune_wake_trail(ts);
        if self.destroyed && p.mass * 0.0064 < self.buoyancy {
            self.buoyancy -= p.mass * 8e-6;
        }
        match p.status {
            Status::Player => {
                self.flags &= !bf::ANCHORED;
                self.process_control_inputs();
            }
            Status::Abandoned | Status::Wrecked => {
                self.flags |= bf::IN_WATER | bf::PROP_IN_WATER;
                p.flags |= pf::IN_WATER;
                self.steer_angle = 0.0;
                self.brake = 0.5;
                self.gas = 0.0;
            }
            _ => {}
        }
        // Propeller speed.
        if !matches!(p.status, Status::Player | Status::Physics) {
            if self.prop_speed > 0.0 {
                self.prop_speed *= 0.95;
            }
        } else {
            let k = if self.gas != 0.0 { 0.05 } else { 0.01 };
            let target = if self.gas > 0.0 {
                0.18 + 0.16 * self.gas
            } else if self.gas < 0.0 {
                0.18 + 0.13 * self.gas
            } else {
                0.0
            };
            self.prop_speed += (target - self.prop_speed) * ts * k;
        }
        // Damage, fire and blowing up (§5.1).
        let dmg = p.damage_intensity * self.h.collision_damage;
        if dmg > 25.0 && p.status != Status::Wrecked && self.health >= 250.0 {
            let old = self.health;
            self.health -= (dmg - 25.0) * if p.status == Status::Player { 0.5 } else { 0.25 };
            if self.health <= 0.0 && old > 0.0 {
                self.health = 1.0;
            }
        }
        if self.health <= 460.0 && p.status != Status::Wrecked {
            if self.health < 250.0 {
                self.burn_ms += (ts * 0.02 * 1000.0) as i32 as f32;
                if self.burn_ms > 5000.0 {
                    self.blow_up = true;
                }
            }
        } else {
            self.burn_ms = 0.0;
        }
        if self.blow_up && !self.destroyed {
            // BlowUpCar (0x6F21B0): the explosion is sent from process_effects.
            p.move_speed.z += 0.13;
            p.status = Status::Wrecked;
            self.destroyed = true;
            self.health = 0.0;
        }
        self.on_ground = p.damage_intensity > 0.0 && p.last_collision_impact_velocity.z > 0.1;
        self.hit_wall = p.has_e(ef::HAS_HIT_WALL);
        p.process_control(ctx);
    }

    fn process_effects(&mut self, id: EntityId, p: &mut Physical, col: &ColModel, f: &mut FrameFx) {
        if self.blow_up {
            self.blow_up = false;
            f.requests.push(WorldRequest::Explosion {
                victim: Some(id),
                creator: None,
                kind: ExplosionType::Boat,
                pos: p.matrix.pos,
                lifetime_ms: 0,
                cam_shake: -1.0,
                no_damage: false,
            });
        }
        // PreRender: the propeller turns, the radar spins.
        self.prop_rot += f.ts * self.prop_speed;
        while self.prop_rot > std::f32::consts::TAU {
            self.prop_rot -= std::f32::consts::TAU;
        }
        if matches!(self.model, 430 | 453 | 454) {
            self.moving_hi_rot += f.ts * 0.02;
        }
        self.do_boat_splashes(p, col, f);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Boat {
    /// `CVehicle::DoBoatSplashes` (0x6DD130), `prt_boatsplash` off both sides of the bow.
    fn do_boat_splashes(&self, p: &Physical, col: &ColModel, f: &mut FrameFx) {
        let splash = self.water_damping.length();
        let v = p.move_speed;
        if v.length_squared() <= 0.0025 || p.matrix.up.z <= 0.0 {
            return;
        }
        let d = (p.matrix.pos - f.cam).truncate().length();
        if d >= 80.0 {
            return;
        }
        let mut s = (v.length() * 0.075 * splash).min(1.0);
        if s <= 0.15 {
            return;
        }
        s *= 0.75;
        let mut ab = ((s * 128.0) as i32 & 0xFF).min(64);
        if d > 50.0 {
            ab = ((80.0 - d) / 30.0 * ab as f32) as i32;
        }
        let r = |f: &mut FrameFx, a: f32, b: f32| a + (b - a) * f.rng.rand01();
        let life = ((2.0 * s + 0.3) * r(f, 0.8, 1.2) * 0.2).min(1.0);
        let mult = PrtMult::new(1.0, 1.0, 1.0, (ab as f32 / 255.0).min(1.0), ((10.0 * s + 0.75) * 0.1).min(1.0), 0.0, life);
        let m = p.matrix;
        for (side, sign) in [(col.bbox_min.x, -1.0f32), (col.bbox_max.x, 1.0)] {
            let at = m.transform(Vec3::new(side * 0.7, col.bbox_max.y * 0.5, 0.0));
            let a = r(f, 0.8, 1.2);
            let b = r(f, 0.3, 0.7);
            let c = r(f, 0.8, 1.2);
            let vel = (-m.fwd * a + m.right * (sign * b) + m.up * c) * (10.0 * s);
            f.fx.add_particle("prt_boatsplash", at, vel, 0.0, mult, -1.0, 1.2, 0.6, false);
        }
    }
}

/// The boats' hull water masks (model space quads, `CBoat::Render` 0x6F0210).
pub fn hull_mask(model: u16) -> &'static [[[f32; 3]; 4]] {
    match model {
        430 => &[[[-1.45, 1.9, 0.96], [1.45, 1.9, 0.96], [-1.45, -3.75, 0.96], [1.45, -3.75, 0.96]]],
        446 => &[[[-1.222, 2.004, 1.409], [1.222, 2.004, 1.409], [-1.24, -1.367, 0.846], [1.24, -1.367, 0.846]]],
        452 => &[[[-1.15, 3.61, 1.03], [1.15, 3.61, 1.03], [-1.15, 0.06, 1.03], [1.15, 0.06, 1.03]]],
        453 => &[[[-1.66, -4.48, 0.83], [1.66, -4.48, 0.83], [-1.9, 2.83, 1.0], [1.9, 2.83, 1.0]]],
        454 => &[[[-1.886, -2.347, 0.787], [1.886, -2.347, 0.787], [-1.886, -4.67, 0.842], [1.886, -4.67, 0.842]]],
        472 => &[
            [[-0.663, 3.565, 0.382], [0.663, 3.565, 0.382], [-1.087, 0.831, 0.381], [1.087, 0.831, 0.381]],
            [[-1.087, 0.831, 0.381], [1.087, 0.831, 0.381], [-1.097, -2.977, 0.381], [1.097, -2.977, 0.381]],
        ],
        473 => &[[[-0.797, 1.641, 0.573], [0.797, 1.641, 0.573], [-0.865, -1.444, 0.509], [0.865, -1.444, 0.509]]],
        484 => &[[[-1.246, -1.373, 0.787], [1.246, -1.373, 0.787], [-1.023, -5.322, 0.787], [1.023, -5.322, 0.787]]],
        595 => &[[[-1.0, 2.5, 0.3], [1.0, 2.5, 0.3], [-1.0, -5.4, 0.3], [1.0, -5.4, 0.3]]],
        _ => &[],
    }
}
