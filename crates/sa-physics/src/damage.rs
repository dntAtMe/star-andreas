//! Car damage: `CDamageManager`, `CAutomobile::VehicleDamage` (collision path),
//! `ApplyDamage`, `CDoor` swinging, fire / BlowUpCar.
//!
//! Visual consequences are reported as `DamageEvent`s for the renderer; the
//! door hinge angles and component states are read directly.
//!
//! Not ported: events/speech/audio, wanted level, bullets/explosions as damage
//! sources, bouncing panels (bumpers just show their `_dam` model), FX.

use glam::Vec3;

use crate::{
    colpoint::ColPoint,
    physical::{EntityType, Matrix, Physical, Status},
};

/// Group damage multipliers (0x8D32A0): bumper, wheel, door, bonnet, boot, panel.
const GROUP_MULT: [f32; 6] = [2.5, 1.25, 3.2, 1.4, 2.5, 2.8];
const APPLY_THRESHOLD: f32 = 150.0;

/// eDoors: 0 bonnet, 1 boot, 2 FL, 3 FR, 4 RL, 5 RR.
pub const DOOR_BONNET: usize = 0;
pub const DOOR_BOOT: usize = 1;
/// ePanels: 0 wing FL, 1 wing FR, 2 RL, 3 RR, 4 windscreen, 5 front bumper, 6 rear bumper.
pub const PANEL_WINDSCREEN: usize = 4;
pub const PANEL_BUMP_FRONT: usize = 5;
pub const PANEL_BUMP_REAR: usize = 6;

/// What a flying component is (SpawnFlyingComponent types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlyingKind {
    Bumper,
    Wheel,
    Door,
    Bonnet,
    Boot,
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DamageEvent {
    /// A door (eDoors index) fell off / was torn off.
    DoorOff(usize, FlyingKind),
    /// A panel (ePanels index) fell off.
    PanelOff(usize, FlyingKind),
    /// Front-left wheel thrown off when the car blew up.
    WheelOff(usize),
    /// The car caught fire.
    OnFire,
    /// The car exploded.
    Exploded,
}

/// `CDamageManager` (car+0x5A0).
#[derive(Debug, Clone, Default)]
pub struct DamageManager {
    pub engine: u8,
    /// 0 ok, 1 burst, 2 missing.
    pub wheels: [u8; 4],
    /// 0 closed ok, 1 open ok, 2 closed damaged, 3 open damaged, 4 missing.
    pub doors: [u8; 6],
    pub lights: [u8; 4],
    /// 0 ok, 1 damaged, 2 very damaged, 3 missing.
    pub panels: [u8; 7],
}

/// `CDoor` (hinge state of a bonnet / boot / door).
#[derive(Debug, Clone, Copy, Default)]
pub struct Door {
    pub open_angle: f32,
    pub closed_angle: f32,
    /// Direction nibble (0 +x, 1 +y, 2 +z, 3 -x, 4 -y, 5 -z) plus behaviour bits.
    pub flags: u16,
    /// Hinge axis: 0 x, 1 y, 2 z (model space).
    pub axis: u8,
    pub state: u8,
    pub angle: f32,
    pub prev_angle: f32,
    pub ang_vel: f32,
}

impl Door {
    /// `CDoor::Open` (0x6F4790).
    pub fn open(&mut self, ratio: f32) {
        self.prev_angle = self.angle;
        if ratio >= 1.0 {
            self.angle = self.open_angle;
            if self.flags & 0x80 == 0 {
                self.state = 1;
            }
        } else {
            self.angle = ratio * self.open_angle;
            if self.angle == 0.0 {
                self.ang_vel = 0.0;
            }
        }
    }

    fn direction(&self, m: &Matrix) -> Vec3 {
        match self.flags & 0xF {
            0 => m.right,
            1 => m.fwd,
            2 => m.up,
            3 => -m.right,
            4 => -m.fwd,
            _ => -m.up,
        }
    }

