//! `CObject` damage (object_damage.md §1–§2, §5): object.dat data on the body
//! (SetObjectData), `ObjectDamage` with the change-model / smash / breakable effects,
//! TryToExplode / Explode, the hit / destroy FX, and the damage taken from bullets, melee,
//! explosions and collisions (the callers are in the world code).
//!
//! Not ported: the breakable pieces (BreakManager_c / BreakablePlugin — breakables just
//! vanish), glass (CGlass), ObjectFireDamage, the dummy ↔ object respawn beyond 80 m,
//! special collision responses (doors, hanging objects, pool balls), lamppost tilt ignore.

use glam::Vec3;
use sa_formats::objdat::ObjectPhysics;

use crate::{
    effects::{ExplosionType, FrameFx, WorldRequest},
    physical::{EntityType, Physical, ef, pf},
    world::{BodyLogic, EntityId},
};

/// `CObject` +0x140 flags used here.
pub mod of {
    pub const TRIGGERED: u32 = 0x40;
    pub const LAMPPOST: u32 = 0x100;
    pub const BROKEN: u32 = 0x400;
}

/// A map object with object.dat data (`CObject`).
pub struct ObjectLogic {
    pub info: ObjectPhysics,
    pub model: String,
    /// +0x154.
    pub health: f32,
    /// flags1C & 0x200: draw the damaged atomic.
    pub render_damaged: bool,
    /// flags1C & 0x81 cleared (no collision, invisible).
    pub hidden: bool,
    pub flags: u32,
    pub last_weapon: u8,
    explode: bool,
    /// (FX name, position, normal) to start this frame.
    fx: Vec<(&'static str, Vec3, Option<Vec3>)>,
}

impl ObjectLogic {
    pub fn new(info: ObjectPhysics, model: &str) -> Self {
        let lamppost = matches!(
            model,
            "mtraffic1" | "mtraffic2" | "vgsstriptlights1" | "lamppost1" | "lamppost2" | "lamppost3" | "doublestreetlght1" | "mlamppost"
        );
        Self {
            info,
            model: model.to_string(),
            health: 1000.0,
            render_damaged: false,
            hidden: false,
            flags: if lamppost { of::LAMPPOST } else { 0 },
            last_weapon: 0xFF,
            explode: false,
            fx: Vec::new(),
        }
    }

    /// `SetObjectData` (0x5A2D00) on a fresh static object.
    pub fn setup_physical(&self, p: &mut Physical) {
        let i = &self.info;
        p.mass = i.mass;
        p.turn_mass = i.turn_mass;
        p.air_resistance = i.air_resistance;
        p.elasticity = i.elasticity;
        p.eflags |= ef::IS_STATIC;
        if i.mass >= 99998.0 {
            p.flags = (p.flags & !pf::APPLY_GRAVITY) | pf::DISABLE_COLLISION_FORCE | pf::COLLIDE_AS_STATIC;
            if i.damage_effect == 0 {
                p.flags |= pf::EXPLOSION_PROOF;
            }
        }
        if i.special == 9 {
            p.flags = (p.flags & !pf::APPLY_GRAVITY) | pf::DISABLE_Z;
        }
    }

    /// HIDE: no collision, invisible, static, explosion proof, stopped.
    fn hide(&mut self, p: &mut Physical) {
        self.hidden = true;
        p.eflags &= !ef::USES_COLLISION;
        p.eflags |= ef::IS_STATIC;
        p.flags |= pf::EXPLOSION_PROOF;
        p.move_speed = Vec3::ZERO;
        p.turn_speed = Vec3::ZERO;
    }

