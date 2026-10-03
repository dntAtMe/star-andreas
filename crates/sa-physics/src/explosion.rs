//! `CExplosion` (16 slots at 0xC88950): `AddExplosion` (0x736A50), `Update` (0x737620)
//! and `CWorld::TriggerExplosion` / `TriggerExplosionSectorList` (0x56B790 / 0x567750).
//!
//! Not ported: audio, crimes/events/stats, pad rumble, ped damage (peds have no health
//! yet), object damage / exploding objects, glass, the fire hydrant, the molotov ped/car
//! /world ignition helpers, and water checks (there is no water yet).

use glam::Vec3;

use crate::{
    automobile::Automobile,
    collision::{ColBox, ColSphere, process_sphere_box},
    colpoint::ColPoint,
    effects::ExplosionType,
    physical::{EntityType, ef, normalise, pf},
    ped::PedLogic,
    world::{EntityId, World},
};

pub(crate) const MAX_EXPLOSIONS: usize = 16;

#[derive(Debug, Clone, Default)]
pub(crate) struct Explosion {
    kind: i32,
    pos: Vec3,
    radius: f32,
    propagation: f32,
    creator: Option<EntityId>,
    victim: Option<EntityId>,
    /// Expiry in ms, stored as a float like the original.
    expiry: f32,
    dmg_pct: f32,
    /// 0 = free; 1 on creation, +1 per active frame (uint8, wraps).
    pub(crate) active: u8,
    trigger_time: u32,
    force: f32,
    fuel_timer: i32,
    fuel_dir: [Vec3; 3],
    fuel_offset: [f32; 3],
    fuel_speed: [f32; 3],
}

/// Types whose blast shortens running bomb fuses.
fn shortens_fuses(kind: i32) -> bool {
    matches!(kind, 2 | 5 | 8 | 9 | 10)
}

