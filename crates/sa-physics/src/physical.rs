//! `CPhysical`: the per-body state and the force / integration / response
//! primitives every movable entity uses.
//!
//! Units follow the game: positions in world units (≈ metres, Z up), speeds in
//! units per 1/50 s "frame", angular speed as an axis-angle vector in rad/frame,
//! and `ts` (the timestep) in frames. Operation order mirrors the original
//! where it can change rounding.

use glam::Vec3;

use crate::{Ctx, colpoint::ColPoint};

pub const GRAVITY: f32 = 0.008;

/// Physical flags (CPhysical +0x40).
pub mod pf {
    /// x2 mass factor vs other physicals, x0.75 in ApplySpringCollisionAlt, x2 spring dampening.
    pub const HEAVY: u32 = 0x1;
    pub const APPLY_GRAVITY: u32 = 0x2;
    /// Receives no collision or friction impulses.
    pub const DISABLE_COLLISION_FORCE: u32 = 0x4;
    /// Others take the static collision path against this body.
    pub const COLLIDE_AS_STATIC: u32 = 0x8;
    /// Translation-only response (peds).
    pub const DISABLE_TURN_FORCE: u32 = 0x10;
    pub const DISABLE_MOVE_FORCE: u32 = 0x20;
    /// Pivots about its origin; gravity becomes a torque.
    pub const INFINITE_MASS: u32 = 0x40;
    pub const DISABLE_Z: u32 = 0x80;
    pub const IN_WATER: u32 = 0x100;
    pub const COLLIDED: u32 = 0x200;
    pub const UNK_800: u32 = 0x800;
    pub const UNK_1000: u32 = 0x1000;
    pub const DONT_APPLY_SPEED: u32 = 0x2000;
    pub const IN_SHIFT: u32 = 0x8000;
    /// Collision test only: sector processing returns on the first contact.
    pub const PROBE: u32 = 0x10000;
    pub const NO_COLLISION: u32 = 0x20000;
    pub const VEHICLE_SURFACE_SPEED: u32 = 0x4000000;
    pub const KEEP_COLLISION_RECORDS: u32 = 0x1000_0000;
    pub const DOOR_HIT_LIMIT: u32 = 0x4000_0000;
}

/// Entity flags (CEntity +0x1C) used by the physics.
pub mod ef {
    pub const USES_COLLISION: u32 = 0x1;
    pub const COLLISION_PROCESSED: u32 = 0x2;
    pub const IS_STATIC: u32 = 0x4;
    pub const HAS_CONTACTED: u32 = 0x8;
    pub const IS_STUCK: u32 = 0x10;
    pub const IN_SAFE_POSITION: u32 = 0x20;
    pub const WAS_POSTPONED: u32 = 0x40;
    pub const REMOVE_FROM_WORLD: u32 = 0x800;
    pub const HAS_HIT_WALL: u32 = 0x1000;
    pub const STATIC_WAITING_FOR_COLLISION: u32 = 0x40000;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityType {
    Building,
    Vehicle,
    Ped,
    Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Player,
    Simple,
    Physics,
    Abandoned,
    Wrecked,
    Other(u8),
}

/// Vehicle class (veh+0x590 / +0x594). Only the values the physics branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleClass {
    Automobile,
    MonsterTruck,
    Boat,
    Train,
    Bike,
    Bmx,
    Trailer,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct VehicleInfo {
    pub class: VehicleClass,
    pub model: u16,
    /// Mass of a towed trailer, if any (affects pair mass factors).
    pub towed_mass: Option<f32>,
}

/// Row-vector matrix as stored by the game: right/forward/up axes plus position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub right: Vec3,
    pub fwd: Vec3,
    pub up: Vec3,
    pub pos: Vec3,
}

impl Matrix {
    pub const IDENTITY: Self = Self { right: Vec3::X, fwd: Vec3::Y, up: Vec3::Z, pos: Vec3::ZERO };

    /// `Multiply3x3`: rotate a model-space vector into world axes.
    pub fn rotate(&self, v: Vec3) -> Vec3 {
        self.right * v.x + self.fwd * v.y + self.up * v.z
    }

    /// `Multiply3x4`: model-space point to world.
    pub fn transform(&self, v: Vec3) -> Vec3 {
        self.rotate(v) + self.pos
    }