    fn local_direction(&self) -> Vec3 {
        match self.flags & 0xF {
            0 => Vec3::X,
            1 => Vec3::Y,
            2 => Vec3::Z,
            3 => -Vec3::X,
            4 => -Vec3::Y,
            _ => -Vec3::Z,
        }
    }

    /// Change of the hinge-point velocity since last frame.
    fn hinge_delta(car: &Physical, old_move: Vec3, old_turn: Vec3, r: Vec3) -> Vec3 {
        car.get_speed(r) - (old_move + old_turn.cross(r))
    }

    /// `CDoor::ProcessImpact` (0x6F4540): does the door pop open?
    pub fn process_impact(&self, car: &Physical, old_move: Vec3, old_turn: Vec3, r: Vec3, rand01: f32) -> bool {
        let d = Self::hinge_delta(car, old_move, old_turn, r);
        let dir = self.direction(&car.matrix);
        let c = d.cross(dir);
        let imp = if self.axis == 0 { c.x } else { c.z };
        let t = (rand01 * 0.75 + 0.75) * 0.1;
        if self.open_angle < self.closed_angle { imp < -t } else { imp > t }
    }

    /// `CDoor::Process` (0x6F4040). Returns true when slammed shut (latched).
    pub fn process(&mut self, car: &Physical, ts: f32, old_move: Vec3, old_turn: Vec3, r: Vec3) -> bool {
        let mut d = Self::hinge_delta(car, old_move, old_turn, r);
        d.z += ts * if self.flags & 0x20 != 0 { 0.0016 } else { 0.008 };
        let m = &car.matrix;
        let dl = Vec3::new(d.dot(m.right), d.dot(m.fwd), d.dot(m.up));
        let a = self.local_direction();
        let (s, c) = self.angle.sin_cos();
        let imp = match self.axis {
            0 => dl.cross(Vec3::new(a.x, c * a.y - s * a.z, s * a.y + c * a.z)).x,
            2 => dl.cross(Vec3::new(s * a.x + c * a.y, c * a.x - s * a.y, a.z)).z,
            _ => s,
        };
        if imp.abs() > 0.001 || self.ang_vel.abs() > 0.001 {
            self.ang_vel += imp;
        }
        // Per call, not timestep-scaled (as in the original).
        self.ang_vel *= 0.97;
        let lim = if self.flags & 0x100 != 0 { 0.05 } else { 0.5 };
        self.ang_vel = self.ang_vel.clamp(-lim, lim);
        self.angle += self.ang_vel;
        if self.flags & 0x80 == 0 {
            self.state = 0;
        }
        let opens_positive = self.open_angle >= self.closed_angle;
        if if opens_positive { self.angle > self.open_angle } else { self.angle < self.open_angle } {
            self.angle = self.open_angle;
            self.ang_vel *= -0.8;
            if self.flags & 0x80 == 0 {
                self.state = 1;
            }
            return false;
        }
        if if opens_positive { self.angle < self.closed_angle } else { self.angle > self.closed_angle } {
            self.angle = self.closed_angle;
            if self.flags & 0x10 != 0 && self.flags & 0x80 == 0 && self.ang_vel.abs() > 0.1 {
                self.ang_vel = 0.0;
                self.state = 4;
                return true;
            }
            self.ang_vel *= if self.flags & 0x20 != 0 { -0.9 } else { -0.4 };
            if self.flags & 0x80 == 0 {
                self.state = 2;
            }
        }
        false
    }
}

/// Door table from the CAutomobile constructor (handling modelFlags select variants).
pub fn init_doors(model_flags: u32, is_van: bool, is_bus: bool) -> [Door; 6] {
    let a = 0.942_477_8; // 0.3 pi
    let b = 1.256_637_1; // 0.4 pi
    let p = std::f32::consts::FRAC_PI_2;
    let door = |axis: u8, open: f32, flags: u16| Door { open_angle: open, axis, flags, ..Default::default() };
    let reverse_bonnet = model_flags & 0x10 != 0;
    let bonnet = if reverse_bonnet { door(0, -a, 0x24) } else { door(0, a, 0x21) };
    let boot = if model_flags & 0x20 != 0 {
        door(0, -b, 0x15)
    } else if model_flags & 0x40 != 0 {
        door(0, p, 0x12)
    } else {
        door(0, -a, 0x14)
    };
    [
        bonnet,
        boot,
        door(2, if is_bus { -p } else { -b }, 0x14),
        door(2, if is_bus { p } else { b }, 0x14),
        if is_van { door(2, -p, 0x10) } else { door(2, -b, 0x14) },
        if is_van { door(2, p, 0x13) } else { door(2, b, 0x14) },
    ]
}