    /// `CObject::ObjectDamage` (0x5A0D90). `damager` = (kind, is the player / player's car,
    /// model).
    #[allow(clippy::too_many_arguments)]
    pub fn object_damage(
        &mut self,
        p: &mut Physical,
        dmg: f32,
        pos: Option<Vec3>,
        n: Option<Vec3>,
        damager: Option<(EntityType, bool, u16)>,
        mut wt: u8,
    ) {
        if !p.has_e(ef::USES_COLLISION) {
            return;
        }
        if wt == 55 && damager.is_some_and(|d| d.0 == EntityType::Vehicle) {
            wt = 50;
        }
        // CanPhysicalBeDamaged: explosion proof against explosions.
        if wt == 51 && p.flags & pf::EXPLOSION_PROOF != 0 {
            return;
        }
        let mult = self.info.damage_mult;
        self.health -= dmg * mult;
        if !(self.health > 0.0) {
            self.health = 0.0;
        }
        let effect = self.info.damage_effect;
        if effect == 0 {
            return;
        }
        if self.model == "imy_shash_wall" && !damager.is_some_and(|d| d.2 == 601) {
            return;
        }
        if damager.is_some_and(|d| d.2 == 530) {
            return;
        }
        self.last_weapon = wt;
        let mut changed = false;
        if dmg * mult > 150.0 || self.health == 0.0 {
            match effect {
                1 => {
                    if !self.render_damaged {
                        changed = true;
                    }
                    self.render_damaged = true;
                }
                20 => {
                    self.hide(p);
                    changed = true;
                }
                21 => {
                    if self.render_damaged {
                        self.hide(p);
                        changed = true;
                    } else {
                        self.render_damaged = true;
                    }
                }
                200 | 202 => {
                    // BreakManager_c::Add (pieces not ported), then HIDE.
                    self.hide(p);
                    self.flags |= of::BROKEN;
                    changed = true;
                }
                _ => {}
            }
        }
        if self.hidden {
            self.health = 0.0;
        }
        if changed {
            let exploded = self.try_to_explode();
            if exploded {
                return;
            }
        }
        let Some(name) = self.info.fx_name else { return };
        let fx = self.info.fx_type;
        let play = fx == 3 || (!changed && fx == 1 && dmg > 30.0) || (changed && fx == 2);
        if !play {
            return;
        }
        let o = self.info.fx_offset;
        if o[0] <= -500.0 {
            if let Some(pos) = pos {
                self.fx.push((name, pos, n));
            }
        } else {
            let at = p.matrix.transform(Vec3::from(o));
            self.fx.push((name, at, None));
        }
    }

    /// `TryToExplode` (0x59F2D0).
    pub fn try_to_explode(&mut self) -> bool {
        if self.info.causes_explosion && self.flags & of::TRIGGERED == 0 {
            self.flags |= of::TRIGGERED;
            self.explode = true;
            return true;
        }
        false
    }

    /// Bullets: `CWeapon::DoBulletImpact` object damage (gun break modes).
    pub fn bullet_damage(&self) -> f32 {
        if matches!(self.info.damage_effect, 200 | 202) {
            match self.info.gun_break_mode {
                1 => 151.0,
                2 => self.info.smash_multiplier * 151.0,
                _ => 50.0,
            }
        } else {
            50.0
        }
    }
}

impl BodyLogic for ObjectLogic {
    fn uproot_limit(&self) -> Option<f32> {
        Some(self.info.uproot)
    }

    fn buoyancy(&self, phys: &Physical) -> Option<f32> {
        (self.info.percent_submerged > 0.0).then(|| 100.0 / self.info.percent_submerged * phys.mass * 0.008)
    }

