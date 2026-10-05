//! `CBike` (bike.md): motorbikes. Suspension lines, CVehicle::ProcessBikeWheel, the lean
//! (visual, from the lateral acceleration), balance damping, the upright torque, wheelies and
//! stoppies, the player's lean torques and inputs, knock-off detection and the PreRender frame
//! values (forks, swing arm, wheels, chassis lean).
//!
//! Not ported: CBmx (bicycles: pedalling, bunny hop), rider anims (ProcessRiderAnims) and the
//! tuck boost that reads them, rest detection, NPC / AI riding, burnout smoke, the bike's own
//! buoyancy, burst tyres, DoBurstAndSoftGroundRatios, fire / blowing up, mouse steering.

use std::sync::atomic::{AtomicBool, Ordering};

use glam::{Vec2, Vec3};

use crate::{
    Ctx,
    automobile::{CarInput, VehicleHandling, WheelState},
    collision::{ColLine, ColModel},
    colpoint::ColPoint,
    damage::Rand,
    effects::FrameFx,
    physical::{Physical, Status},
    surface::SurfaceInfos,
    world::{BodyLogic, EntityId, LineHits},
};

/// `bAlreadySkidding` (0xC1CDAF): sticky, only ever set.
static BIKE_ALREADY_SKIDDING: AtomicBool = AtomicBool::new(false);

/// `tBikeHandlingData` after ConvertBikeDataToGameUnits (0x6F5290).
#[derive(Debug, Clone)]
pub struct BikeHandling {
    pub lean_fwd_com: f32,
    pub lean_fwd_force: f32,
    pub lean_bak_com: f32,
    pub lean_bak_force: f32,
    /// sin(MaxLean).
    pub max_lean: f32,
    /// FullAnimLean in radians.
    pub full_anim_lean: f32,
    pub des_lean: f32,
    pub speed_steer: f32,
    pub slip_steer: f32,
    pub no_player_com_z: f32,
    /// sin(WheelieAng), sin(StoppieAng).
    pub wheelie_ang: f32,
    pub stoppie_ang: f32,
    pub wheelie_steer: f32,
    pub wheelie_stab_mult: f32,
    pub stoppie_stab_mult: f32,
}

impl BikeHandling {
    pub fn from_raw(b: &sa_formats::vehicle::BikeHandling) -> Self {
        let rad = 0.017_453_292f32;
        Self {
            lean_fwd_com: b.lean_fwd_com,
            lean_fwd_force: b.lean_fwd_force,
            lean_bak_com: b.lean_bak_com,
            lean_bak_force: b.lean_bak_force,
            max_lean: (b.max_lean * rad).sin(),
            full_anim_lean: b.full_anim_lean * rad,
            des_lean: b.des_lean,
            speed_steer: b.speed_steer,
            slip_steer: b.slip_steer,
            no_player_com_z: b.no_player_com_z,
            wheelie_ang: (b.wheelie_ang * rad).sin(),
            stoppie_ang: (b.stoppie_ang * rad).sin(),
            wheelie_steer: b.wheelie_steer,
            wheelie_stab_mult: b.wheelie_stab_mult,
            stoppie_stab_mult: b.stoppie_stab_mult,
        }
    }
}

/// Model-space frame positions the bike needs (from the DFF).
#[derive(Debug, Clone, Copy, Default)]
pub struct BikeFrames {
    pub wheel_front: Vec3,
    pub wheel_rear: Vec3,
    pub forks_front: Option<Vec3>,
    pub forks_rear: Option<Vec3>,
}

/// +0x614 flags.
pub mod bkf {
    pub const NO_KNOCK_OFF_IN_WATER: u8 = 0x04;
    pub const BALANCED: u8 = 0x08;
    pub const ON_STAND: u8 = 0x10;
    pub const BOOST: u8 = 0x20;
    pub const BURNING: u8 = 0x40;
    pub const WHEELIE_CAM: u8 = 0x80;
}

pub struct Bike {
    pub h: VehicleHandling,
    pub bh: BikeHandling,
    pub model: u16,
    pub wheel_size: [f32; 2],
    /// tan(rake) and the rake in degrees.
    pub rake_tan: f32,
    pub rake_deg: f32,
    spring_len: [f32; 4],
    line_len: [f32; 4],
    pub front_node_z: f32,
    pub rear_node_z: f32,
    pub swingarm_len: f32,
    /// Steering-head pivot (y, z).
    pub head_pivot: Vec2,
    height_above_road: f32,
    /// Visual hub z per wheel.
    pub hub_z: [f32; 2],
    pub ratio: [f32; 4],
    prev_ratio: [f32; 4],
    timer: [f32; 4],
    wcp: [ColPoint; 4],
    pub wheel_lighting: [u8; 4],
    /// 0 ok, 1 burst, 2 missing.
    pub wheel_status: [u8; 2],
    wheel_state: [WheelState; 2],
    pub wheel_speed: [f32; 2],
    pub wheel_rot: [f32; 2],
    /// +0x58C steer stick, +0x650 lean stick (+1 = forward).
    steer_in: f32,
    lean_in: f32,
    /// +0x494 target steer (rad, + = left), +0x644 actual.
    pub steer: f32,
    pub steer_actual: f32,
    /// +0x64C lean state, +0x648 render lean (+ = right).
    lean: f32,
    pub lean_render: f32,
    pub gas: f32,
    pub brake: f32,
    pub handbrake: bool,
    burnout: bool,
    grip_recovery: f32,
    gear: u8,
    ground_normal: Vec3,
    ground_right: Vec3,
    prev_move: Vec3,
    pub flags: u8,
    contact_lines: u8,
    rear_on_ground: u8,
    pub health: f32,
    /// Set when DamageKnockOffRider fires (the app takes the rider off).
    pub knock_off: Option<Vec3>,
    pub input: CarInput,
    /// Lean stick (−1 back .. +1 forward), set by the app (pad up/down).
    pub input_lean: f32,
    pub surfaces: SurfaceInfos,
    pub engine_on: bool,
    rng: Rand,
    /// The steering frame axis (rake) and colModel min z (chassis drop).
    col_min_z: f32,
}