/// Small deterministic RNG standing in for the CRT `rand()` (0..=0x7FFF).
#[derive(Debug, Clone)]
pub struct Rand(u32);

impl Rand {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    pub fn next(&mut self) -> u32 {
        // MSVC rand(): x = x*214013 + 2531011; (x >> 16) & 0x7FFF
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        (self.0 >> 16) & 0x7FFF
    }

    pub fn unit(&mut self) -> f32 {
        (self.next() & 0xFFFF) as f32 / 32768.0
    }
}

/// Per-car damage state (lives in the Automobile).
#[derive(Debug, Clone)]
pub struct CarDamage {
    pub dm: DamageManager,
    pub doors: [Door; 6],
    pub health: f32,
    /// Converted collision damage multiplier (raw * 2000 / mass).
    pub collision_mult: f32,
    pub burn_timer_ms: f32,
    pub on_fire: bool,
    pub events: Vec<DamageEvent>,
    /// Velocities saved at the end of the previous frame (car+0x8B4 / +0x8C0).
    pub old_move: Vec3,
    pub old_turn: Vec3,
    /// Damage intensity seen this frame (for the swinging-door pop-open check).
    pub frame_intensity: f32,
    pub rng: Rand,
    /// Bounding-box max x of the collision model (side logic).
    pub bbox_max_x: f32,
}

impl CarDamage {
    pub fn new(collision_mult: f32, doors: [Door; 6], bbox_max_x: f32, seed: u32) -> Self {
        Self {
            dm: DamageManager::default(),
            doors,
            health: 1000.0,
            collision_mult,
            burn_timer_ms: 0.0,
            on_fire: false,
            events: Vec::new(),
            old_move: Vec3::ZERO,
            old_turn: Vec3::ZERO,
            frame_intensity: 0.0,
            rng: Rand::new(seed),
            bbox_max_x,
        }
    }

    fn progress_panel(&mut self, p: usize) -> bool {
        let s = self.dm.panels[p];
        if s == 2 {
            let r = self.rng.next();
            if p == PANEL_WINDSCREEN {
                if r & 1 != 0 {
                    return false;
                }
            } else if r & 7 != 0 {
                return false;
            }
        }
        if s == 3 {
            return false;
        }
        self.dm.panels[p] = s + 1;
        true
    }

    fn progress_door(&mut self, d: usize) -> bool {
        if self.dm.doors[d] == 4 {
            return false;
        }
        let new = match self.dm.doors[d] {
            0 | 1 => {
                self.doors[d].open(0.0);
                2
            }
            2 => 3,
            3 => {
                if self.rng.next() & 7 != 0 {
                    return false;
                }
                4
            }
            s => s,
        };
        self.dm.doors[d] = new;
        true
    }

    /// Visual side of door damage (SetDoorDamage 0x6B1600, simplified: doors can always be damaged).
    fn set_door_damage(&mut self, d: usize) {
        match self.dm.doors[d] {
            2 => {
                let door = &mut self.doors[d];
                door.angle = 0.0;
                door.prev_angle = 0.0;
                door.ang_vel = 0.0;
            }
            4 => {
                let kind = match d {
                    DOOR_BONNET => FlyingKind::Bonnet,
                    DOOR_BOOT => FlyingKind::Boot,
                    _ => FlyingKind::Door,
                };
                self.events.push(DamageEvent::DoorOff(d, kind));
            }
            1 | 3 => {
                if d == DOOR_BONNET {
                    self.doors[0].ang_vel = 0.2;
                }
            }
            _ => {}
        }
    }

