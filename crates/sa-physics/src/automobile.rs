//! `CAutomobile` physics: handling conversion, transmission, suspension lines,
//! `ProcessControl`, `ProcessSuspension`, `ProcessCarWheelPair` and
//! `CVehicle::ProcessWheel`.
//!
//! Wheel order is the game's: 0 front-left, 1 rear-left, 2 front-right, 3 rear-right.
//!
//! Not ported: wheels resting on other moving entities (relative contact
//! speed), high-speed compression damping contacts, hydraulics, towing,
//! buoyancy, nitro, mouse steering and per-model specials.

use std::sync::atomic::{AtomicBool, Ordering};

use glam::Vec3;
use sa_formats::vehicle::Handling as RawHandling;

use crate::{
    Ctx,
    collision::{ColLine, ColModel},
    damage::{CarDamage, init_doors},
    effects::{ExplosionType, FrameFx, FxHandle, WorldRequest},
    fxhelpers::WheelVeh,
    colpoint::ColPoint,
    physical::{Physical, Status, normalise, pf},
    surface::{SURFACE_WHEELBASE, SurfaceInfos},
    world::{BodyLogic, EntityId, LineHits},
};

/// 0xC1CDAC: set once any wheel enters ProcessWheel skidding, never cleared.
static ALREADY_SKIDDING: AtomicBool = AtomicBool::new(false);

const DEG2RAD: f32 = 0.017453292;
const ROLLING_RESISTANCE: f32 = 0.9; // cHandlingDataMgr +0x04

pub mod hflags {
    pub const BOOST_1G: u32 = 0x1;
    pub const BOOST_2G: u32 = 0x2;
    pub const NPC_NEUTRAL_HANDL: u32 = 0x8;
    pub const STEER_REARWHEELS: u32 = 0x20;
    pub const HB_REARWHEEL_STEER: u32 = 0x40;
    pub const NOS_INST: u32 = 0x80000;
    pub const OFFROAD_ABILITY: u32 = 0x100000;
    pub const OFFROAD_ABILITY2: u32 = 0x200000;
    pub const PROC_REARWHEEL_1ST: u32 = 0x800000;
    pub const USE_MAXSP_LIMIT: u32 = 0x1000000;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Gear {
    pub max_vel: f32,
    pub change_up: f32,
    pub change_down: f32,
}

/// `cTransmission`.
#[derive(Debug, Clone, Default)]
pub struct Transmission {
    pub gears: [Gear; 6],
    pub drive_type: char,
    pub num_gears: u8,
    pub flags: u32,
    /// Per driven wheel, game units.
    pub engine_accel: f32,
    pub engine_inertia: f32,
    /// Gearing top speed (1.2 x forward cap normally).
    pub max_velocity: f32,
    pub max_forward: f32,
    pub max_reverse: f32,
}

impl Transmission {
    /// 0x6D0460
    fn init_gear_ratios(&mut self) {
        self.gears = [Gear::default(); 6];
        let n = self.num_gears.max(1) as usize;
        let inv_n = 1.0 / n as f32;
        let f_max = self.max_velocity;
        let base = 0.5 * f_max * inv_n;
        for i in 1..=n.min(5) {
            self.gears[i].max_vel = (i as f32 * (f_max - base)) * inv_n + base;
            let d = self.gears[i].max_vel - self.gears[i - 1].max_vel;
            if i < n {
                if i + 1 < 6 {
                    self.gears[i + 1].change_down = 0.42 * d + self.gears[i - 1].max_vel;
                }
                self.gears[i].change_up = 0.6667 * d + self.gears[i - 1].max_vel;
            } else {
                self.gears[i].change_up = f_max;
            }
        }
        self.gears[0] = Gear { max_vel: self.max_reverse, change_up: -0.01, change_down: self.max_reverse };
        self.gears[1].change_down = -0.01;
    }