impl Bike {
    /// The ctor (0x6BF430) and SetupSuspensionLines (0x6B89B0): four lines, two per wheel at
    /// ±¼ of the wheel size.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        h: VehicleHandling,
        bh: BikeHandling,
        model: u16,
        wheel_size: [f32; 2],
        rake_deg: f32,
        frames: BikeFrames,
        col: &mut ColModel,
        surfaces: SurfaceInfos,
    ) -> Self {
        col.lines.clear();
        let mut spring_len = [0.0; 4];
        let mut line_len = [0.0; 4];
        let (mut front_node_z, mut rear_node_z, mut swingarm_len) = (0.0, 0.0, 0.0);
        for i in 0..4 {
            let (w, sz) = if i < 2 { (frames.wheel_front, wheel_size[0]) } else { (frames.wheel_rear, wheel_size[1]) };
            let dy = if i == 0 || i == 2 { 0.25 * sz } else { -0.25 * sz };
            if i == 0 {
                front_node_z = w.z;
            }
            if i == 2 {
                rear_node_z = w.z;
                swingarm_len = frames.forks_rear.map_or(0.0, |s| Vec2::new(w.y - s.y, w.z - s.z).length());
            }
            let p = Vec3::new(w.x, w.y + dy, w.z);
            let start = Vec3::new(p.x, p.y, p.z + h.susp_upper);
            let end = Vec3::new(p.x, p.y, p.z + h.susp_upper + (h.susp_lower - h.susp_upper) - sz * 0.5);
            col.lines.push(ColLine { start, end });
            spring_len[i] = h.susp_upper - h.susp_lower;
            line_len[i] = start.z - end.z;
        }
        let head_pivot = frames.forks_front.map_or(Vec2::ZERO, |f| Vec2::new(f.y, f.z));
        let height_above_road = wheel_size[0] * 0.5 - col.lines[0].start.z + (1.0 - 1.0 / (h.susp_force * 4.0)) * spring_len[0];
        let hub_z = [wheel_size[0] * 0.5 - height_above_road, wheel_size[1] * 0.5 - height_above_road];
        col.bbox_min.z = col.bbox_min.z.min(col.lines[0].end.z);
        col.bound_radius = col.bound_radius.max(col.bbox_min.length()).max(col.bbox_max.length());
        let col_min_z = col.bbox_min.z;
        Self {
            h,
            bh,
            model,
            wheel_size,
            rake_tan: rake_deg.to_radians().tan(),
            rake_deg,
            spring_len,
            line_len,
            front_node_z,
            rear_node_z,
            swingarm_len,
            head_pivot,
            height_above_road,
            hub_z,
            ratio: [1.0; 4],
            prev_ratio: [1.0; 4],
            timer: [0.0; 4],
            wcp: [ColPoint::default(); 4],
            wheel_lighting: [0x48; 4],
            wheel_status: [0; 2],
            wheel_state: [WheelState::Normal; 2],
            wheel_speed: [0.0; 2],
            wheel_rot: [0.0; 2],
            steer_in: 0.0,
            lean_in: 0.0,
            steer: 0.0,
            steer_actual: 0.0,
            lean: 0.0,
            lean_render: 0.0,
            gas: 0.0,
            brake: 0.0,
            handbrake: false,
            burnout: false,
            grip_recovery: 1.0,
            gear: 1,
            ground_normal: Vec3::Z,
            ground_right: Vec3::X,
            prev_move: Vec3::ZERO,
            flags: bkf::ON_STAND,
            contact_lines: 0,
            rear_on_ground: 0,
            health: 1000.0,
            knock_off: None,
            input: CarInput::default(),
            input_lean: 0.0,
            surfaces,
            engine_on: false,
            rng: Rand::new(model as u32 * 977 + 3),
            col_min_z,
        }
    }

    /// The ctor's physical values.
    pub fn setup_physical(&self, p: &mut Physical) {
        p.mass = self.h.mass;
        p.turn_mass = self.h.turn_mass;
        p.com = self.h.centre_of_mass;
        p.com.z = 0.1;
        p.elasticity = 0.05;
        p.air_resistance = self.h.air_resistance();
    }

    /// `ProcessControlInputs` (0x6BE310), pad path.
    fn process_control_inputs(&mut self, p: &Physical, ts: f32) {
        let fwd_speed = p.move_speed.dot(p.matrix.fwd);
        self.handbrake = self.input.handbrake;
        self.steer_in = (self.steer_in + (self.input.steer - self.steer_in) * ts * 0.2).clamp(-1.0, 1.0);
        self.lean_in = (self.lean_in + (self.input_lean - self.lean_in) * ts * 0.2).clamp(-1.0, 1.0);
        let acc = self.input.accelerate - self.input.brake;
        if fwd_speed.abs() >= 0.01 {
            if fwd_speed < 0.0 {
                if acc >= 0.0 {
                    self.brake = acc;
                    self.gas = 0.0;
                } else {
                    self.gas = acc;
                    self.brake = 0.0;
                }
            } else if acc < 0.0 {
                self.gas = 0.0;
                self.brake = -acc;
            } else {
                self.gas = acc;
                self.brake = 0.0;
            }
        } else if self.input.accelerate * 255.0 > 150.0 && self.input.brake * 255.0 > 150.0 {
            self.gas = self.input.accelerate;
            self.brake = self.input.brake;
            self.burnout = true;
        } else {
            self.gas = acc;
            self.brake = 0.0;
        }
        let s = self.steer_in * self.steer_in * self.steer_in.signum();
        self.steer = self.h.steering_lock.to_radians() * s;
    }

    /// The lean torques of ProcessAI's player branch (§3.2), stat modifier 11 = 1.
    fn lean_torques(&mut self, p: &mut Physical, ts: f32) {
        let inp = self.lean_in;
        let s = p.move_speed.length().min(0.1);
        let (k, apply) = if inp < 0.0 {
            p.com.y = self.bh.lean_bak_com * inp + self.h.centre_of_mass.y;
            let apply = (self.brake == 0.0 && !self.handbrake) || self.contact_lines == 0;
            (self.bh.lean_bak_force * p.turn_mass * inp * s * ((s / 0.1).max(self.gas) + self.gas) * 0.5, apply)
        } else {
            p.com.y = self.bh.lean_fwd_com * inp + self.h.centre_of_mass.y;
            let apply = self.brake < 0.0 || self.contact_lines == 0;
            (self.bh.lean_fwd_force * p.turn_mass * inp * s * ((s / 0.1).max(self.brake) + self.brake) * 0.5, apply)
        };
        if apply {
            let at = p.com + p.matrix.fwd;
            p.apply_turn_force(p.matrix.up * -(ts * k), at);
        }
    }

    /// `DoSoftGroundResistance` (0x6B6D40).
    fn soft_ground(&mut self, p: &mut Physical, ts: f32, extra: &mut u32) {
        let up = p.matrix.up;
        let sand = (0..4).any(|i| self.ratio[i] < 1.0 && self.surfaces.adhesion_group(self.wcp[i].surface_b) == 4);
        if sand {
            let mut v = p.move_speed - up * p.move_speed.dot(up);
            if self.gas > 0.3 {
                if v.length() < 0.3 {
                    *extra |= 4;
                }
                v -= p.matrix.fwd * v.dot(p.matrix.fwd);
            }
            p.apply_move_force(v * -(ts * p.mass * 0.02));
        } else if (0..4).any(|i| self.ratio[i] < 1.0 && self.wcp[i].surface_b == 178) {
            let v = p.move_speed - up * p.move_speed.dot(up);
            p.apply_move_force(v * -(ts * p.mass * 0.003));
        }
    }

    /// COM and the pitch / roll rate damping (§2.2).
    fn balance(&mut self, p: &mut Physical, ts: f32, extra: u32) {
        if extra & 2 == 0 && self.flags & (bkf::ON_STAND | bkf::BALANCED) == 0 {
            p.com = Vec3::new(self.h.centre_of_mass.x, self.h.centre_of_mass.y, self.bh.no_player_com_z);
            return;
        }
        let (mut a, b, mut c) = (0.9995f32, 0.9f32, 1.0f32);
        let m = p.matrix;
        let w = p.turn_speed;
        let t = Vec3::new(w.dot(m.right), w.dot(m.fwd), w.dot(m.up));
        if p.status == Status::Player {
            let mod13 = 1.0f32;
            if self.ratio[0] >= 1.0 && self.ratio[1] >= 1.0 {
                c = 100.0;
                let s = mod13 * 0.2;
                if (self.ratio[2] < 1.0 || self.ratio[3] < 1.0) && m.fwd.z > 0.0 {
                    a = 0.9995 - ((self.bh.wheelie_ang - m.fwd.z).abs() * s).min(0.05);
                } else {
                    a = 0.98;
                }
            } else if self.timer[2] <= 0.0 && self.timer[3] <= 0.0 {
                c = 100.0;
                let (s1, s2) = (mod13 * 0.075, mod13 * 0.25);
                if m.fwd.z < 0.0 {
                    a = (((self.bh.stoppie_ang - m.fwd.z).abs() * s2).min(s1) + 0.9) * 0.9995;
                }
            }
        }
        let a2 = a / (t.x * t.x * c + 1.0);
        let b2 = b / (1000.0 * t.y * t.y + 1.0);
        let d_pitch = t.x * a2.powf(ts) - t.x;
        let d_roll = t.y * b2.powf(ts) - t.y;
        let p0 = m.rotate(p.com);
        let tm = p.turn_mass;
        p.apply_turn_force(-m.up * d_roll * tm, p0 + m.right);
        p.apply_turn_force(m.up * d_pitch * tm, p0 + m.fwd);
        if p.status != Status::Player {
            p.com = self.h.centre_of_mass;
        }
    }

    /// `CVehicle::ProcessBikeWheel` (0x6D73B0).
    #[allow(clippy::too_many_arguments)]
    fn process_bike_wheel(
        &mut self,
        p: &mut Physical,
        ts: f32,
        f: Vec3,
        rt: Vec3,
        cs: Vec3,
        cp: Vec3,
        thrust: f32,
        brake: f32,
        adhesion: f32,
        mut destab: f32,
        wheel: usize,
    ) {
        let n_contact = 2.0f32;
        let fwd_speed = f.x * cs.x + cs.z * f.z + f.y * cs.y;
        let braking = brake != 0.0;
        let driving = !braking && thrust != 0.0;
        let reverse = !braking && thrust < 0.0;
        let mut st = self.wheel_state[wheel];
        let state = &mut st;
        if *state != WheelState::Normal {
            BIKE_ALREADY_SKIDDING.store(true, Ordering::Relaxed);
        }
        let skid = BIKE_ALREADY_SKIDDING.load(Ordering::Relaxed);
        *state = WheelState::Normal;
        let mut adh = ts * adhesion;
        if skid {
            adh *= self.h.traction_loss;
        }
        let mut side = 0.0f32;
        let right_speed = rt.dot(cs);
        if right_speed != 0.0 {
            side = -(right_speed / n_contact);
            if self.wheel_status[wheel] == 1 {
                side += (0.1 * self.rng.rand01() - 0.05) * fwd_speed.min(0.12);
            }
        }
        let mut fwd_f = 0.0f32;
        if driving {
            fwd_f = thrust;
            side = side.clamp(-adh, adh);
        } else if fwd_speed != 0.0 {
            let ideal = -(fwd_speed / n_contact);
            let mut b = brake;
            if !braking && self.gas.abs() < 0.01 {
                b = 0.9 * 0.6 / (p.mass + 200.0);
            }
            if b > adh {
                fwd_f = ideal;
                if fwd_speed.abs() > 0.005 {
                    *state = WheelState::Locked;
                }
            } else {
                fwd_f = ideal.clamp(-b, b);
            }
        }
        let sum_sq = fwd_f * fwd_f + side * side;
        if sum_sq <= adh * adh {
            if destab < 1.0 {
                if !skid {
                    destab *= self.h.traction_loss;
                }
                if adh * adh * destab * destab < sum_sq {
                    side *= adh * destab / sum_sq.sqrt();
                }
            }
        } else {
            if *state != WheelState::Locked {
                *state = if !driving || fwd_speed >= 0.1 { WheelState::Skidding } else { WheelState::Spinning };
            }
            let loss = if skid { 1.0 } else { self.h.traction_loss };
            let s = loss / sum_sq.sqrt() * adh;
            fwd_f *= s;
            side *= s;
            if destab < 1.0 {
                side *= destab;
            }
        }
        self.wheel_state[wheel] = st;
        if fwd_f == 0.0 && side == 0.0 {
            return;
        }
        let total = rt * side + f * fwd_f;
        let mag = total.length();
        let dir = total / mag;
        let c = cp.cross(dir);
        let eff = mag / (1.0 / p.mass + c.length_squared() / p.turn_mass);
        p.apply_move_force(dir * (p.mass * mag));
        let turn_f = dir * eff;
        let right = p.matrix.right;
        let fwd = p.matrix.fwd;
        let r = turn_f.dot(right);
        let cp_r = cp.dot(right);
        let cp_f = cp.dot(fwd);
        if wheel != 1 || (!braking && !reverse) {
            p.apply_turn_force((turn_f - right * r) * 2.0, cp - right * cp_r);
        }
        p.apply_turn_force(right * r, fwd * cp_f);
    }

    /// `DamageKnockOffRider` (0x6B5A10) with the player's rider (skill 0).
    fn damage_knock_off(&mut self, p: &Physical, intensity: f32, n: Vec3) {
        let mut k = intensity / p.mass * 800.0;
        if p.status == Status::Player {
            k *= 0.75;
        } else {
            return;
        }
        if k <= 10.0 {
            return;
        }
        let m = p.matrix;
        let fd = n.dot(m.fwd);
        let z = if n.z >= 0.85 { n.z } else { 0.0 };
        let mut fwd_mul = if fd.abs() <= 0.85 { 0.6 } else { 7.0 * z * z + 0.6 };
        if m.up.z < 0.0 {
            fwd_mul = 5.0;
        }
        let (up_neg, mut up_pos) = (1.5f32, 0.05f32);
        if self.model == 468 {
            fwd_mul *= 0.65;
            up_pos = 0.75 * 0.05;
        }
        let nu = n.dot(m.up);
        let k = (n.dot(m.right).abs() * 0.45 + nu.max(0.0) * up_pos + fd.abs() * fwd_mul - nu.min(0.0) * up_neg) * k;
        if k > 75.0 {
            self.knock_off = Some(n);
        }
    }

    /// `CBike::VehicleDamage` (0x6B8EC0), collision part.
    fn vehicle_damage(&mut self, p: &Physical) {
        let i = p.damage_intensity;
        if i < 1.0 {
            return;
        }
        if i > 20.0 {
            self.flags &= !bkf::ON_STAND;
        }
        self.damage_knock_off(p, i, p.last_collision_impact_velocity.normalize_or_zero());
        if i > 25.0 && p.status != Status::Wrecked {
            let k = if p.status == Status::Player { 0.5 } else { 0.25 };
            self.health = (self.health - (i - 25.0) * self.h.collision_damage * k).max(0.0);
        }
    }
}