    /// `CMatrix::Reorthogonalise` (0x59B6A0).
    pub fn reorthogonalise(&mut self) {
        let u = normalise(self.right.cross(self.fwd));
        let r = normalise(self.fwd.cross(u));
        let f = u.cross(r);
        self.right = r;
        self.fwd = f;
        self.up = u;
    }
}

/// `CVector::Normalise`: sets x = 1 on a zero vector.
pub fn normalise(v: Vec3) -> Vec3 {
    let sq = v.z * v.z + v.x * v.x + v.y * v.y;
    if sq > 0.0 { v * (1.0 / sq.sqrt()) } else { Vec3::new(1.0, v.y, v.z) }
}

#[derive(Debug, Clone)]
pub struct Physical {
    pub kind: EntityType,
    pub status: Status,
    pub matrix: Matrix,
    /// Entity flags (`ef`).
    pub eflags: u32,
    /// Physical flags (`pf`).
    pub flags: u32,
    pub move_speed: Vec3,
    pub turn_speed: Vec3,
    pub friction_move: Vec3,
    pub friction_turn: Vec3,
    pub mass: f32,
    pub turn_mass: f32,
    pub air_resistance: f32,
    pub elasticity: f32,
    /// Centre of mass, model space.
    pub com: Vec3,
    pub vehicle: Option<VehicleInfo>,
    /// Bounding-sphere radius of the collision model (used for the
    /// DISABLE_Z planar friction in `apply_friction_accumulated`).
    pub bound_radius: f32,
    // Damage bookkeeping (`SetDamagedPieceRecord`).
    pub damage_intensity: f32,
    /// `m_pEntityIgnoredCollision` (+0x128): no collision with this entity.
    pub ignored: Option<crate::world::EntityId>,
    /// The entity of the last hard contact this frame (collision record 0).
    pub last_hit: Option<crate::world::EntityId>,
    pub damage_piece: u8,
    pub last_collision_pos: Vec3,
    pub last_collision_impact_velocity: Vec3,
    /// Kind of entity that caused `damage_intensity` (m_pDamageEntity's type).
    pub damage_entity_kind: Option<EntityType>,
    /// Distance moved by the last successful ProcessCollision / ProcessShift.
    pub moving_speed: f32,
    /// Static-friction contacts of this frame (ApplyFriction's spark inputs), drained
    /// by the world's effects pass.
    pub scrapes: Vec<Scrape>,
}

/// A static friction contact: what `CPhysical::ApplyFriction` (0x5454C0) feeds its sparks.
#[derive(Debug, Clone, Copy)]
pub struct Scrape {
    pub point: Vec3,
    pub normal: Vec3,
    /// Tangential slip speed (units/tick) and its direction.
    pub slip: f32,
    pub dir: Vec3,
    pub surface_a: u8,
    pub surface_b: u8,
    pub move_speed: Vec3,
}

impl Physical {
    pub fn new(kind: EntityType, matrix: Matrix) -> Self {
        Self {
            kind,
            status: Status::Physics,
            matrix,
            eflags: ef::USES_COLLISION,
            flags: pf::APPLY_GRAVITY,
            move_speed: Vec3::ZERO,
            turn_speed: Vec3::ZERO,
            friction_move: Vec3::ZERO,
            friction_turn: Vec3::ZERO,
            mass: 1.0,
            turn_mass: 1.0,
            air_resistance: 0.1,
            elasticity: 0.0,
            com: Vec3::ZERO,
            vehicle: None,
            bound_radius: 1.0,
            damage_intensity: 0.0,
            ignored: None,
            last_hit: None,
            damage_piece: 0,
            last_collision_pos: Vec3::ZERO,
            last_collision_impact_velocity: Vec3::ZERO,
            damage_entity_kind: None,
            moving_speed: 0.0,
            scrapes: Vec::new(),
        }
    }

    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    pub fn has_e(&self, f: u32) -> bool {
        self.eflags & f != 0
    }

    pub fn is_vehicle(&self) -> bool {
        self.kind == EntityType::Vehicle
    }

    pub fn vclass(&self) -> Option<VehicleClass> {
        self.vehicle.map(|v| v.class)
    }