    /// `CObject::Explode` (0x5A1340) and the queued FX.
    fn process_effects(&mut self, id: EntityId, p: &mut Physical, _col: &crate::collision::ColModel, f: &mut FrameFx) {
        if self.explode {
            self.explode = false;
            let mut at = p.matrix.pos;
            at.z += 0.5;
            f.requests.push(WorldRequest::Explosion {
                victim: Some(id),
                creator: None, // FindPlayerPed: credited to the player
                kind: ExplosionType::Object,
                pos: at,
                lifetime_ms: 0,
                cam_shake: -1.0,
                no_damage: false,
            });
            if matches!(self.info.damage_effect, 200 | 202) {
                self.hide(p);
                self.flags |= of::BROKEN;
            } else if p.flags & pf::DISABLE_COLLISION_FORCE == 0 {
                p.move_speed.z += 0.5;
                p.move_speed.x += ((f.rng.next() & 0xFF) as f32 - 128.0) * 0.0002;
                p.move_speed.y += ((f.rng.next() & 0xFF) as f32 - 128.0) * 0.0002;
                p.eflags &= !(ef::IS_STATIC | ef::STATIC_WAITING_FOR_COLLISION);
            }
            if self.info.fx_type == 2 {
                if let Some(name) = self.info.fx_name {
                    let at = p.matrix.transform(Vec3::from(self.info.fx_offset));
                    self.fx.push((name, at, None));
                }
            }
        }
        for (name, at, n) in self.fx.drain(..) {
            let h = match n {
                Some(n) => f.fx.create_dir(name, at, n),
                None => f.fx.create(name, at, None, false),
            };
            f.fx.play_and_kill(h);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl crate::world::World {
    /// The object branches of ApplyCollision / ProcessCollisionSectorList (§5.1, §5.2): the
    /// impact impulse of body `i` on object `j` damages it (`k·imp > 20`, bikes ×3); when the
    /// object is gone, A gets `n·imp / (2·mult)` and passes through. Returns true then.
    pub(crate) fn object_hit(&mut self, i: usize, j: usize, cps: &[crate::colpoint::ColPoint]) -> bool {
        use crate::physical::VehicleClass;
        let Some(ob) = self.body(EntityId::Body(j as u32)) else { return false };
        let Some(o) = ob.logic.as_any().downcast_ref::<ObjectLogic>() else { return false };
        if o.info.damage_effect == 0 || o.hidden {
            return false;
        }
        let mult = o.info.damage_mult;
        let Some(a_body) = self.body(EntityId::Body(i as u32)) else { return false };
        let a = &a_body.phys;
        let mut best: Option<(f32, crate::colpoint::ColPoint, Vec3)> = None;
        for cp in cps {
            let r = cp.point - a.matrix.pos;
            let turn = !a.has(pf::DISABLE_TURN_FORCE);
            let v = if turn { a.get_speed(r) } else { a.move_speed };
            let vn = v.dot(cp.normal);
            if vn >= 0.0 {
                continue;
            }
            let imp = if turn {
                let c = r.cross(cp.normal);
                let m_eff = 1.0 / (1.0 / a.mass + c.length_squared() / a.turn_mass);
                let e = if a.has_e(ef::HAS_HIT_WALL) { 0.0 } else { a.elasticity };
                -(1.0 + e) * m_eff * vn
            } else {
                -vn * a.mass
            };
            if best.is_none_or(|b| imp > b.0) {
                best = Some((imp, *cp, r));
            }
        }
        let Some((imp, cp, r)) = best else { return false };
        let k = if a.vehicle.is_some_and(|v| v.class == VehicleClass::Bike) { 3.0 } else { 1.0 };
        if k * imp <= 20.0 {
            return false;
        }
        let is_player = a.status == crate::physical::Status::Player
            || a_body.logic.as_any().downcast_ref::<crate::ped::PedLogic>().is_some_and(|p| p.is_player);
        let damager = Some((a.kind, is_player, a.vehicle.map_or(0, |v| v.model)));
        let a_static = a.is_static();
        let Some(b) = self.body_mut(EntityId::Body(j as u32)) else { return false };
        let crate::world::Body { phys, logic, .. } = b;
        let Some(o) = logic.as_any_mut().downcast_mut::<ObjectLogic>() else { return false };
        o.object_damage(phys, k * imp, Some(cp.point), Some(cp.normal), damager, 55);
        if !o.hidden {
            return false;
        }
        if !a_static {
            if let Some(ab) = self.body_mut(EntityId::Body(i as u32)) {
                ab.phys.apply_force(cp.normal * imp / (2.0 * mult.max(0.01)), r, true);
            }
        }
        true
    }
}