impl BodyLogic for Bike {
    fn process_control(&mut self, p: &mut Physical, col: &mut ColModel, ctx: &Ctx, lines: &LineHits) {
        let ts = ctx.ts;
        self.flags &= !bkf::BOOST;
        self.burnout = false;
        // ProcessAI (§2.1).
        let mut extra = 0u32;
        match p.status {
            Status::Player => {
                extra |= 2;
                self.flags &= !bkf::BALANCED;
                self.engine_on = true;
                self.process_control_inputs(p, ts);
                self.lean_torques(p, ts);
                self.soft_ground(p, ts, &mut extra);
            }
            Status::Abandoned => {
                self.brake = 0.0;
                self.handbrake = p.move_speed.length_squared() < 0.01 || self.flags & bkf::ON_STAND != 0;
                self.gas = 0.0;
            }
            Status::Wrecked => {
                self.brake = 0.05;
                self.handbrake = true;
                self.steer = 0.0;
                self.gas = 0.0;
            }
            _ => {}
        }
        let m = p.matrix;
        if self.flags & bkf::ON_STAND != 0 && (m.right.z.abs() > 0.35 || m.fwd.z.abs() > 0.5) {
            self.flags &= !bkf::ON_STAND;
        }
        self.balance(p, ts, extra);
        self.vehicle_damage(p);
        let lying = (p.damage_intensity > 0.0
            && p.last_collision_impact_velocity.normalize_or_zero().dot(p.matrix.right).abs() > 0.5
            && p.move_speed.length_squared() < 0.1)
            || self.flags & bkf::BALANCED != 0;
        // Line results of the last collision passes.
        for i in 0..4 {
            self.ratio[i] = lines.values[i];
            if lines.values[i] < 1.0 {
                self.wcp[i] = lines.points[i];
                self.wheel_lighting[i] = lines.points[i].lighting_b;
            }
        }
        // Unridden handlebars drift into the fall.
        if extra & 2 == 0 && self.flags & (bkf::ON_STAND | bkf::BALANCED) == 0 {
            if p.matrix.right.z >= 0.0 {
                if self.steer < 0.4363 {
                    self.steer += ts * 0.008_726_6;
                }
            } else if self.steer > -0.4363 {
                self.steer -= ts * 0.008_726_6;
            }
        }
        p.process_control(ctx);

        // Ratio conversion.
        for i in 0..4 {
            let k = 1.0 - self.spring_len[i] / self.line_len[i];
            self.ratio[i] = (self.ratio[i] - k) / (1.0 - k);
        }
        let m = p.matrix;
        // Suspension (§2.6).
        let mut cp = [Vec3::ZERO; 4];
        let mut dir = [Vec3::ZERO; 4];
        for i in 0..4 {
            if self.ratio[i] < 1.0 {
                cp[i] = self.wcp[i].point - m.pos;
                dir[i] = m.rotate(col.lines[i].end - col.lines[i].start).normalize_or_zero();
            }
        }
        let mut spring_out = [0.0f32; 4];
        for i in 0..4 {
            if self.ratio[i] >= 1.0 {
                cp[i] = m.rotate(col.lines[i].end);
            } else {
                let mut bias = self.h.susp_bias;
                if i >= 2 {
                    bias = 1.0 - bias;
                }
                if self.wcp[i].normal.z <= 0.35 {
                    spring_out[i] = p.apply_spring_collision(ts, self.h.susp_force, dir[i], cp[i], self.ratio[i], bias);
                } else {
                    let mut n = self.wcp[i].normal;
                    spring_out[i] = p.apply_spring_collision_alt(ts, self.h.susp_force, dir[i], cp[i], self.ratio[i], bias, &mut n);
                }
            }
        }
        let spd: [Vec3; 4] = std::array::from_fn(|i| p.get_speed(cp[i]));
        for (a, b) in [(0usize, 1usize), (2, 3)] {
            if self.ratio[a] < 1.0 || self.ratio[b] < 1.0 {
                let n = if self.ratio[a] >= 1.0 { self.wcp[b].normal } else { self.wcp[a].normal };
                if n.z > 0.35 {
                    dir[a] = -n;
                }
                let n = if self.ratio[b] >= 1.0 { self.wcp[a].normal } else { self.wcp[b].normal };
                if n.z > 0.35 {
                    dir[b] = -n;
                }
            }
        }
        for i in 0..4 {
            if self.ratio[i] < 1.0 {
                p.apply_spring_dampening(ts, self.h.susp_damping, spring_out[i], dir[i], cp[i], spd[i]);
            }
        }

        // Contacts, drive, traction (§2.7).
        let brake = self.h.brake_decel * self.brake * ts;
        let fwd_speed = p.move_speed.dot(m.fwd);
        let mut gear = self.gear;
        let (mut revs, mut load) = (0.0, 0.0);
        let accel = if self.engine_on {
            self.h.trans.drive_acceleration_rl(ts, self.gas, &mut gear, fwd_speed, &mut revs, &mut load, self.rear_on_ground, 0, false)
        } else {
            0.0
        };
        self.gear = gear;
        let player = p.status == Status::Player;
        let (b_f, b_r, t_f, t_r) = if player {
            let tf = 2.0 * self.h.traction_bias;
            (2.0 * self.h.brake_bias, 2.0 * (1.0 - self.h.brake_bias), tf, 2.0 - tf)
        } else {
            (1.0, 1.0, 1.0, 1.0)
        };
        self.contact_lines = 0;
        self.rear_on_ground = 0;
        let mut gn = Vec3::ZERO;
        for i in 0..4 {
            let counted = if self.ratio[i] >= 1.0 {
                self.timer[i] = (self.timer[i] - ts).max(0.0);
                self.timer[i] > 0.0
            } else {
                self.timer[i] = 4.0;
                true
            };
            if counted {
                self.contact_lines += 1;
                if i >= 2 {
                    self.rear_on_ground = 1;
                }
                gn += self.wcp[i].normal;
            }
        }
        self.ground_normal = if self.contact_lines == 0 { Vec3::Z } else { gn / self.contact_lines as f32 };
        if m.up.dot(self.ground_normal) < -0.5 {
            self.ground_normal = -self.ground_normal;
        }
        let f_idx = if self.ratio[1] <= self.ratio[0] { 1 } else { 0 };
        let r_idx = if self.ratio[3] <= self.ratio[2] { 3 } else { 2 };
        let cp_f = m.rotate(Vec3::new(
            0.0,
            col.lines[0].start.y,
            col.lines[0].start.z - self.ratio[f_idx].min(1.0) * self.spring_len[0] - self.wheel_size[0] * 0.5,
        ));
        let cp_r = m.rotate(Vec3::new(
            0.0,
            col.lines[3].start.y,
            col.lines[2].start.z - self.ratio[r_idx].min(1.0) * self.spring_len[2] - self.wheel_size[1] * 0.5,
        ));
        let traction = self.h.traction_mult * 0.004 * 0.25;

        // Actual steer angle (§2.9).
        if !player && self.flags & bkf::ON_STAND != 0 && self.flags & bkf::BALANCED == 0 {
            if self.steer_actual < 0.349 {
                self.steer_actual += ts * 0.026_18;
            }
        } else if p.move_speed.x.abs() < 0.01 && p.move_speed.y.abs() < 0.01 && self.steer == 0.0 {
            self.steer_actual *= 0.96f32.powf(ts);
        } else {
            let mut f = 1.0f32;
            if fwd_speed > 0.01 && (self.timer[0] > 0.0 || self.timer[1] > 0.0) && player {
                let tcp = ColPoint { surface_a: 60, surface_b: 1, ..Default::default() };
                let mut x = self.surfaces.adhesive_limit(&tcp) * self.bh.speed_steer * traction * 4.0;
                let g = self.surfaces.adhesion_group(self.wcp[r_idx].surface_b);
                if g == 3 || g == 4 {
                    x *= self.bh.slip_steer;
                }
                x = (x / (fwd_speed * fwd_speed)).min(1.0);
                f = x.asin() / self.h.steering_lock.to_radians();
                if (self.steer < 0.0 && self.lean_render < 0.0) || (self.steer > 0.0 && self.lean_render > 0.0) {
                    f *= 2.0;
                }
                f = f.min(1.0);
            }
            if !player {
                f = 1.0;
            }
            self.steer_actual = f * self.steer;
        }

        let v_saved = p.move_speed;
        // Front wheel (§2.10); the rear-first order flag is not handled.
        if self.timer[0] > 0.0 || self.timer[1] > 0.0 {
            let d = m.rotate(Vec3::new(-self.steer_actual.sin(), self.steer_actual.cos(), 0.0));
            let n = self.wcp[f_idx].normal;
            let fv = (d - n * d.dot(n)).normalize_or_zero();
            let mut rt = fv.cross(n).normalize_or_zero();
            if lying {
                rt.z = 0.0;
            }
            let mut tcp = self.wcp[f_idx];
            tcp.surface_a = 60;
            let mut adh = self.surfaces.adhesive_limit(&tcp) * traction;
            if player {
                adh *= self.surfaces.wet_multiplier(tcp.surface_b);
            }
            if self.wheel_status[0] == 1 {
                adh *= 0.4;
            }
            let cs = p.get_speed(cp_f);
            self.process_bike_wheel(p, ts, fv, rt, cs, cp_f, 0.0, brake * b_f, adh * t_f, 1.0, 0);
            if extra & 4 != 0 && matches!(self.wheel_state[0], WheelState::Spinning | WheelState::Skidding) {
                self.wheel_state[0] = WheelState::Normal;
            }
        } else {
            self.wheel_speed[0] *= 0.95;
            self.wheel_rot[0] += self.wheel_speed[0];
        }
        // Rear wheel, handbrake, burnout (§2.11).
        if self.timer[2] > 0.0 || self.timer[3] > 0.0 {
            let n = self.wcp[r_idx].normal;
            let fv = (m.fwd - n * m.fwd.dot(n)).normalize_or_zero();
            let mut rt = fv.cross(n).normalize_or_zero();
            if lying {
                rt.z = 0.0;
            }
            let mut a_t = traction;
            let mut b_r_ = brake;
            if self.handbrake {
                b_r_ = 20000.0;
                self.grip_recovery = 1.0;
            } else if self.burnout {
                b_r_ = 0.0;
                a_t = 0.0;
                p.apply_turn_force(m.right * (self.steer * p.turn_mass * -0.0007 * ts), cp[2]);
            } else if self.grip_recovery < 1.0 && self.gas > 0.75 {
                a_t = traction * self.grip_recovery;
                p.apply_turn_force(m.right * ((1.0 - self.grip_recovery) * self.steer * p.turn_mass * -0.0007 * ts), cp[2]);
            }
            let mut tcp = self.wcp[r_idx];
            tcp.surface_a = 60;
            let mut adh = self.surfaces.adhesive_limit(&tcp) * a_t;
            if player {
                adh *= self.surfaces.wet_multiplier(tcp.surface_b);
            }
            if self.wheel_status[1] == 1 {
                adh *= 0.4;
            }
            let cs = p.get_speed(cp_r);
            self.process_bike_wheel(p, ts, fv, rt, cs, cp_r, accel, b_r_ * b_r, adh * t_r, 1.0, 1);
            if extra & 4 != 0 && matches!(self.wheel_state[1], WheelState::Spinning | WheelState::Skidding) {
                self.wheel_state[1] = WheelState::Normal;
            }
        } else {
            if self.handbrake {
                self.wheel_speed[1] = 0.0;
            } else if accel > 0.0 {
                if self.wheel_speed[1] < 1.0 {
                    self.wheel_speed[1] -= 0.1;
                }
            } else if accel < 0.0 && self.wheel_speed[1] > -1.0 {
                self.wheel_speed[1] += 0.05;
            }
            self.wheel_rot[1] += ts * self.wheel_speed[1];
        }
        if !self.burnout || self.wheel_state[1] != WheelState::Spinning {
            if self.grip_recovery < 1.0 {
                self.grip_recovery += ts * 0.005;
            }
        } else {
            self.grip_recovery = (self.grip_recovery - ts * 0.002).max(0.0);
        }

        // Lean (§2.13).
        if lying {
            self.ground_normal = m.fwd.cross(Vec3::Z).normalize_or_zero().cross(m.fwd).normalize_or_zero();
        }
        if extra & 2 == 0 && self.flags & bkf::BALANCED == 0 {
            if self.flags & bkf::ON_STAND != 0 {
                let f = 0.97f32.powf(ts);
                self.lean = f * self.lean - (1.0 - f) * (m.right.z.clamp(-1.0, 1.0).asin() + 0.2618);
            } else {
                self.lean *= 0.95f32.powf(ts);
            }
        } else {
            self.ground_right = m.fwd.cross(self.ground_normal).normalize_or_zero();
            let a = if self.contact_lines == 0 {
                self.steer / self.h.steering_lock.to_radians() * ts * -0.004
            } else {
                (p.move_speed - v_saved).dot(self.ground_right)
            };
            let x = a / (ts.max(0.01) * 0.008);
            let mut lim = self.bh.max_lean;
            if self.wheel_status[0] == 1 {
                lim *= 0.4;
            }
            let x = x.clamp(-lim, lim);
            let f = self.bh.des_lean.powf(ts);
            self.lean = x.asin() * (1.0 - f) + f * self.lean;
        }
        self.lean_render = self.lean;
        self.prev_move = p.move_speed;

        // End of frame: ratios reset.
        for i in 0..4 {
            self.prev_ratio[i] = self.ratio[i];
            self.ratio[i] = 1.0;
        }
        // Parked hold.
        if (self.gas == 0.0 || p.status == Status::Wrecked)
            && p.move_speed.x.abs() < 0.005
            && p.move_speed.y.abs() < 0.005
            && p.move_speed.z.abs() < 0.005
        {
            p.turn_speed.z = 0.0;
            p.move_speed = Vec3::ZERO;
        }

        // Upright torque and wheelie / stoppie (§2.15–2.16).
        if extra & 2 != 0 || self.flags & (bkf::ON_STAND | bkf::BALANCED) != 0 {
            let m = p.matrix;
            let s = m.right.dot(self.ground_normal).clamp(-1.0, 1.0);
            let k = if extra & 2 != 0 { -0.07 } else { -0.1 };
            if extra & 2 != 0 {
                self.flags &= !bkf::ON_STAND;
            }
            let at = m.rotate(p.com) + m.right;
            p.apply_turn_force(m.up * (s * p.turn_mass * k * ts), at);
            if p.status == Status::Player {
                let v = p.move_speed.length().min(0.1);
                let pp = m.rotate(p.com) + m.fwd;
                let tm = p.turn_mass;
                if self.timer[0] <= 0.0 && self.timer[1] <= 0.0 && m.fwd.z > 0.0 && (self.timer[2] > 0.0 || self.timer[3] > 0.0) {
                    let d = self.bh.wheelie_ang - m.fwd.z;
                    let d2 = if d > 0.15 {
                        (0.3 - d).max(0.0)
                    } else if d >= -0.08 {
                        d
                    } else if -0.14 - d <= 0.0 {
                        -0.14 - d
                    } else {
                        0.0
                    };
                    p.apply_turn_force(m.up * (d2 * self.bh.wheelie_stab_mult * v * tm * ts * 0.5), pp);
                    p.apply_turn_force(m.right * (self.bh.wheelie_steer * self.steer_actual * tm * ts * 0.5), pp);
                    let vv = p.move_speed.length_squared();
                    p.apply_move_force(m.right * (vv * p.mass * self.bh.wheelie_steer * self.steer_actual * ts * 0.01));
                    self.lean_render += ts * self.steer_actual * -0.1;
                } else if self.timer[2] <= 0.0 && self.timer[3] <= 0.0 && m.fwd.z < 0.0 && (self.timer[0] > 0.0 || self.timer[1] > 0.0) {
                    let d = self.bh.stoppie_ang - m.fwd.z;
                    let d2 = if d > 0.15 {
                        (0.3 - d).max(0.0)
                    } else if d >= -0.15 {
                        d
                    } else if -0.3 - d <= 0.0 {
                        -0.3 - d
                    } else {
                        0.0
                    };
                    p.apply_turn_force(m.up * (d2 * self.bh.stoppie_stab_mult * v * tm * ts * 0.5), pp);
                    let s = m.right.dot(p.move_speed) * tm * ts * 0.05;
                    let a = Vec3::Z.cross(m.right).normalize_or_zero();
                    p.apply_turn_force(m.right * -s, -a);
                }
            }
        }
    }