    /// 0x6D05E0. Returns per-wheel drive acceleration (units/frame for this step).
    #[allow(clippy::too_many_arguments)]
    fn drive_acceleration(
        &self,
        ts: f32,
        gas: f32,
        gear: &mut u8,
        velocity: f32,
        revs: &mut f32,
        load: &mut f32,
        drive_wheels_on_ground: u8,
        cheat: u8,
    ) -> f32 {
        let v = velocity;
        if v < self.max_reverse {
            return 0.0;
        }
        let (mut use_rl, mut dw, mut cheat) = (true, drive_wheels_on_ground, cheat);
        loop {
            if v > self.max_velocity {
                return 0.0;
            }
            let mut g = *gear as usize;
            if v > self.gears[g].change_up {
                if g == 0 && gas <= 0.0 {
                    break;
                }
                g += 1;
            } else {
                if v >= self.gears[g].change_down || g == 0 || (g == 1 && gas >= 0.0) {
                    break;
                }
                g -= 1;
            }
            *gear = g.min(self.num_gears as usize) as u8;
            use_rl = false;
            dw = 0;
            cheat = 0;
            if v < self.max_reverse {
                return 0.0;
            }
        }
        let n = self.num_gears;
        let gf = if n == 1 {
            1.0
        } else if *gear == 0 {
            4.5
        } else {
            let x = 1.0 - (*gear as f32 - 1.0) / (n as f32 - 1.0);
            let k = if self.flags & hflags::BOOST_1G != 0 {
                5.0
            } else if self.flags & hflags::BOOST_2G != 0 {
                4.0
            } else {
                3.0
            };
            x * x * k + 1.0
        };
        let (c, m) = match cheat {
            1 => (1.2, 1.0),
            2 => (1.0, 2.0),
            _ => (1.0, 1.0),
        };
        let mut acc = ((((c * self.engine_accel) * gf) * m) * 0.4) * gas * ts;
        if use_rl {
            if dw == 0 {
                *revs = (gas.abs() / self.engine_inertia * ts * 0.1 + *revs).min(1.0);
                *load = 0.1;
            } else {
                let g = *gear as usize;
                let b = (1.0 - 0.6667) * (self.max_velocity / n.max(1) as f32);
                let r = match g {
                    0 => (b - v) / (b - self.gears[0].change_down),
                    1 => (v + b) / (b + self.gears[1].change_up),
                    _ => (v - self.gears[g].change_down) / (self.gears[g].change_up - self.gears[g].change_down),
                };
                let mut d = r - *revs;
                if cheat == 1 {
                    d *= 0.75;
                } else if cheat == 2 {
                    d *= 0.5;
                }
                let x = (1.0 - d * self.engine_inertia).clamp(0.1, 1.0);
                let s = (1.0 - 0.85) * x + 0.85 * *load;
                acc *= s;
                *load = s;
                *revs = r;
            }
        }
        let gm = self.gears[*gear as usize].max_vel;
        let over = if gm < 0.0 && v < c * gm {
            c * gm - v
        } else if gm > 0.0 && v > c * gm {
            v - c * gm
        } else {
            return acc;
        };
        let t = (over / 0.05).min(1.0);
        (1.0 - t) * acc
    }
}

/// `tHandlingData` after `ConvertDataToGameUnits` (0x6F5080).
#[derive(Debug, Clone)]
pub struct VehicleHandling {
    pub mass: f32,
    pub turn_mass: f32,
    pub drag_mult: f32,
    pub centre_of_mass: Vec3,
    pub traction_mult: f32,
    pub traction_loss: f32,
    pub traction_bias: f32,
    pub brake_decel: f32,
    pub brake_bias: f32,
    pub steering_lock: f32,
    pub susp_force: f32,
    pub susp_damping: f32,
    pub susp_high_speed_damping: f32,
    pub susp_upper: f32,
    pub susp_lower: f32,
    pub susp_bias: f32,
    pub anti_dive: f32,
    pub model_flags: u32,
    pub flags: u32,
    /// Converted collision damage multiplier (raw * 2000 / mass).
    pub collision_damage: f32,
    /// `fBuoyancyConstant` = mass·0.8 / nPercentSubmerged.
    pub buoyancy_constant: f32,
    /// handling.cfg engine type: 'P' petrol, 'D' diesel, 'E' electric.
    pub engine_type: char,
    pub trans: Transmission,
}

impl VehicleHandling {
    pub fn from_raw(h: &RawHandling) -> Self {
        let k_accel = 1.0f32 / 2500.0;
        let k_vel = 0.277778f32 / 50.0;
        let a = (h.engine_accel * 0.4) * k_accel;
        let vmax = h.max_velocity * k_vel;
        // Drag-limited top speed search (done in double precision on the FPU).
        let (a64, drag_mult) = (a as f64, h.drag_mult as f64);
        let mut v = vmax as f64;
        loop {
            if v <= 0.0 {
                break;
            }
            v -= 0.01;
            let drag = if drag_mult >= 0.01 {
                drag_mult / 1000.0 * 0.5 * v * v
            } else {
                -((1.0 / (v * v * drag_mult + 1.0)) - 1.0) * v
            };
            if !(a64 * (1.0 / 6.0) < drag) {
                break;
            }
        }
        let v = v as f32;
        let mut t = Transmission {
            drive_type: h.drive_type,
            num_gears: h.gears.clamp(1, 5) as u8,
            flags: h.handling_flags,
            engine_inertia: h.engine_inertia,
            ..Default::default()
        };
        let mut rev;
        if h.id == "RCBANDIT" {
            t.max_velocity = vmax;
            t.max_forward = vmax;
            rev = -vmax;
        } else if h.handling_flags & hflags::USE_MAXSP_LIMIT != 0 {
            t.max_velocity = vmax;
            t.max_forward = vmax * 0.8333333;
            rev = t.max_forward * -0.25;
        } else {
            t.max_forward = v;
            t.max_velocity = v * 1.2;
            rev = t.max_forward * -0.3;
        }
        if rev > -0.2 {
            rev = -0.2;
        }
        t.max_reverse = rev;
        t.engine_accel = a * if h.drive_type == '4' { 0.25 } else { 0.5 };
        t.init_gear_ratios();
        Self {
            mass: h.mass,
            turn_mass: h.turn_mass,
            drag_mult: h.drag_mult,
            centre_of_mass: Vec3::from(h.centre_of_mass),
            traction_mult: h.traction_mult,
            traction_loss: h.traction_loss,
            traction_bias: h.traction_bias,
            brake_decel: h.brake_decel * k_accel,
            brake_bias: h.brake_bias,
            steering_lock: h.steering_lock,
            susp_force: h.susp_force,
            susp_damping: h.susp_damping,
            susp_high_speed_damping: h.susp_high_speed_damping,
            susp_upper: h.susp_upper,
            susp_lower: h.susp_lower,
            susp_bias: h.susp_bias,
            anti_dive: h.anti_dive,
            model_flags: h.model_flags,
            flags: h.handling_flags,
            collision_damage: 1.0 / h.mass * h.collision_damage * 2000.0,
            buoyancy_constant: h.mass * 0.8 / h.percent_submerged.max(1.0),
            engine_type: h.engine_type,
            trans: t,
        }
    }

    /// Air resistance as set by the CAutomobile constructor.
    pub fn air_resistance(&self) -> f32 {
        if self.drag_mult > 0.01 { self.drag_mult / 1000.0 * 0.5 } else { self.drag_mult }
    }

    fn front_drive(&self) -> bool {
        self.trans.drive_type != 'R'
    }