    /// `CDamageManager::ApplyDamage` (0x6C24B0), with tComponent numbering.
    pub fn apply_damage(&mut self, component: u8, intensity: f32) -> bool {
        let (group, sub) = match component {
            1..=4 => (1usize, component as usize - 1),
            5 => (3, 0),
            6 => (4, 1),
            7..=10 => (2, component as usize - 5),
            11..=15 => (5, component as usize - 11),
            16 | 17 => (0, component as usize - 11),
            _ => return false,
        };
        let mut x = intensity * GROUP_MULT[group];
        if component == 15 {
            x *= 0.6;
        }
        if x <= APPLY_THRESHOLD {
            return false;
        }
        match group {
            1 => {
                if self.dm.wheels[sub] < 2 {
                    self.dm.wheels[sub] += 1;
                }
            }
            2..=4 => {
                if self.progress_door(sub) {
                    self.set_door_damage(sub);
                }
            }
            0 => {
                if self.progress_panel(sub) && self.dm.panels[sub] == 3 {
                    self.events.push(DamageEvent::PanelOff(sub, FlyingKind::Bumper));
                }
            }
            _ => {
                if sub < 4 {
                    self.dm.lights[sub] = 1;
                }
                if self.progress_panel(sub) && self.dm.panels[sub] == 3 {
                    self.events.push(DamageEvent::PanelOff(sub, FlyingKind::Panel));
                }
            }
        }
        true
    }

    /// `CAutomobile::VehicleDamage` (0x6A7650), collision path.
    pub fn vehicle_damage(&mut self, p: &mut Physical, ts: f32, is_player_car: bool) {
        let dmg = p.damage_intensity;
        let piece = p.damage_piece;
        let min_dmg = p.mass * 0.000_666_666_66 * 25.0;
        let pos = p.last_collision_pos;
        let dir = p.last_collision_impact_velocity;
        self.frame_intensity = dmg;
        if p.matrix.up.z < 0.0 && !is_player_car && p.status != Status::Wrecked {
            self.health = (self.health - ts * 4.0).max(0.0);
        }
        if dmg == 0.0 {
            return self.health_effects();
        }
        if p.damage_entity_kind == Some(EntityType::Building) && p.matrix.up.dot(dir) > 0.6 {
            return; // ground contact from below is harmless
        }
        if dmg <= min_dmg || p.status == Status::Wrecked {
            return self.health_effects();
        }
        let speed2 = p.move_speed.length_squared();
        if p.matrix.up.z > 0.0 || speed2 > 0.3 {
            let d4 = dmg * 4.0;
            let mass = p.mass;
            match piece {
                1 => _ = self.apply_damage(5, d4),
                2 => _ = self.apply_damage(6, d4),
                5..=8 => _ = self.apply_damage(piece + 2, d4),
                9 => _ = self.apply_damage(11, d4),
                10 => _ = self.apply_damage(12, d4),
                19 => _ = self.apply_damage(15, d4),
                3 => {
                    self.apply_damage(16, d4);
                    let side = dir.dot(p.matrix.right);
                    let lat = (pos - p.matrix.pos).dot(p.matrix.right) / self.bbox_max_x.max(0.1);
                    let mut bits = 0u8;
                    if lat > 0.7 || (lat > 0.5 && side < -0.5 && 0.35 * mass < dmg) {
                        bits = 8;
                    } else if lat < -0.7 || (lat < -0.5 && side > 0.5 && 0.35 * mass < dmg) {
                        bits = 4;
                    }
                    if self.dm.panels[PANEL_BUMP_FRONT] >= 1 && 0.3 * mass < dmg {
                        bits |= 1;
                    }
                    if self.dm.panels[PANEL_BUMP_FRONT] >= 2 && 0.2 * mass < dmg {
                        bits |= 2;
                    }
                    if bits & 1 != 0 {
                        self.apply_damage(5, d4);
                    }
                    if bits & 4 != 0 {
                        self.apply_damage(11, d4);
                    }
                    if bits & 8 != 0 {
                        self.apply_damage(12, d4);
                    }
                    if bits & 2 != 0 {
                        self.apply_damage(15, d4);
                    }
                }
                4 => {
                    self.apply_damage(17, d4);
                    if self.dm.panels[PANEL_BUMP_REAR] < 2 {
                        self.apply_damage(6, d4);
                    }
                }
                _ => {}
            }
        }
        // Health loss (§3.5).
        let mut d = (dmg - min_dmg) * self.collision_mult * 0.6;
        if d > 0.0 {
            let old = self.health as i16;
            d *= if is_player_car { 0.5 } else { 0.25 };
            self.health -= d;
            if self.health <= 0.0 && old > 0 {
                self.health = 1.0;
            }
        }
        self.health_effects();
    }