    /// PreRender (0x6BD090): hub heights and wheel spin.
    fn process_effects(&mut self, _id: EntityId, p: &mut Physical, col: &ColModel, f: &mut FrameFx) {
        let ts = f.ts;
        for w in 0..2 {
            let (a, b) = if w == 0 { (0, 1) } else { (2, 3) };
            let s = self.prev_ratio[a].min(self.prev_ratio[b]);
            let z = col.lines[a].start.z - if s > 0.0 { s * self.spring_len[a] } else { 0.0 };
            self.hub_z[w] += (z - self.hub_z[w]) * 0.75;
        }
        let m = p.matrix;
        if self.timer[0] > 0.0 || self.timer[1] > 0.0 {
            let df = m.rotate(Vec3::new(-self.steer.sin(), self.steer.cos(), 0.0));
            let at = Vec3::new(0.0, (col.lines[0].start.y + col.lines[1].start.y) * 0.5, self.hub_z[0] - self.wheel_size[0] * 0.5);
            let sp = p.get_speed(at);
            self.wheel_speed[0] = -df.dot(sp) / (self.wheel_size[0] * 0.5);
            self.wheel_rot[0] += ts * self.wheel_speed[0];
        }
        if self.timer[2] > 0.0 || self.timer[3] > 0.0 {
            let at = Vec3::new(0.0, (col.lines[2].start.y + col.lines[3].start.y) * 0.5, self.hub_z[1] - self.wheel_size[0] * 0.5);
            let sp = p.get_speed(at);
            self.wheel_speed[1] = match self.wheel_state[1] {
                WheelState::Spinning => -1.1,
                WheelState::Locked => 0.0,
                _ => -m.fwd.dot(sp) / (self.wheel_size[1] * 0.5),
            };
            self.wheel_rot[1] += ts * self.wheel_speed[1];
        }
        let _ = self.height_above_road;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Bike {
    /// The chassis lean: (pitch, roll, drop) of `SetRotateX(|lean|·−0.05); RotateY(lean)` and
    /// `P.z += (1 − cos lean)·colMin.z·0.9`.
    pub fn chassis_lean(&self) -> (f32, f32, f32) {
        let l = self.lean_render;
        (l.abs() * -0.05, l, (1.0 - l.cos()) * self.col_min_z * 0.9)
    }
}