impl World {
    /// `GetRandomNumberInRange(float, float)` (0x41BD90).
    pub(crate) fn rand_range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.rng.rand01()
    }

    /// `CWorld::FindGroundZFor3DCoord` (0x5696C0): buildings straight below `p`.
    pub fn find_ground_z(&mut self, p: Vec3) -> Option<f32> {
        self.line_of_sight(p, Vec3::new(p.x, p.y, -1000.0), true, None).map(|(_, _, cp)| cp.point.z)
    }

    fn engine_dummy(&self, id: EntityId) -> Vec3 {
        self.body(id)
            .and_then(|b| b.logic.as_any().downcast_ref::<Automobile>())
            .map_or(Vec3::ZERO, |a| a.engine_pos)
    }

    /// `CExplosion::AddExplosion` (0x736A50). `cam_shake` -1 = the type's default.
    #[allow(clippy::too_many_arguments)]
    pub fn add_explosion(
        &mut self,
        victim: Option<EntityId>,
        creator: Option<EntityId>,
        kind: ExplosionType,
        pos: Vec3,
        lifetime_ms: u32,
        mut cam_shake: f32,
        no_damage: bool,
    ) -> bool {
        let Some(slot) = self.explosions.iter().position(|e| e.active == 0) else {
            return false;
        };
        let kind = kind as i32;
        let now = self.now_ms;
        let mut e = Explosion {
            kind,
            pos,
            radius: 1.0,
            propagation: 0.5,
            creator,
            victim,
            dmg_pct: 1.0,
            active: 1,
            ..Default::default()
        };
        // Fuel spurt directions; the rand() order matters.
        for k in 0..3 {
            if k == 0 || self.rng.next() < 0x3FFF {
                e.fuel_dir[k].x = 2.0 * self.rng.rand01() - 1.0;
                e.fuel_dir[k].y = 2.0 * self.rng.rand01() - 1.0;
                e.fuel_dir[k].z = self.rng.rand01() * 0.8 + 0.2;
                e.fuel_offset[k] = self.rng.rand01() * 1.5 + 0.5;
                e.fuel_speed[k] = self.rng.rand01() * 10.0 + 20.0;
            }
        }
        e.trigger_time = if lifetime_ms != 0 { now.wrapping_add(lifetime_ms) } else { 0 };

        let (radius, force, extra_ms, fx): (f32, f32, u32, Option<&'static str>) = match kind {
            0 => (9.0, 300.0, 750, Some("explosion_small")),
            1 => (6.0, 0.0, 3000, Some("explosion_molotov")),
            2 => (10.0, 300.0, 750, Some("explosion_small")),
            3 => (10.0, 200.0, 750, Some("explosion_small")),
            4 | 5 => (9.0, 300.0, 4250, Some("explosion_medium")),
            6 | 7 => (25.0, 600.0, 3000, Some("explosion_large")),
            8 | 9 => (10.0, 150.0, 750, None),
            10 => (10.0, 150.0, 750, Some("explosion_large")),
            11 => (3.0, 90.0, 750, Some("explosion_small")),
            _ => (3.0, 90.0, 750, Some("explosion_tiny")),
        };
        if kind == 3 {
            e.dmg_pct = 0.2;
        }
        if !no_damage {
            e.radius = radius;
            e.force = force;
        }
        e.expiry = now.wrapping_add(extra_ms).wrapping_add(lifetime_ms) as f32;

        // FX placement.
        let mut fx_pos = pos;
        if kind == 1 {
            if let Some(gz) = self.find_ground_z(pos + Vec3::new(0.0, 0.0, 3.0)) {
                fx_pos.z = gz;
            }
        }
        if let Some(name) = fx {
            let created = if kind == 4 || kind == 5 {
                // The engine dummy of the victim, attached to it (not the damage centre).
                victim.map(|v| {
                    let off = self.engine_dummy(v);
                    self.effects.create(name, off, Some(v), false)
                })
            } else {
                match victim.and_then(|v| self.body(v).map(|b| (v, b.phys.matrix.pos))) {
                    // Quirk: a world delta used as a local offset of the victim.
                    Some((v, vpos)) => Some(self.effects.create(name, fx_pos - vpos, Some(v), false)),
                    None => Some(self.effects.create(name, fx_pos, None, false)),
                }
            };
            if let Some(h) = created {
                self.effects.play_and_kill(h);
            }
        }
        self.explosions[slot] = e;

        // Ground fires.
        let n = match kind {
            1 => (self.rng.next().wrapping_sub(2)) & 3,
            2 | 3 | 9 => (self.rng.next() + 1) & 3,
            _ => 0,
        };
        for i in 0..n {
            let mut p = pos;
            if i != 0 {
                p.x += self.rng.rand01() * 8.0 - 4.0;
                p.y += self.rng.rand01() * 8.0 - 4.0;
            }
            if let Some(gz) = self.find_ground_z(Vec3::new(p.x, p.y, pos.z + 3.0)) {
                if (gz - pos.z).abs() < 10.0 {
                    let t = (self.rng.rand01() * 0.4 * 7000.0 + 5600.0) as u32;
                    if let Some(f) = self.start_fire_at(Vec3::new(p.x, p.y, gz), creator, t, 3) {
                        self.fires[f].first_generation = i == 0;
                    }
                }
            }
        }

        self.effects.scorches.push(pos);
        let e = &self.explosions[slot];
        if e.force != 0.0 && e.trigger_time == 0 {
            let (r, f, pct) = (e.radius, e.force, e.dmg_pct);
            self.trigger_explosion(pos, r, f, victim, creator, shortens_fuses(kind), pct);
        }
        if cam_shake == -1.0 {
            cam_shake = if kind == 1 { 0.2 } else { 0.6 };
        }
        self.effects.cam_shakes.push((cam_shake, pos));
        true
    }

    /// `CExplosion::Update` (0x737620), once per frame.
    pub(crate) fn update_explosions(&mut self, ts: f32) {
        let now = self.now_ms;
        for i in 0..MAX_EXPLOSIONS {
            if self.explosions[i].active == 0 {
                continue;
            }
            if self.explosions[i].trigger_time != 0 {
                let e = &mut self.explosions[i];
                if now > e.trigger_time {
                    e.trigger_time = 0;
                    if e.force != 0.0 {
                        let e = e.clone();
                        self.trigger_explosion(
                            e.pos,
                            e.radius,
                            e.force,
                            e.victim,
                            e.creator,
                            shortens_fuses(e.kind),
                            e.dmg_pct,
                        );
                    }
                }
                continue;
            }
            let e = &mut self.explosions[i];
            e.radius += ts * e.propagation;
            let remaining = (e.expiry - now as f32) as i32;
            let (kind, pos, victim, creator) = (e.kind, e.pos, e.victim, e.creator);
            match kind {
                4..=6 => {
                    if let Some(v) = victim.and_then(|v| self.body(v).map(|b| b.phys.matrix.pos)) {
                        if self.rng.next() & 0x1F == 0 {
                            let x = self.rand_range(-0.5, 0.5);
                            let y = self.rand_range(-0.5, 0.5);
                            let d = self.rand_range(1.0, 2.0);
                            let mut p = normalise(Vec3::new(x, y, 0.0)) * d + v;
                            p.z += 2.0;
                            self.try_start_fire_at_coors(p, 0, false, 10.0);
                        }
                    }
                    if self.frame & 1 != 0 {
                        self.effects.add_light(pos, 15.0, Vec3::new(1.0, 0.7, 0.5), true);
                    }
                }
                0 | 2 | 3 | 7 | 8 | 9 => {
                    if self.frame & 1 != 0 {
                        self.effects.add_light(pos, 20.0, Vec3::new(1.0, 1.0, 0.5), true);
                    }
                    if kind == 7 {
                        let r = (self.rng.unit() * 100.0) as i32;
                        if let Some((v, vp)) = victim.and_then(|v| self.body(v).map(|b| (v, b.phys.matrix.pos))) {
                            if r < 5 {
                                self.add_explosion(Some(v), creator, ExplosionType::Rocket, vp, 0, -1.0, false);
                            }
                        }
                    }
                }
                _ => {}
            }
            let e = &mut self.explosions[i];
            e.active = if remaining > 0 { e.active.wrapping_add(1) } else { 0 };
            e.fuel_timer -= (ts * 0.02 * -1000.0) as i32;
            if (4..=7).contains(&kind) && (e.fuel_timer as u32) <= 200 {
                let e = e.clone();
                for k in 0..3 {
                    if e.fuel_offset[k] > 0.0 {
                        let d = e.fuel_timer as f32 * 0.001 * e.fuel_speed[k] + e.fuel_offset[k];
                        let h = self.effects.create("explosion_fuel_car", e.pos + e.fuel_dir[k] * d, None, false);
                        self.effects.play_and_kill(h);
                    }
                }
            }
        }
    }

    /// `CWorld::TriggerExplosion` (0x56B790): damage and impulse, applied once.
    /// Bodies are scanned per list (vehicles, peds, objects) instead of per repeat sector.
    #[allow(clippy::too_many_arguments)]
    pub fn trigger_explosion(
        &mut self,
        pos: Vec3,
        radius: f32,
        force: f32,
        victim: Option<EntityId>,
        creator: Option<EntityId>,
        shorten: bool,
        dmg_pct: f32,
    ) {
        let ts = self.last_ts;
        for kind in [EntityType::Vehicle, EntityType::Ped, EntityType::Object] {
            for id in self.body_ids() {
                let Some(b) = self.body_mut(id) else { continue };
                if b.phys.kind != kind {
                    continue;
                }
                let d = b.phys.matrix.pos - pos;
                let dist = d.length();
                if !(dist < radius) {
                    continue;
                }
                let p = &mut b.phys;
                if kind == EntityType::Ped && !p.has_e(ef::USES_COLLISION) {
                    continue; // in a vehicle
                }
                let f = (2.0 * (radius - dist) / radius).min(1.0);
                if p.is_static() {
                    if kind == EntityType::Object {
                        let limit = b.logic.uproot_limit().unwrap_or(f32::MAX);
                        if !p.has(pf::DISABLE_COLLISION_FORCE) && force > limit {
                            p.eflags &= !(ef::IS_STATIC | ef::STATIC_WAITING_FOR_COLLISION);
                        }
                    } else if p.has_e(ef::USES_COLLISION) {
                        p.eflags &= !(ef::IS_STATIC | ef::STATIC_WAITING_FOR_COLLISION);
                    }
                }
                if p.is_static() || !p.has_e(ef::USES_COLLISION) {
                    continue;
                }
                let imp_mass = p.mass * (1.0 / 1400.0) * f * force;
                let inv = 1.0 / dist.max(0.01);
                let mut dir = Vec3::new(d.x * inv, d.y * inv, (d.z * inv).max(0.0));
                match kind {
                    EntityType::Vehicle => {
                        if let Some(car) = b.logic.as_any_mut().downcast_mut::<Automobile>() {
                            car.damage.inflict_damage(&mut b.phys, creator, true, f * dmg_pct * 1100.0);
                            if shorten && car.damage.bomb_timer_ms > 0 {
                                car.damage.bomb_timer_ms = car.damage.bomb_timer_ms / 10 + 1;
                            }
                        }
                        let p = &mut b.phys;
                        // Push at the nearest point of the bounding box.
                        let local = p.matrix.inverse().transform(pos);
                        let sphere = ColSphere { center: local, radius: dist, ..Default::default() };
                        let bbox = ColBox { min: b.col.bbox_min, max: b.col.bbox_max, ..Default::default() };
                        let mut cp = ColPoint::default();
                        let mut min_dist = 100_000.0;
                        if process_sphere_box(&sphere, &bbox, &mut cp, &mut min_dist) {
                            let off = p.matrix.rotate(cp.point);
                            let mut n = p.matrix.rotate(-cp.normal);
                            if n.z < -0.2 {
                                n.z = -0.2;
                            } else if n.z > 0.0 && n.z < 0.2 {
                                n.z += 0.2;
                            }
                            let mut f2 = (2.0 * (radius - (p.matrix.pos + off - pos).length()) / radius).min(1.0);
                            if victim == Some(id) {
                                f2 *= 0.2;
                            }
                            let mut imp = (p.mass / 3000.0).min(1.0) * f2 * force;
                            let mom = n.dot(p.get_speed(off)) * p.mass;
                            if mom > 3.0 * imp {
                                imp = (imp - mom).max(0.0);
                            }
                            if !p.has(pf::DISABLE_COLLISION_FORCE) {
                                p.apply_force(n * imp, off, true);
                            }
                        }
                    }
                    EntityType::Ped => {
                        let mut m = imp_mass.min(p.mass * 0.25);
                        let mom = dir.dot(p.move_speed) * p.mass;
                        if mom > 2.0 * m {
                            m = (m - mom).max(0.0);
                        }
                        let mut fz = dir.z * m;
                        let standing = b.logic.as_any_mut().downcast_mut::<PedLogic>().filter(|l| l.standing);
                        if let Some(l) = standing {
                            fz += 4.0;
                            l.standing = false;
                        } else {
                            fz += ts * b.phys.mass * 0.008;
                        }
                        b.phys.apply_move_force(Vec3::new(dir.x * m, dir.y * m, fz));
                    }
                    _ => {
                        if !p.has(pf::DISABLE_COLLISION_FORCE | pf::DISABLE_Z) {
                            if dir.z < 0.1 {
                                dir.z = 0.2;
                            }
                            let mut m = imp_mass;
                            let mom = dir.dot(p.move_speed) * p.mass;
                            if mom > 4.0 * m {
                                m = (m - mom).max(0.0);
                            }
                            dir *= m;
                            p.apply_move_force(dir);
                            let k = (p.turn_mass / p.mass).min(1.0);
                            let arm = Vec3::new(0.0, 0.0, b.col.bound_radius * 0.5);
                            p.apply_turn_force(dir * k, arm);
                        }
                    }
                }
            }
        }
    }
}