    fn rear_drive(&self) -> bool {
        self.trans.drive_type != 'F'
    }
}

/// Driver input (pad). Steering +1 = full left.
#[derive(Debug, Clone, Copy, Default)]
pub struct CarInput {
    pub steer: f32,
    pub accelerate: f32,
    pub brake: f32,
    pub handbrake: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WheelState {
    #[default]
    Normal,
    Spinning,
    Skidding,
    Locked,
}

pub struct Automobile {
    pub h: VehicleHandling,
    pub model: u16,
    pub wheel_size_front: f32,
    pub wheel_size_rear: f32,
    pub spring_len: [f32; 4],
    pub line_len: [f32; 4],
    pub front_height: f32,
    pub hub_z: [f32; 4],
    /// Converted spring compression this frame (1.0 = extended / no contact).
    pub comp: [f32; 4],
    pub comp_prev: [f32; 4],
    pub wheel_cp: [ColPoint; 4],
    pub wheel_timer: [f32; 4],
    pub wheel_state: [WheelState; 4],
    pub wheel_speed: [f32; 4],
    pub wheel_rot: [f32; 4],
    pub steer_in: f32,
    pub steer_angle: f32,
    pub gas: f32,
    pub brake: f32,
    pub handbrake: bool,
    pub burnout: bool,
    pub engine_on: bool,
    /// m_fBuoyancyConstant (+0xA0): handling's, reduced while sinking.
    pub buoyancy: f32,
    /// +0x42B & 0x40: sinking.
    pub sinking: bool,
    pub gear: u8,
    pub revs: f32,
    pub load: f32,
    pub tyre_temp: f32,
    pub num_contact_wheels: u8,
    pub drive_wheels_on_ground: u8,
    pub input: CarInput,
    pub surfaces: SurfaceInfos,
    /// Second steer angle (HB_REARWHEEL_STEER).
    pub steer_angle2: f32,
    pub damage: CarDamage,
    /// Model-space door hinge positions (eDoors order), set by the app from the DFF.
    pub door_hinges: [Option<Vec3>; 6],
    /// Vehicle structure dummies 7 ("engine") and 0 ("headlights"), model space,
    /// (0,0,0) when the DFF lacks them. Set by the app.
    pub engine_pos: Vec3,
    pub headlights_pos: Vec3,
    /// Primary paint colour (ms_vehicleColourTable[+0x434]), set by the app.
    pub colour: [u8; 4],
    /// Head / tail / siren lights (dummies set by the app).
    pub lights: crate::vehicle_lights::CarLights,
    /// +0x578 engine smoke and +0x57C fire_car FX systems.
    smoke_fx: Option<FxHandle>,
    fire_fx: Option<FxHandle>,
    rest_counter: u32,
    avg_move: Vec3,
    avg_turn: Vec3,
    /// Collision box max x (anti-roll lever arm) and box size (collision steps).
    bbox_max_x: f32,
    bbox_size: Vec3,
}

impl Automobile {
    /// `dummies`: model-space wheel dummy positions in game order (FL, RL, FR, RR).
    /// Adds the four suspension lines to `col` (SetupSuspensionLines 0x6A65D0).
    pub fn new(
        h: VehicleHandling,
        model: u16,
        wheel_size_front: f32,
        wheel_size_rear: f32,
        dummies: [Vec3; 4],
        col: &mut ColModel,
        surfaces: SurfaceInfos,
    ) -> Self {
        col.lines.clear();
        let mut spring_len = [0.0; 4];
        let mut line_len = [0.0; 4];
        for (i, p) in dummies.iter().enumerate() {
            let size = if i == 0 || i == 2 { wheel_size_front } else { wheel_size_rear };
            let start = Vec3::new(p.x, p.y, p.z + h.susp_upper);
            let end = Vec3::new(p.x, p.y, start.z + (h.susp_lower - h.susp_upper) - size * 0.5);
            col.lines.push(ColLine { start, end });
            spring_len[i] = h.susp_upper - h.susp_lower;
            line_len[i] = start.z - end.z;
        }
        let eq = 1.0 - 1.0 / (h.susp_force * 4.0);
        let front_height = wheel_size_front * 0.5 - col.lines[0].start.z + eq * spring_len[0];
        let hub_z = [0, 1, 2, 3].map(|i| {
            let size = if i == 0 || i == 2 { wheel_size_front } else { wheel_size_rear };
            size * 0.5 - front_height
        });
        col.bbox_min.z = col.bbox_min.z.min(col.lines[0].end.z);
        col.bound_radius = col.bound_radius.max(col.bbox_min.length()).max(col.bbox_max.length());
        let damage = CarDamage::new(
            h.collision_damage,
            init_doors(h.model_flags, h.model_flags & 0x1 != 0, h.model_flags & 0x2 != 0),
            col.bbox_max.x,
            model as u32 * 7919 + 1,
        );
        let buoyancy0 = h.buoyancy_constant;
        Self {
            h,
            model,
            wheel_size_front,
            wheel_size_rear,
            spring_len,
            line_len,
            front_height,
            hub_z,
            comp: [1.0; 4],
            comp_prev: [1.0; 4],
            wheel_cp: [ColPoint::default(); 4],
            wheel_timer: [0.0; 4],
            wheel_state: [WheelState::Normal; 4],
            wheel_speed: [0.0; 4],
            wheel_rot: [0.0; 4],
            steer_in: 0.0,
            steer_angle: 0.0,
            gas: 0.0,
            brake: 0.0,
            handbrake: false,
            burnout: false,
            engine_on: false,
            buoyancy: buoyancy0,
            sinking: false,
            gear: 1,
            revs: 0.0,
            load: 0.0,
            tyre_temp: 1.0,
            num_contact_wheels: 0,
            drive_wheels_on_ground: 0,
            input: CarInput::default(),
            surfaces,
            rest_counter: 0,
            steer_angle2: 0.0,
            damage,
            door_hinges: [None; 6],
            engine_pos: Vec3::ZERO,
            headlights_pos: Vec3::ZERO,
            colour: [255; 4],
            lights: crate::vehicle_lights::CarLights {
                // m_nRandomSeed: any per-car u16.
                seed: (model as u32).wrapping_mul(40503) as u16,
                halogen: false,
                ..Default::default()
            },
            smoke_fx: None,
            fire_fx: None,
            avg_move: Vec3::ZERO,
            avg_turn: Vec3::ZERO,
            bbox_max_x: col.bbox_max.x,
            bbox_size: col.bbox_max - col.bbox_min,
        }
    }

    /// Configure a Physical for this car (CAutomobile constructor values).
    pub fn setup_physical(&self, p: &mut Physical) {
        p.mass = self.h.mass;
        p.turn_mass = self.h.turn_mass;
        p.com = self.h.centre_of_mass;
        p.elasticity = 0.05;
        p.air_resistance = self.h.air_resistance();
    }

    fn wheel_radius(&self, i: usize) -> f32 {
        (if i == 0 || i == 2 { self.wheel_size_front } else { self.wheel_size_rear }) * 0.5
    }