    fn health_effects(&mut self) {
        if self.health < 250.0 && self.dm.engine < 225 {
            self.dm.engine = 225;
            self.burn_timer_ms = 0.0;
        }
    }

    /// PreRender: swinging doors (ProcessSwingingDoor 0x6A9D70). `hinges` are the
    /// model-space hinge positions of the six doors (None if the car lacks them).
    pub fn process_doors(&mut self, p: &Physical, ts: f32, hinges: &[Option<Vec3>; 6], status_ok: bool) {
        for (door, hinge) in [2usize, 3, 4, 5, 0, 1].into_iter().map(|d| (d, hinges[d])) {
            let Some(hinge) = hinge else { continue };
            let mut st = self.dm.doors[door];
            if st != 1 && st != 3 {
                if st >= 3 {
                    continue;
                }
                if !(self.frame_intensity > 100.0 && door >= 1 && status_ok) {
                    continue;
                }
            }
            let r = p.matrix.rotate(hinge);
            if st == 0 || st == 2 {
                let rnd = self.rng.unit();
                if self.doors[door].process_impact(p, self.old_move, self.old_turn, r, rnd) {
                    st += 1;
                    self.dm.doors[door] = st;
                }
            }
            if st == 1 || st == 3 {
                let fwd_speed = p.move_speed.dot(p.matrix.fwd);
                if door == DOOR_BONNET && self.doors[0].flags & 0xF == 1 {
                    self.doors[0].ang_vel += (self.doors[0].angle.sin() + 0.1) * 0.05 * fwd_speed;
                }
                if self.doors[door].process(p, ts, self.old_move, self.old_turn, r) {
                    self.dm.doors[door] = st - 1;
                }
                if door == DOOR_BONNET && self.doors[0].state == 1 && fwd_speed > 0.4 {
                    // Torn off by the wind.
                    self.dm.doors[0] = 4;
                    self.events.push(DamageEvent::DoorOff(0, FlyingKind::Door));
                }
            }
        }
        self.old_move = p.move_speed + p.friction_move;
        self.old_turn = p.turn_speed + p.friction_turn;
    }

    /// ProcessCarOnFireAndExplode (0x6A7090). Returns true when the car blows up this frame.
    pub fn process_fire(&mut self, p: &mut Physical, ts: f32) -> bool {
        let start = self.dm.engine;
        let mut exploded = false;
        if self.health >= 250.0 || p.status == Status::Wrecked {
            self.burn_timer_ms = 0.0;
            self.on_fire = false;
        } else {
            if !self.on_fire {
                self.on_fire = true;
                self.events.push(DamageEvent::OnFire);
            }
            self.burn_timer_ms += ((ts * 0.02 * 1000.0) as u32) as f32;
            if self.burn_timer_ms > 5000.0 {
                self.blow_up(p);
                exploded = true;
            }
        }
        if start > 225 && self.health > 250.0 {
            self.health -= 2.0;
        }
        exploded
    }