    /// `CEntity::IsStatic` (0x4633E0).
    pub fn is_static(&self) -> bool {
        self.has_e(ef::IS_STATIC) || self.has_e(ef::STATIC_WAITING_FOR_COLLISION)
    }

    /// Centre of mass in world axes (offset from `pos`); zero for INFINITE_MASS bodies.
    fn com_world(&self) -> Vec3 {
        if self.has(pf::INFINITE_MASS) { Vec3::ZERO } else { self.matrix.rotate(self.com) }
    }

    /// Turn mass including the pivot term used by ApplyForce / friction turn.
    fn turn_mass_pivot(&self) -> f32 {
        if self.has(pf::INFINITE_MASS) {
            (self.com.z * self.mass) * self.com.z * 0.5 + self.turn_mass
        } else {
            self.turn_mass
        }
    }

    /// Effective mass of a contact at offset `r` (already relative to the COM) along `n`.
    pub fn eff_mass(&self, r: Vec3, n: Vec3) -> f32 {
        let x = r.cross(n);
        1.0 / (x.length_squared() / self.turn_mass + 1.0 / self.mass)
    }

    // ------------------------------------------------------------ forces

    /// 0x5429F0
    pub fn apply_move_force(&mut self, mut f: Vec3) {
        if self.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS) != 0 {
            return;
        }
        if self.has(pf::DISABLE_Z) {
            f.z = 0.0;
        }
        let inv = 1.0 / self.mass;
        self.move_speed += f * inv;
    }

    /// 0x542A50. `p` is the offset from the entity position, world axes.
    pub fn apply_turn_force(&mut self, mut f: Vec3, mut p: Vec3) {
        if self.has(pf::DISABLE_TURN_FORCE) {
            return;
        }
        let com = self.com_world();
        if self.has(pf::DISABLE_MOVE_FORCE) {
            f.z = 0.0;
            p.z = 0.0;
        }
        let torque = (p - com).cross(f);
        self.turn_speed += torque * (1.0 / self.turn_mass);
    }

    /// 0x542B50
    pub fn apply_force(&mut self, f: Vec3, p: Vec3, do_turn: bool) {
        let mut fm = f;
        if self.has(pf::DISABLE_Z) {
            fm.z = 0.0;
        }
        if self.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS) == 0 {
            self.move_speed += fm * (1.0 / self.mass);
        }
        if !self.has(pf::DISABLE_TURN_FORCE) && do_turn {
            self.turn_speed += self.turn_delta(f, p);
        }
    }

    /// Shared turn half of ApplyForce / ApplyFrictionTurnForce.
    fn turn_delta(&self, mut f: Vec3, mut p: Vec3) -> Vec3 {
        let i = self.turn_mass_pivot();
        let com = self.com_world();
        if self.has(pf::DISABLE_MOVE_FORCE) {
            f.z = 0.0;
            p.z = 0.0;
        }
        (p - com).cross(f) * (1.0 / i)
    }

    /// 0x5430A0
    pub fn apply_friction_move_force(&mut self, mut f: Vec3) {
        if self.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS) != 0 {
            return;
        }
        if self.has(pf::DISABLE_Z) {
            f.z = 0.0;
        }
        self.friction_move += f * (1.0 / self.mass);
    }

    /// 0x543100
    pub fn apply_friction_turn_force(&mut self, f: Vec3, p: Vec3) {
        if self.has(pf::DISABLE_TURN_FORCE) {
            return;
        }
        self.friction_turn += self.turn_delta(f, p);
    }

    /// 0x543220
    pub fn apply_friction_force(&mut self, f: Vec3, p: Vec3) {
        self.apply_friction_move_force(f);
        self.apply_friction_turn_force(f, p);
    }

    /// 0x542CE0: velocity of the point at offset `p` from `pos` (includes friction accumulators).
    pub fn get_speed(&self, p: Vec3) -> Vec3 {
        let com = self.com_world();
        let w = self.turn_speed + self.friction_turn;
        let c = w.cross(p - com);
        Vec3::new(
            self.move_speed.x + c.x + self.friction_move.x,
            self.move_speed.y + c.y + self.friction_move.y,
            c.z + self.move_speed.z + self.friction_move.z,
        )
    }

    // ------------------------------------------------------------ integration

    /// 0x542DD0
    pub fn apply_move_speed(&mut self, ts: f32) {
        if self.flags & (pf::DONT_APPLY_SPEED | pf::DISABLE_MOVE_FORCE) != 0 {
            self.move_speed = Vec3::ZERO;
        } else {
            self.matrix.pos += self.move_speed * ts;
        }
    }

    /// 0x542E20: first-order rotation about the centre of mass (no re-orthonormalisation).
    pub fn apply_turn_speed(&mut self, ts: f32) {
        if self.has(pf::DONT_APPLY_SPEED) {
            self.turn_speed = Vec3::ZERO;
            return;
        }
        let dw = self.turn_speed * ts;
        let m = &mut self.matrix;
        m.right += dw.cross(m.right);
        m.fwd += dw.cross(m.fwd);
        m.up += dw.cross(m.up);
        if self.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS) == 0 {
            let c = self.matrix.rotate(-self.com);
            self.matrix.pos += dw.cross(c);
        }
    }

    /// 0x547B80 (generic path; the swinging-door and mini-game box cases are not ported).
    pub fn apply_speed(&mut self, ts: f32) {
        self.apply_move_speed(ts);
        self.apply_turn_speed(ts);
    }

    /// 0x542FE0
    pub fn apply_gravity(&mut self, ts: f32) {
        if !self.has(pf::APPLY_GRAVITY) || self.has(pf::DISABLE_MOVE_FORCE) {
            return;
        }
        if self.has(pf::INFINITE_MASS) {
            let c = self.matrix.rotate(self.com);
            self.apply_force(Vec3::new(0.0, 0.0, ts * self.mass * -GRAVITY), c, true);
        } else if self.has_e(ef::USES_COLLISION) {
            self.move_speed.z -= ts * GRAVITY;
        }
    }

    /// 0x544C40. `extra_player_resistance`: CCullZones::DoExtraAirResistanceForPlayer.
    pub fn apply_air_resistance(&mut self, ts: f32, extra_player_resistance: bool) {
        if self.air_resistance > 0.1 && !self.is_vehicle() {
            let f = self.air_resistance.powf(ts);
            self.move_speed *= f;
            self.turn_speed *= f;
        } else {
            let v = self.move_speed;
            let mut drag = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt() * self.air_resistance;
            if extra_player_resistance
                && matches!(self.vclass(), Some(VehicleClass::Automobile) | Some(VehicleClass::Bike))
            {
                drag *= 2.5;
            }
            let f = (1.0 - drag).powf(ts);
            self.move_speed *= f;
            // Not timestep-scaled in the original.
            self.turn_speed *= 0.99;
        }
    }

    /// 0x5483D0: move the friction accumulators into the speeds.
    pub fn apply_friction_accumulated(&mut self, ctx: &Ctx) {
        if self.has(pf::DISABLE_Z) {
            let r = self.bound_radius;
            let cp = ColPoint {
                point: self.matrix.pos - Vec3::new(0.0, 0.0, r),
                normal: Vec3::Z,
                ..Default::default()
            };
            self.apply_friction_static(ctx, ctx.ts * 0.001, &cp);
            self.turn_speed.z *= 0.98f32.powf(ctx.ts);
        }
        self.move_speed += self.friction_move;
        self.turn_speed += self.friction_turn;
        self.friction_move = Vec3::ZERO;
        self.friction_turn = Vec3::ZERO;
        // Abandoned upside-down bike slowdown.
        if self.vclass() == Some(VehicleClass::Bike)
            && self.status == Status::Abandoned
            && self.matrix.up.z.abs() < 0.707
            && self.move_speed.length_squared() < 0.05 * 0.05
            && self.turn_speed.length_squared() < 0.01 * 0.01
        {
            self.move_speed *= 0.5f32.powf(ctx.ts);
        }
    }

    /// 0x5485E0: per-frame physics control (before collision).
    pub fn process_control(&mut self, ctx: &Ctx) {
        if self.kind != EntityType::Ped {
            self.flags &= !pf::IN_WATER;
        }
        self.eflags &= !(ef::HAS_CONTACTED | ef::IN_SAFE_POSITION | ef::WAS_POSTPONED | ef::HAS_HIT_WALL);
        if self.status == Status::Simple {
            return;
        }
        self.flags &= !(pf::COLLIDED | pf::DOOR_HIT_LIMIT);
        self.last_hit = None;
        self.damage_piece = 0;
        self.damage_intensity = 0.0;
        self.damage_entity_kind = None;
        self.apply_friction_accumulated(ctx);
        self.apply_gravity(ctx.ts);
        self.apply_air_resistance(ctx.ts, false);
    }

    /// 0x5433B0
    pub fn skip_physics(&mut self) {
        if self.kind != EntityType::Ped && self.kind != EntityType::Vehicle {
            self.flags &= !pf::IN_WATER;
        }
        self.eflags &= !(ef::HAS_CONTACTED | ef::IN_SAFE_POSITION | ef::WAS_POSTPONED | ef::HAS_HIT_WALL);
        self.flags &= !pf::COLLIDED;
        self.damage_piece = 0;
        self.damage_intensity = 0.0;
        if self.status != Status::Simple {
            self.friction_move = Vec3::ZERO;
            self.friction_turn = Vec3::ZERO;
        }
    }

    /// 0x5428C0 (object surface-65 marking and the mini-game hook omitted).
    pub fn set_damaged_piece_record(&mut self, impulse: f32, cp: &ColPoint, sign: f32, other: Option<EntityType>) {
        if impulse > self.damage_intensity {
            self.damage_intensity = impulse;
            self.damage_entity_kind = other;
            // For B the contact's own piece is piece_b.
            self.damage_piece = if sign < 0.0 { cp.piece_b } else { cp.piece_a };
            self.last_collision_pos = cp.point;
            self.last_collision_impact_velocity = cp.normal * sign;
        }
    }

    // ------------------------------------------------------------ static responses

    /// 0x5435C0 `ApplyCollision(CEntity*, CColPoint&, float&)` against static geometry.
    pub fn apply_collision_static(&mut self, ctx: &Ctx, cp: &ColPoint) -> Option<f32> {
        let n = cp.normal;
        if self.has(pf::DISABLE_TURN_FORCE) {
            let mv = self.move_speed;
            let vn = (mv.z * n.z + mv.y * n.y) + n.x * mv.x;
            if vn >= 0.0 {
                return None;
            }
            let impulse = -vn * self.mass;
            self.apply_move_force(n * impulse);
            return Some(impulse);
        }
        let r = cp.point - self.matrix.pos;
        let v = self.get_speed(r);
        let vn = (n.y * v.y + n.x * v.x) + v.z * n.z;
        if vn >= 0.0 {
            return None;
        }
        // No INFINITE_MASS check here (quirk).
        let com = self.matrix.rotate(self.com);
        let m = self.eff_mass(r - com, n);
        let impulse = -(1.0 + self.elasticity) * m * vn;
        let mut j = n * impulse;
        if self.is_vehicle() && n.z < 0.7 {
            j.z *= 0.3;
        }
        if !self.has(pf::DISABLE_COLLISION_FORCE) {
            // Vehicles only get torque from static hits in the first collision pass.
            let turn = !(self.is_vehicle() && ctx.later_collision_pass);
            self.apply_force(j, r, turn);
        }
        Some(impulse)
    }

    /// 0x544D50 `ApplyCollisionAlt`: accumulates the response into `move_acc` / `turn_acc`.
    /// `other_is_building_or_static`: whether the other entity is a building or flagged static.
    pub fn apply_collision_alt(
        &mut self,
        ctx: &Ctx,
        cp: &ColPoint,
        other_is_building_or_static: bool,
        move_acc: &mut Vec3,
        turn_acc: &mut Vec3,
    ) -> Option<f32> {
        let n = cp.normal;
        if self.has(pf::DISABLE_TURN_FORCE) {
            let mv = self.move_speed;
            let vn = (mv.z * n.z + mv.y * n.y) + n.x * mv.x;
            if vn >= 0.0 {
                return None;
            }
            let impulse = -vn * self.mass;
            self.apply_move_force(n * impulse);
            return Some(impulse);
        }
        let r = cp.point - self.matrix.pos;
        let v = self.get_speed(r);
        // (Surface 65 "moving surface" extra velocity for vehicles not ported.)
        let vn = n.x * v.x + v.z * n.z + n.y * v.y;
        if vn >= 0.0 {
            return None;
        }
        let com = self.com_world();
        let m = self.eff_mass(r - com, n);
        let mut g = ctx.ts * GRAVITY;
        // k: 1 object, 2 upside-down vehicle, 3 abandoned/wrecked bike, 4 boat.
        let mut k = 0;
        if self.kind == EntityType::Object {
            g *= 1.3;
            k = 1;
        } else if self.is_vehicle() && !self.has(pf::IN_WATER) {
            let bike_dead = self.vclass() == Some(VehicleClass::Bike)
                && matches!(self.status, Status::Abandoned | Status::Wrecked);
            if bike_dead {
                g *= 1.7;
                k = 3;
            } else if self.vclass() == Some(VehicleClass::Boat) {
                g *= 1.5;
                k = 4;
            } else if self.matrix.up.z < -0.3 {
                g *= 1.4;
                k = 2;
            }
        }
        let mv = self.move_speed;
        let resting = mv.x.abs() < g && mv.y.abs() < g && mv.z.abs() < 2.0 * g;
        let mut impulse = None;
        if (2..=4).contains(&k) && resting {
            impulse = Some(-0.95 * m * vn);
        }
        // k == 1 (objects): the -0.98 resting impulse is computed and then
        // overwritten by the generic formula in the original, so it is skipped.
        let impulse = impulse.unwrap_or_else(|| {
            if self.vclass() == Some(VehicleClass::Boat) && (cp.surface_b == 43 || n.z < 0.5) {
                -(1.0 + 2.0 * self.elasticity) * m * vn
            } else {
                -(1.0 + self.elasticity) * m * vn
            }
        });
        let mut j = n * impulse;
        if self.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS | pf::DISABLE_Z) != 0 {
            self.apply_force(j, r, true);
        } else {
            let mut dv = j / self.mass;
            if self.is_vehicle() {
                if !self.has_e(ef::HAS_HIT_WALL)
                    || (self.move_speed.length_squared() <= 0.1 && other_is_building_or_static)
                {
                    dv *= 1.2;
                }
                *move_acc += dv;
                j *= 0.8;
            } else {
                *move_acc += dv;
            }
            let c = self.matrix.rotate(self.com);
            *turn_acc += (r - c).cross(j) / self.turn_mass;
        }
        Some(impulse)
    }

    /// 0x543890 `ApplySoftCollision(CEntity*, CColPoint&, float&)` (wheels and stuck contacts).
    pub fn apply_soft_collision_static(&mut self, ctx: &Ctx, cp: &ColPoint) -> Option<f32> {
        if self.has(pf::DISABLE_TURN_FORCE) {
            let _ = self.apply_collision_static(ctx, cp);
        }
        let r = cp.point - self.matrix.pos;
        let v = self.get_speed(r);
        let mut nn = cp.normal;
        let mut k = 0.95;
        if self.vclass() == Some(VehicleClass::MonsterTruck) {
            let d = nn.dot(self.matrix.up);
            if d < -0.9 {
                return None;
            }
            if d < 0.0 {
                nn = normalise(nn - self.matrix.up * d);
            } else if d > 0.5 {
                k = 1.05;
            }
        }
        let vn = (v.y * nn.y + v.z * nn.z) + v.x * nn.x;
        let com = self.com_world();
        let rr = r - com;
        let m = if self.has(pf::DISABLE_MOVE_FORCE) {
            1.0 / (rr.cross(nn).length_squared() / self.turn_mass)
        } else {
            self.eff_mass(rr, nn)
        };
        let mut impulse;
        if self.vclass() == Some(VehicleClass::Automobile) && cp.is_wheel_a() {
            impulse = cp.depth * ctx.ts * 2.0 * m * GRAVITY;
            if vn < 0.0 {
                impulse -= 0.4 * m * vn;
            }
            let right = self.matrix.right;
            nn -= right * (0.9 * nn.dot(right));
        } else {
            impulse = cp.depth.min(0.5) * ctx.ts * 2.0 * m * GRAVITY;
            if vn < 0.0 {
                impulse -= m * vn * k;
            } else {
                return None;
            }
        }
        if impulse == 0.0 {
            return None;
        }
        self.apply_force(nn * impulse, r, true);
        Some(impulse.abs())
    }

    /// 0x5454C0 `ApplyFriction(float adhesion, CColPoint&)` against static geometry.
    pub fn apply_friction_static(&mut self, ctx: &Ctx, adhesion: f32, cp: &ColPoint) -> bool {
        if self.has(pf::DISABLE_COLLISION_FORCE) {
            return false;
        }
        let n = cp.normal;
        if self.has(pf::DISABLE_TURN_FORCE) {
            let mv = self.move_speed;
            let vn = (mv.z * n.z + mv.y * n.y) + mv.x * n.x;
            let vt = mv - n * vn;
            let s = (vt.z * vt.z + vt.y * vt.y + vt.x * vt.x).sqrt();
            if s <= 0.0 {
                return false;
            }
            let dir = vt * (1.0 / s);
            let f = (-s).max(-(ctx.ts / self.mass) * adhesion);
            // z is not applied (quirk).
            self.friction_move.x += f * dir.x;
            self.friction_move.y += f * dir.y;
            return true;
        }
        let r = cp.point - self.matrix.pos;
        let v = self.get_speed(r);
        let vn = (v.y * n.y + v.z * n.z) + v.x * n.x;
        let vt = v - n * vn;
        let s = (vt.z * vt.z + vt.y * vt.y + vt.x * vt.x).sqrt();
        if s <= 0.0 {
            return false;
        }
        let dir = vt / s;
        let com = self.matrix.rotate(self.com);
        let m = self.eff_mass(r - com, dir);
        let imp = (-(m * s)).max(-adhesion);
        self.apply_friction_force(dir * imp, r);
        if s > 0.1 {
            self.scrapes.push(Scrape {
                point: cp.point,
                normal: n,
                slip: s,
                dir,
                surface_a: cp.surface_a,
                surface_b: cp.surface_b,
                move_speed: self.move_speed,
            });
        }
        true
    }

    // ------------------------------------------------------------ springs (wheels)

    /// 0x543C90
    pub fn apply_spring_collision(&mut self, ts: f32, k: f32, dir: Vec3, p: Vec3, ratio: f32, bias: f32) -> f32 {
        let c = 1.0 - ratio;
        if c <= 0.0 {
            return 0.0;
        }
        let f = ((((c * self.mass) * k) * 0.016) * ts.min(3.0)) * bias;
        self.apply_force(-dir * f, p, true);
        f
    }

    /// 0x543D60. `n` may be flipped in place.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_spring_collision_alt(
        &mut self,
        ts: f32,
        k: f32,
        dir: Vec3,
        p: Vec3,
        ratio: f32,
        bias: f32,
        n: &mut Vec3,
    ) -> f32 {
        let c = 1.0 - ratio;
        if c <= 0.0 {
            return 0.0;
        }
        if dir.dot(*n) > 0.0 {
            *n = -*n;
        }
        let mut f = c * (ts.min(3.0) * self.mass) * k * bias * 0.016;
        if self.has(pf::HEAVY) {
            f *= 0.75;
        }
        self.apply_force(*n * f, p, true);
        f
    }

    /// 0x543E90
    pub fn apply_spring_dampening(&mut self, ts: f32, damp: f32, spring_force: f32, dir: Vec3, p: Vec3, speed: Vec3) {
        let a = dir.dot(speed);
        let b = dir.dot(self.get_speed(p));
        let mut x = ts.min(3.0) * damp;
        if self.has(pf::HEAVY) {
            x *= 2.0;
        }
        x = x.clamp(-0.25, 0.25);
        let mut d = -(x * a);
        if d > 0.0 && d + b > 0.0 {
            d = if b < 0.0 { -b } else { 0.0 };
        } else if d < 0.0 && d + b < 0.0 {
            d = if b > 0.0 { -b } else { 0.0 };
        }
        let com = self.matrix.rotate(self.com);
        let mut imp = self.eff_mass(p - com, dir) * d;
        imp = imp.min(spring_force.abs() * 0.999);
        self.apply_force(dir * imp, p, true);
    }
}