    /// 0x6AD690 ProcessControlInputs (main, non-mouse path).
    fn process_control_inputs(&mut self, p: &Physical, ts: f32) {
        let fwd_speed = p.move_speed.dot(p.matrix.fwd);
        self.handbrake = self.input.handbrake;
        self.steer_in += (self.input.steer - self.steer_in) * 0.2 * ts;
        self.steer_in = self.steer_in.clamp(-1.0, 1.0);
        let acc = self.input.accelerate - self.input.brake;
        if fwd_speed.abs() >= 0.01 {
            if fwd_speed < 0.0 {
                if acc >= 0.0 {
                    if self.gas <= 0.5 || fwd_speed <= -0.15 {
                        self.gas = 0.0;
                        self.brake = acc;
                    } else {
                        self.gas = acc;
                        self.brake = 0.0;
                    }
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
        } else if self.input.accelerate > 150.0 / 255.0 && self.input.brake > 150.0 / 255.0 {
            self.gas = self.input.accelerate;
            self.brake = self.input.brake;
            self.burnout = true;
        } else {
            self.gas = acc;
            self.brake = 0.0;
        }
        let s = self.steer_in * self.steer_in * self.steer_in.signum();
        self.steer_angle = self.h.steering_lock * DEG2RAD * s;
    }

    /// 0x6AFB10 ProcessSuspension.
    fn process_suspension(&mut self, p: &mut Physical, ts: f32) {
        let fwd_abs = p.move_speed.dot(p.matrix.fwd).abs();
        let mut spring_out = [0f32; 4];
        let mut dir = [-p.matrix.up; 4];
        let mut off = [Vec3::ZERO; 4];
        for i in 0..4 {
            if self.comp[i] < 1.0 {
                off[i] = self.wheel_cp[i].point - p.matrix.pos;
            }
        }
        for i in 0..4 {
            if self.comp[i] < 1.0 {
                let mut bias = self.h.susp_bias;
                if i == 1 || i == 3 {
                    bias = 1.0 - bias;
                }
                let mut n = self.wheel_cp[i].normal;
                spring_out[i] = p.apply_spring_collision_alt(ts, self.h.susp_force, dir[i], off[i], self.comp[i], bias, &mut n);
            }
        }
        let mut spd = [Vec3::ZERO; 4];
        for i in 0..4 {
            spd[i] = p.get_speed(off[i]);
            if self.comp[i] < 1.0 && self.wheel_cp[i].normal.z > 0.35 {
                dir[i] = -self.wheel_cp[i].normal;
            }
        }
        for i in 0..4 {
            if self.comp[i] < 0.99999 {
                p.apply_spring_dampening(ts, self.h.susp_damping, spring_out[i], dir[i], off[i], spd[i]);
            }
        }
        // Anti-roll-over when nearly stopped and lying on one side.
        let (thresh, k_roll) = if p.status == Status::Player { (0.02, 0.0025) } else { (0.04, 0.005) };
        if fwd_abs < thresh {
            let c = &self.comp;
            let s = if c[0] == 1.0 && c[1] == 1.0 && (c[2] < 1.0 || c[3] < 1.0) {
                1.0
            } else if c[2] == 1.0 && c[3] == 1.0 && (c[0] < 1.0 || c[1] < 1.0) {
                -1.0
            } else {
                return;
            };
            let fwd = p.matrix.fwd;
            let x = fwd.cross(Vec3::Z);
            if x.dot(p.matrix.right).abs() < 0.6 {
                let f = k_roll * p.turn_mass * s;
                let up = p.matrix.up;
                let right = p.matrix.right;
                p.apply_turn_force(up * f, right * self.bbox_max_x);
                p.apply_move_force(Vec3::Z.cross(fwd) * (k_roll * p.mass * s));
            }
        }
    }

    /// 0x6A4EC0 ProcessCarWheelPair.
    #[allow(clippy::too_many_arguments)]
    fn process_wheel_pair(
        &mut self,
        p: &mut Physical,
        ts: f32,
        wa: usize,
        wb: usize,
        steer: f32,
        cs: &[Vec3; 4],
        cp: &[Vec3; 4],
        mut traction: f32,
        accel: f32,
        mut brake: f32,
        front: bool,
    ) {
        let driven = if front { self.h.front_drive() } else { self.h.rear_drive() };
        let mut load = 2.0 * self.h.susp_bias;
        if !front {
            load = 2.0 - load;
            if self.handbrake && self.h.flags & hflags::HB_REARWHEEL_STEER == 0 {
                brake = 20000.0;
            } else if driven && self.burnout {
                brake = 0.0;
                traction = 0.0;
                let t = -0.002 * p.turn_mass * (3000.0 / p.turn_mass).min(1.0);
                let right = p.matrix.right;
                p.apply_turn_force(cp[wa], right * (t * self.steer_angle));
            } else if self.h.flags & hflags::NOS_INST == 0 && self.h.rear_drive() {
                traction *= self.tyre_temp;
            }
        }
        if self.wheel_timer[wa] > 0.0 || self.wheel_timer[wb] > 0.0 {
            let steering = steer > -100.0;
            let (s, c) = if steering { steer.sin_cos() } else { (0.0, 1.0) };
            let neutral = p.status != Status::Player && self.h.flags & hflags::NPC_NEUTRAL_HANDL != 0;
            let bb = if neutral { 1.0 } else if front { 2.0 * self.h.brake_bias } else { 2.0 - 2.0 * self.h.brake_bias };
            let tb = if neutral {
                1.0
            } else if front {
                2.0 * self.h.traction_bias
            } else {
                2.0 - 2.0 * self.h.traction_bias
            };
            for w in [wa, wb] {
                if self.wheel_timer[w] <= 0.0 {
                    continue;
                }
                let thrust = if driven { accel } else { 0.0 };
                let n = self.wheel_cp[w].normal;
                let fwd = p.matrix.fwd;
                let mut f = normalise(fwd - n * fwd.dot(n));
                let mut rt = normalise(f.cross(n));
                if steering && self.model != 520 {
                    let f2 = f * c - rt * s;
                    let r2 = f * s + rt * c;
                    f = f2;
                    rt = r2;
                }
                self.wheel_cp[w].surface_a = SURFACE_WHEELBASE;
                let mut adh = self.surfaces.adhesive_limit(&self.wheel_cp[w]) * traction;
                if p.status == Status::Player {
                    let sb = self.wheel_cp[w].surface_b;
                    adh *= self.surfaces.wet_multiplier(sb);
                    adh *= ((1.0 - self.comp[w]) * 4.0 * self.h.susp_force * load).min(2.0);
                    let rough = self.surfaces.adhesion_group(sb) > 2;
                    if self.h.flags & hflags::OFFROAD_ABILITY2 != 0 && rough {
                        adh *= 1.4;
                    } else if self.h.flags & hflags::OFFROAD_ABILITY != 0 && rough {
                        adh *= 1.15;
                    }
                }
                let mut st = self.wheel_state[w];
                // A burst tyre: m_fWheelDamageEffect (0.5) traction and a random side force.
                let burst = self.damage.dm.wheels[w] == 1;
                let adh = if burst { adh * WHEEL_DAMAGE_EFFECT } else { adh };
                let mut rng = std::mem::replace(&mut self.damage.rng, crate::damage::Rand::new(0));
                self.process_wheel(p, ts, f, rt, cs[w], cp[w], thrust, brake * bb, adh * tb, w, &mut st, burst, &mut rng);
                self.damage.rng = rng;
                self.wheel_state[w] = if driven && self.gas < 0.0 && st == WheelState::Spinning { WheelState::Normal } else { st };
            }
        }
        if !front && self.h.flags & hflags::NOS_INST == 0 {
            if self.burnout
                && driven
                && (self.wheel_state[1] == WheelState::Spinning || self.wheel_state[3] == WheelState::Spinning)
            {
                self.tyre_temp = (self.tyre_temp + ts * 0.001).min(3.0);
            } else if self.tyre_temp > 1.0 {
                self.tyre_temp = 0.995f32.powf(ts) * (self.tyre_temp - 1.0) + 1.0;
            }
        }
        for w in [wa, wb] {
            if self.wheel_timer[w] > 0.0 {
                continue;
            }
            if driven && accel != 0.0 {
                if accel > 0.0 {
                    if self.wheel_speed[w] < 1.0 {
                        self.wheel_speed[w] -= 0.1;
                    }
                } else if self.wheel_speed[w] > -1.0 {
                    self.wheel_speed[w] += 0.05;
                }
            } else {
                self.wheel_speed[w] *= 0.95;
            }
            self.wheel_rot[w] += ts * self.wheel_speed[w];
        }
    }

    /// 0x6D6C00 CVehicle::ProcessWheel.
    #[allow(clippy::too_many_arguments)]
    fn process_wheel(
        &self,
        p: &mut Physical,
        ts: f32,
        f: Vec3,
        rt: Vec3,
        cs: Vec3,
        cp: Vec3,
        thrust: f32,
        brake: f32,
        mut adhesion: f32,
        wheel: usize,
        state: &mut WheelState,
        burst: bool,
        rng: &mut crate::damage::Rand,
    ) {
        let n_contact = self.num_contact_wheels.max(1) as f32;
        let mut side = 0.0f32;
        let mut fwd_f = 0.0f32;
        let fwd_speed = (f.y * cs.y + f.x * cs.x) + cs.z * f.z;
        let braking = brake != 0.0;
        let driving = !braking && thrust != 0.0;
        adhesion *= ts;
        let player = p.status == Status::Player;
        if *state != WheelState::Normal {
            adhesion *= self.h.traction_loss;
            ALREADY_SKIDDING.store(true, Ordering::Relaxed);
            if *state == WheelState::Spinning && player {
                adhesion *= 1.0 - self.gas.abs() * 0.2;
            }
        }
        *state = WheelState::Normal;
        let right_speed = (rt.y * cs.y + rt.z * cs.z) + rt.x * cs.x;
        if right_speed != 0.0 {
            side = -(right_speed / n_contact);
            if burst {
                side += fwd_speed.min(0.3) * ((0.13 - -0.13) * rng.rand01() + -0.13);
            }
        }
        if driving {
            fwd_f = thrust;
            side = side.clamp(-adhesion, adhesion);
        } else if fwd_speed != 0.0 {
            let ideal = -(fwd_speed / n_contact);
            let b = if braking {
                brake
            } else if self.gas.abs() < 0.01 {
                let mut b = ROLLING_RESISTANCE / p.mass;
                if p.mass < 500.0 {
                    b *= 0.1;
                } else if self.model == 441 {
                    b *= 0.2;
                }
                b
            } else {
                brake
            };
            if b > adhesion {
                fwd_f = ideal;
                if fwd_speed.abs() > 0.005 {
                    *state = WheelState::Locked;
                }
            } else {
                fwd_f = ideal.clamp(-b, b);
            }
        }
        let sum_sq = side * side + fwd_f * fwd_f;
        if sum_sq > adhesion * adhesion {
            if *state != WheelState::Locked {
                let lim = if fwd_speed > 0.15 && (wheel == 0 || wheel == 2) { 0.6 } else { 0.3 };
                *state = if driving && fwd_f.abs() > lim * adhesion { WheelState::Spinning } else { WheelState::Skidding };
            }
            let loss = if ALREADY_SKIDDING.load(Ordering::Relaxed) {
                1.0
            } else if *state == WheelState::Spinning && player {
                self.h.traction_loss * (1.0 - self.gas.abs() * 0.2)
            } else {
                self.h.traction_loss
            };
            let scale = loss / sum_sq.sqrt() * adhesion;
            fwd_f *= scale;
            side *= scale;
        }
        if fwd_f == 0.0 && side == 0.0 {
            return;
        }
        let total = Vec3::new(side * rt.x + fwd_f * f.x, side * rt.y + fwd_f * f.y, side * rt.z + fwd_f * f.z);
        let mut turn_v = total;
        let anti_dive = self.h.anti_dive > 0.0 && (braking || driving);
        if anti_dive {
            let ad = if braking { self.h.anti_dive } else { self.h.anti_dive * 0.5 };
            turn_v = total - (f * ad) * fwd_f;
        }
        let mag = total.length();
        let tmag = if anti_dive { turn_v.length() } else { mag };
        let move_dir = if mag > 0.0 { total / mag } else { Vec3::new(1.0, total.y, total.z) };
        let turn_dir = if anti_dive {
            if tmag > 0.0 { turn_v / tmag } else { Vec3::new(1.0, turn_v.y, turn_v.z) }
        } else {
            move_dir
        };
        let c = cp.cross(turn_dir);
        let eff = 1.0 / (1.0 / p.mass + (c.x * c.x + c.y * c.y + c.z * c.z) / p.turn_mass);
        p.apply_move_force(move_dir * (p.mass * mag));
        p.apply_turn_force(turn_dir * (eff * tmag), cp);
    }

    /// Rest detection for parked / wrecked cars (step 2). Returns true when physics is skipped.
    fn rest_check(&mut self, p: &mut Physical, ts: f32) -> bool {
        if !matches!(p.status, Status::Abandoned | Status::Wrecked) {
            self.rest_counter = 0;
            return false;
        }
        let (move_t, turn_t, moving_t) =
            if p.status == Status::Wrecked { (0.006, 0.0015, 0.015) } else { (0.003, 0.0009, 0.005) };
        self.avg_move = (self.avg_move + p.move_speed) * 0.5;
        self.avg_turn = (self.avg_turn + p.turn_speed) * 0.5;
        let moving = self.avg_move.length_squared() > (move_t * ts) * (move_t * ts)
            || self.avg_turn.length_squared() > (turn_t * ts) * (turn_t * ts)
            || p.moving_speed >= moving_t;
        if moving {
            self.rest_counter = 0;
            false
        } else {
            self.rest_counter += 1;
            if self.rest_counter > 10 {
                self.rest_counter = 10;
                p.move_speed = Vec3::ZERO;
                p.turn_speed = Vec3::ZERO;
                true
            } else {
                false
            }
        }
    }

}

/// `CDamageManager::m_fWheelDamageEffect` (ctor 0x6B0ADD).
const WHEEL_DAMAGE_EFFECT: f32 = 0.5;

impl Automobile {
    /// 0x6A47F0 CAutomobile::DoBurstAndSoftGroundRatios (run from PreRender in SA, i.e.
    /// between the collision passes and the next ProcessControl's step 9).
    fn do_burst_and_soft_ground_ratios(&mut self, p: &Physical, ts: f32) {
        let fwd_abs = p.move_speed.dot(p.matrix.fwd).abs();
        for i in 0..4 {
            // Wheel status order matches the lines: 0 FL, 1 RL, 2 FR, 3 RR.
            let status = self.damage.dm.wheels[i];
            let ext = (self.line_len[i] - self.spring_len[i]) / self.line_len[i];
            if status == 2 {
                self.comp[i] = 1.0;
            } else if status == 1 {
                let r = (self.damage.rng.next() & 0xFFFF) as f32 * (1.0 / 32768.0);
                if ((r * ((fwd_abs * 40.0) as i32 as u16 as f32 + 98.0)) as i32) < 100 {
                    self.comp[i] = (self.comp[i] + ext * 0.25).min(1.0);
                }
            } else if self.comp[i] < 1.0
                && self.surfaces.adhesion_group(self.wheel_cp[i].surface_b) == 4
                && self.model != 432
            {
                let k = if self.h.flags & hflags::OFFROAD_ABILITY2 != 0 {
                    0.15
                } else if self.h.flags & hflags::OFFROAD_ABILITY != 0 {
                    0.2
                } else {
                    0.3
                };
                let f = ((1.0 - (fwd_abs / 0.3) * 0.7) - self.surfaces.wet_roads * 0.7).max(0.4);
                self.comp[i] = (self.comp[i] + ext * f * k).min(1.0);
            } else if self.comp[i] < 1.0 && self.wheel_cp[i].surface_b == 178 {
                let size = if i == 0 || i == 2 { self.wheel_size_front } else { self.wheel_size_rear };
                let mut q = 1.5 / (size * 0.5);
                if fwd_abs > 0.3 {
                    q *= fwd_abs / 0.3;
                }
                let q = 1.0 / q;
                let mut a = q * self.wheel_rot[i];
                a -= a.floor();
                let mut b = (ts * self.wheel_speed[i] + self.wheel_rot[i]) * q;
                b -= b.floor();
                if (self.wheel_speed[i] > 0.0 && b < a) || (self.wheel_speed[i] < 0.0 && a < b) {
                    self.comp[i] = (self.comp[i] - ext * 0.3).max(0.2);
                }
            }
        }
    }

    /// 0x6A32B0 CAutomobile::BurstTyre. `piece` is a col piece (13 LF, 14 RF, 15 LR,
    /// 16 RR) or a wheel index.
    pub fn burst_tyre(&mut self, p: &mut Physical, piece: u8, apply_forces: bool) -> bool {
        if self.model == 432 || p.status == Status::Wrecked {
            return false;
        }
        let w = match piece {
            13 => 0,
            14 => 2,
            15 => 1,
            16 => 3,
            w => w as usize,
        };
        if w > 3 || self.damage.dm.wheels[w] != 0 {
            return false;
        }
        self.damage.dm.wheels[w] = 1;
        if apply_forces {
            let r1 = self.damage.rng.rand01() * 0.06 - 0.03;
            p.apply_move_force(p.matrix.right * (r1 * p.mass));
            let r2 = self.damage.rng.rand01() * 0.06 - 0.03;
            let (right, fwd) = (p.matrix.right, p.matrix.fwd);
            p.apply_turn_force(right * (r2 * p.turn_mass), fwd);
        }
        true
    }

    /// 0x6D1230 ProcessWheelRotation.
    fn wheel_rotation(state: WheelState, dir: Vec3, speed: Vec3, radius: f32) -> f32 {
        match state {
            WheelState::Spinning => -1.1,
            WheelState::Locked => 0.0,
            _ => -dir.dot(speed) / radius,
        }
    }

    /// Visual wheel work from CAutomobile::PreRender (hub height, spin).
    fn update_wheel_visuals(&mut self, p: &Physical, col: &ColModel, ts: f32) {
        for i in 0..4 {
            let line = col.lines[i];
            let mut z = line.start.z;
            let s = self.comp[i];
            if s > 0.0 {
                z -= s.min(1.0) * self.spring_len[i];
            }
            if self.comp[i] >= 1.0 || z <= self.hub_z[i] {
                z = (z - self.hub_z[i]) * 0.75 + self.hub_z[i];
            }
            self.hub_z[i] = z;
        }
        let front_dir = p.matrix.rotate(Vec3::new(-self.steer_angle.sin(), self.steer_angle.cos(), 0.0));
        for i in 0..4 {
            if self.wheel_timer[i] <= 0.0 {
                continue;
            }
            let dir = if i == 0 || i == 2 { front_dir } else { p.matrix.fwd };
            let cpw = self.wheel_cp[i].point - p.matrix.pos;
            let spd = p.get_speed(cpw);
            self.wheel_speed[i] = Self::wheel_rotation(self.wheel_state[i], dir, spd, self.wheel_radius(i));
            self.wheel_rot[i] += ts * self.wheel_speed[i];
        }
    }
}

impl BodyLogic for Automobile {
    /// The tyre col model of `bIncludeCarTyres` line tests: a sphere per present wheel at
    /// its hub, wheel pieces 13 LF, 14 RF, 15 LR, 16 RR.
    fn tyre_spheres(&self, col: &ColModel) -> Vec<crate::collision::ColSphere> {
        (0..4)
            .filter(|&i| self.damage.dm.wheels[i] != 2 && i < col.lines.len())
            .map(|i| {
                let l = col.lines[i];
                crate::collision::ColSphere {
                    center: Vec3::new(l.start.x, l.start.y, self.hub_z[i]),
                    radius: self.wheel_radius(i),
                    surf: crate::collision::Surf { material: 0, piece: [13, 15, 14, 16][i], lighting: 0 },
                }
            })
            .collect()
    }

    /// 0x6B1880 CAutomobile::ProcessControl (physics path).
    fn process_control(&mut self, p: &mut Physical, col: &mut ColModel, ctx: &Ctx, lines: &LineHits) {
        let ts = ctx.ts;
        self.burnout = false;
        self.surfaces.wet_roads = ctx.wet_roads;

        // ProcessAI: centre of mass and control inputs.
        p.com = self.h.centre_of_mass;
        match p.status {
            Status::Player => {
                self.engine_on = true;
                self.process_control_inputs(p, ts);
            }
            Status::Abandoned => {
                self.gas = 0.0;
                self.steer_angle = 0.0;
                self.handbrake = false;
                self.brake = if p.move_speed.length_squared() < 0.01 { 0.2 } else { 0.0 };
            }
            Status::Wrecked => {
                self.brake = 0.05;
                self.handbrake = true;
                self.gas = 0.0;
                self.steer_angle = 0.0;
            }
            _ => {}
        }
        let skip = self.rest_check(p, ts);

        // Step 4: VehicleDamage (consumes last frame's damage record).
        let is_player = p.status == Status::Player;
        self.damage.vehicle_damage(p, ts, is_player);
        if p.status == Status::Wrecked {
            self.engine_on = false;
            self.gas = 0.0;
        }

        // Wheel line results of the last collision passes (raw line fractions).
        for i in 0..4 {
            self.comp[i] = lines.values[i];
            if lines.values[i] < 1.0 {
                self.wheel_cp[i] = lines.points[i];
            }
        }
        self.do_burst_and_soft_ground_ratios(p, ts);

        if skip {
            p.skip_physics();
        } else {
            p.process_control(ctx);

            // Step 9: line fraction -> spring compression.
            for i in 0..4 {
                let k = 1.0 - self.spring_len[i] / self.line_len[i];
                self.comp[i] = (self.comp[i] - k) / (1.0 - k);
            }
            self.process_suspension(p, ts);

            // Step 11: wheel contact points and speeds.
            let mut cp = [Vec3::ZERO; 4];
            let mut cs = [Vec3::ZERO; 4];
            for i in 0..4 {
                cp[i] = if self.comp[i] < 1.0 {
                    self.wheel_cp[i].point - p.matrix.pos
                } else {
                    p.matrix.rotate(col.lines[i].end)
                };
                cs[i] = p.get_speed(cp[i]);
            }
            let fwd_speed = p.move_speed.dot(p.matrix.fwd);

            // Step 13: brake, timers, contact counts.
            let brake = self.brake * self.h.brake_decel * ts;
            self.num_contact_wheels = 0;
            self.drive_wheels_on_ground = 0;
            for i in 0..4 {
                if self.comp[i] >= 1.0 {
                    let t = self.wheel_timer[i] - ts;
                    self.wheel_timer[i] = if t <= 0.0 { 0.0 } else { t };
                } else {
                    self.wheel_timer[i] = 4.0;
                }
                if self.wheel_timer[i] > 0.0 {
                    self.num_contact_wheels += 1;
                    let front = i == 0 || i == 2;
                    let dt = self.h.trans.drive_type;
                    if dt == '4' || (dt == 'F' && front) || (dt == 'R' && !front) {
                        self.drive_wheels_on_ground += 1;
                    }
                }
            }

            // Step 14: drive.
            let mut gear = self.gear;
            let (mut revs, mut load) = (self.revs, self.load);
            let accel = if self.engine_on {
                self.h.trans.drive_acceleration(
                    ts,
                    self.gas,
                    &mut gear,
                    fwd_speed,
                    &mut revs,
                    &mut load,
                    self.drive_wheels_on_ground,
                    0,
                )
            } else {
                0.0
            };
            (self.gear, self.revs, self.load) = (gear, revs, load);

            // Step 15: traction.
            let traction = self.h.traction_mult * 0.004 * 0.25;

            // Step 16: high-speed steering limit (player only).
            if fwd_speed > 0.01
                && (self.wheel_timer[0] > 0.0 || self.wheel_timer[1] > 0.0)
                && p.status == Status::Player
            {
                let side = p.move_speed.dot(p.matrix.right);
                let probe = ColPoint { surface_a: SURFACE_WHEELBASE, surface_b: 1, ..Default::default() };
                let adh = self.surfaces.adhesive_limit(&probe);
                let x = (adh * traction * 4.0 * 4.0 / (fwd_speed * fwd_speed)).min(1.0);
                let mut f = x.asin() / (self.h.steering_lock * DEG2RAD);
                if (self.steer_angle < 0.0 && side > 0.05) || (self.steer_angle > 0.0 && side < -0.05) || self.handbrake {
                    f = 1.0;
                }
                self.steer_angle *= f.min(1.0);
            }

            // Step 17: wheel pairs.
            let fl = self.h.flags;
            let front_steer = if fl & hflags::STEER_REARWHEELS != 0 { -999.0 } else { self.steer_angle };
            let rear_steer = if fl & hflags::STEER_REARWHEELS != 0 {
                -self.steer_angle
            } else if fl & hflags::HB_REARWHEEL_STEER != 0 {
                self.steer_angle2
            } else {
                -999.0
            };
            let rear_first = fl & hflags::PROC_REARWHEEL_1ST != 0;
            if !rear_first {
                self.process_wheel_pair(p, ts, 0, 2, front_steer, &cs, &cp, traction, accel, brake, true);
            }
            self.process_wheel_pair(p, ts, 1, 3, rear_steer, &cs, &cp, traction, accel, brake, false);
            if rear_first {
                self.process_wheel_pair(p, ts, 0, 2, front_steer, &cs, &cp, traction, accel, brake, true);
            }
        }

        // Step 21: remember compression for the next frame.
        self.comp_prev = self.comp;

        // Step 25: parked-car hold.
        let v = p.move_speed;
        if p.has(pf::DISABLE_COLLISION_FORCE) && p.has(pf::COLLIDE_AS_STATIC) {
            p.move_speed = Vec3::ZERO;
            p.turn_speed = Vec3::ZERO;
            p.friction_move = Vec3::ZERO;
            p.friction_turn = Vec3::ZERO;
        } else if !skip
            && (self.gas == 0.0 || p.status == Status::Wrecked)
            && v.x.abs() < 0.005
            && v.y.abs() < 0.005
            && v.z.abs() < 0.005
            && !p.has(pf::IN_WATER)
            && self.comp_prev.iter().any(|&c| c < 1.0)
        {
            p.move_speed = Vec3::ZERO;
            p.turn_speed.z = 0.0;
        }

        // Step 20: fire and explosion.
        self.damage.process_fire(p, ts);
        // PreRender: swinging doors / bonnet.
        let ok = matches!(p.status, Status::Player | Status::Physics | Status::Abandoned);
        let hinges = self.door_hinges;
        self.damage.process_doors(p, ts, &hinges, ok);

        self.update_wheel_visuals(p, col, ts);
    }

    fn on_remove(&mut self, fx: &mut crate::effects::Effects) {
        for h in [self.smoke_fx.take(), self.fire_fx.take()].into_iter().flatten() {
            fx.kill(h);
        }
    }

    /// BlowUpCar's world side, the FX part of ProcessCarOnFireAndExplode (0x6A7090)
    /// and CVehicle::ProcessEngineSmokeFx (0x6D2A80, from PreRender).
    fn process_effects(&mut self, id: EntityId, p: &mut Physical, col: &ColModel, f: &mut FrameFx) {
        if let Some(culprit) = self.damage.blown_up.take() {
            f.fx.cam_shakes.push((0.4, p.matrix.pos));
            // gFireManager.StartFire(car, culprit, ...) is refused here: the engine
            // status is 250 after FuckCarCompletely (>= 225).
            let lim = 0.75; // cars and quads
            let r = &mut self.damage.rng;
            let mut u = |a: f32, b: f32| a + (b - a) * r.rand01();
            let (bx, by) = (u(-lim, lim) * col.bbox_max.x, u(-lim, lim) * col.bbox_max.y);
            let mut pos = p.matrix.pos + p.matrix.right * bx + p.matrix.fwd * by;
            pos.z -= (r.rand01() + 0.5) * col.bbox_max.z;
            let kind = if matches!(self.model, 564 | 441) { ExplosionType::QuickCar } else { ExplosionType::Car };
            f.requests.push(WorldRequest::Explosion {
                victim: Some(id),
                creator: culprit,
                kind,
                pos,
                lifetime_ms: 0,
                cam_shake: -1.0,
                no_damage: false,
            });
        }

        // VehicleDamage's collision particles.
        if let Some((pos, force)) = self.damage.colliding_particles.take() {
            f.car_colliding_particles(p.matrix.pos, col.bound_radius, p.move_speed, pos, force, true, self.colour, 1.0);
        }
        // PreRender: AddSingleWheelParticles for the four wheels (status PLAYER/SIMPLE/PHYSICS).
        if matches!(p.status, Status::Player | Status::Simple | Status::Physics) && !matches!(self.model, 539 | 441) {
            let v = WheelVeh {
                model: self.model,
                pos: p.matrix.pos,
                move_speed: p.move_speed,
                gas: self.gas,
                subtype: 0,
                lighting: 1.0,
                player_driven: p.status == Status::Player,
            };
            let speed = p.move_speed.length();
            let rear_skid = self.wheel_state[1] == WheelState::Skidding || self.wheel_state[3] == WheelState::Skidding;
            for i in 0..4 {
                let flags = if (i == 0 || i == 2) && !rear_skid { 4 } else { 0 };
                let state = self.wheel_state[i] as u8;
                let status = self.damage.dm.wheels[i];
                let cp = self.wheel_cp[i];
                f.single_wheel_particles(&v, p.matrix.fwd, state, status, self.comp_prev[i], speed, &cp, flags);
            }
        }

        // DoVehicleLights and the special lights (CAutomobile::PreRender).
        self.lights.halogen = self.h.flags & 0x0040_0000 != 0;
        let base_id = match id {
            EntityId::Body(i) => (i as u64 + 1) << 16,
            EntityId::Building(i) => (i as u64 + 1) << 40,
        };
        let lc = crate::vehicle_lights::LightCtx {
            id,
            base_id,
            model: self.model,
            phys: p,
            engine_on: self.engine_on,
            brake: self.brake,
            handbrake: self.handbrake,
            has_driver: p.status == Status::Player,
            light_status: self.damage.dm.lights,
            rear_bumper: self.damage.dm.panels[6],
        };
        crate::vehicle_lights::do_vehicle_lights(&mut self.lights, &lc, f);

        // fire_car at the engine, attached to the car.
        if !self.damage.burning {
            if let Some(h) = self.fire_fx.take() {
                f.fx.kill(h);
            }
        } else if self.fire_fx.is_none() {
            let h = f.fx.create("fire_car", self.engine_pos, Some(id), false);
            f.fx.play(h);
            self.fire_fx = Some(h);
        }
        if let Some(h) = self.fire_fx {
            f.fx.set_vel_add(h, p.move_speed * 50.0);
        }

        // Engine smoke between 650 and 250 health; white to black via the system time.
        let hp = self.damage.health;
        if hp >= 650.0 || hp < 250.0 || p.has(pf::IN_WATER) {
            if let Some(h) = self.smoke_fx.take() {
                f.fx.kill(h);
            }
            return;
        }
        let h = *self.smoke_fx.get_or_insert_with(|| {
            let name = if self.h.engine_type == 'E' { "overheat_car_electric" } else { "overheat_car" };
            let h = f.fx.create(name, self.engine_pos, Some(id), false);
            f.fx.play(h);
            h
        });
        f.fx.set_const_time(h, true, 1.0 - (hp - 250.0) * 0.0025);
        f.fx.set_vel_add(h, p.move_speed * 50.0);
    }

    /// 0x6D0E90 SpecialEntityCalcCollisionSteps.
    fn collision_steps(&self, p: &Physical, ts: f32) -> (u8, bool) {
        if p.has(pf::DISABLE_COLLISION_FORCE) {
            return (1, false);
        }
        let v = p.move_speed;
        let d2 = v.length_squared() * ts * ts;
        if d2 < 0.16 {
            return (1, false);
        }
        let m = if p.status == Status::Player {
            5.0
        } else if d2 <= 0.32 {
            2.5
        } else {
            3.3333
        };
        let steps = (d2.sqrt() * m).ceil().clamp(1.0, 255.0) as u8;
        let m3 = p.matrix;
        let r = [(m3.right, self.bbox_size.x), (m3.fwd, self.bbox_size.y), (m3.up, self.bbox_size.z)]
            .iter()
            .map(|(axis, ext)| v.dot(*axis).abs() * ts / ext.max(0.01))
            .fold(0.0f32, f32::max);
        (steps, r < 1.0)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