    /// BlowUpCar (0x6B3780), physics-relevant part.
    pub fn blow_up(&mut self, p: &mut Physical) {
        p.move_speed.z += 0.13;
        p.status = Status::Wrecked;
        // FuckCarCompletely: one random front/left wheel missing, all doors off, engine dead.
        let w = (self.rng.unit() * 3.0) as usize % 3;
        self.dm.wheels[w] = 2;
        self.dm.engine = 250;
        self.dm.lights = [0; 4];
        for d in 0..6 {
            if self.dm.doors[d] != 4 {
                self.dm.doors[d] = 4;
                let kind = match d {
                    DOOR_BONNET => FlyingKind::Bonnet,
                    DOOR_BOOT => FlyingKind::Boot,
                    _ => FlyingKind::Door,
                };
                self.events.push(DamageEvent::DoorOff(d, kind));
            }
        }
        self.events.push(DamageEvent::WheelOff(0));
        self.health = 0.0;
        self.on_fire = false;
        self.events.push(DamageEvent::Exploded);
    }
}

/// Initial velocity of a flying component (SpawnFlyingComponent 0x6A8580),
/// given the part's world position. Returns (moveSpeed, turnSpeed).
pub fn flying_component_velocity(car: &Physical, part_pos: Vec3, kind: FlyingKind, windscreen: bool) -> (Vec3, Vec3) {
    let mut v = car.move_speed;
    let lifted = matches!(kind, FlyingKind::Bonnet | FlyingKind::Boot) || windscreen;
    if v.z > 0.0 {
        v.z *= 1.5;
    } else if car.matrix.up.z > 0.0 && lifted {
        v.z = 0.04 - 1.5 * v.z;
    } else {
        v.z *= 0.25;
    }
    v.x *= 0.75;
    v.y *= 0.75;
    let mut dir = (part_pos - car.matrix.pos).normalize_or_zero();
    if lifted {
        dir += car.matrix.up;
    }
    // ApplyMoveForce(dir) on a 10 kg object.
    v += dir / 10.0;
    (v, car.turn_speed * 2.0)
}

/// Contact used as a damage record (for tests).
pub fn record(p: &mut Physical, impulse: f32, piece: u8, normal: Vec3, other: EntityType) {
    let cp = ColPoint { point: p.matrix.pos, normal, piece_a: piece, ..Default::default() };
    p.set_damaged_piece_record(impulse, &cp, 1.0, Some(other));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car() -> (Physical, CarDamage) {
        let mut p = Physical::new(EntityType::Vehicle, Matrix::IDENTITY);
        p.mass = 1500.0;
        p.status = Status::Player;
        let mult = 0.72 * 2000.0 / 1500.0;
        (p, CarDamage::new(mult, init_doors(0, false, false), 1.0, 7))
    }

    #[test]
    fn small_bumps_do_nothing() {
        let (mut p, mut d) = car();
        record(&mut p, 20.0, 3, -Vec3::Y, EntityType::Building); // below mass/60 = 25
        d.vehicle_damage(&mut p, 1.0, true);
        assert_eq!(d.dm.panels[PANEL_BUMP_FRONT], 0);
        assert_eq!(d.health, 1000.0);
    }

    #[test]
    fn front_hit_dents_bumper_and_costs_health() {
        let (mut p, mut d) = car();
        record(&mut p, 200.0, 3, -Vec3::Y, EntityType::Building);
        d.vehicle_damage(&mut p, 1.0, true);
        assert_eq!(d.dm.panels[PANEL_BUMP_FRONT], 1);
        // (200 - 25) * 0.96 * 0.6 * 0.5
        assert!((d.health - (1000.0 - 175.0 * 0.96 * 0.6 * 0.5)).abs() < 0.01, "{}", d.health);
        // A second big hit progresses the bumper and the bonnet comes with it.
        record(&mut p, 600.0, 3, -Vec3::Y, EntityType::Building);
        d.vehicle_damage(&mut p, 1.0, true);
        assert_eq!(d.dm.panels[PANEL_BUMP_FRONT], 2);
        assert!(d.dm.doors[DOOR_BONNET] >= 2);
    }

    #[test]
    fn low_health_burns_then_explodes() {
        let (mut p, mut d) = car();
        d.health = 200.0;
        d.health_effects();
        let mut exploded = false;
        for _ in 0..400 {
            exploded |= d.process_fire(&mut p, 1.0);
        }
        assert!(exploded);
        assert_eq!(p.status, Status::Wrecked);
        assert!(d.events.contains(&DamageEvent::Exploded));
    }
}
