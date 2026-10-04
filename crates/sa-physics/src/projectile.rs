//! `CProjectileInfo` (thrown weapons and rockets), `CWeapon::FireProjectile` (0x741360),
//! `CWorld::UseDetonator` (0x5660B0), `CWeapon::FireAreaEffect` (0x73E800) and `CShotInfo`
//! (flamethrower, spraycan, extinguisher particles).
//!
//! Not ported: heat-seeker lock-on and homing (a heat-seeker fired without a lock is a
//! plain rocket in SA too), flares and the freefall bomb, tear gas choking (no ped damage
//! yet), spray tags, satchels sticking to moving vehicles (they stop where they hit), AI
//! throws, vehicle-mounted launchers.

use std::collections::HashMap;

use glam::Vec3;

use crate::{
    collision::{ColModel, ColSphere, Surf},
    effects::{ExplosionType, FxHandle, PrtMult},
    physical::{EntityType, Matrix, Physical, ef, pf},
    weapon::{WeaponInfo, wf, wt},
    world::{BodyLogic, EntityId, LosOpts, World},
};

/// Projectile types (eWeaponType values).
pub mod ptype {
    pub const GRENADE: u32 = 16;
    pub const TEARGAS: u32 = 17;
    pub const MOLOTOV: u32 = 18;
    pub const ROCKET: u32 = 19;
    pub const ROCKET_HS: u32 = 20;
    pub const SATCHEL: u32 = 39;
}

/// `CProjectileInfo` (0x24 bytes, 32 at 0xC891A8).
#[derive(Debug, Clone, Default)]
pub struct ProjectileInfo {
    pub ty: u32,
    pub creator: Option<EntityId>,
    /// 0 = never.
    pub destroy_ms: u32,
    pub active: bool,
    pub last_pos: Vec3,
    pub fx: Option<FxHandle>,
    pub body: Option<EntityId>,
}

/// The `CProjectile` object's logic: plain physics plus the model to draw.
pub struct ProjectileLogic {
    pub model: i32,
    /// The model's bound radius.
    pub radius: f32,
    /// Uses object info type 5 (grenade, tear gas, satchel).
    pub info5: bool,
}

impl BodyLogic for ProjectileLogic {
    /// `CObject::SpecialEntityCalcCollisionSteps` (0x5A02E0), the object-info-type-5 branch:
    /// `ceil(|v|·ts / boundRadius)` steps once a step moves farther than the bound radius.
    fn collision_steps(&self, p: &Physical, ts: f32) -> (u8, bool) {
        if !self.info5 {
            return (1, false);
        }
        let d = p.move_speed.length() * ts;
        if d < self.radius {
            return (1, false);
        }
        ((d / self.radius).ceil().min(255.0) as u8, false)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// `CShotInfo` (0x2C bytes, 100 at 0xC89690).
#[derive(Debug, Clone, Copy, Default)]
pub struct ShotInfo {
    pub ty: u32,
    pub pos: Vec3,
    pub vel: Vec3,
    pub radius: f32,
    pub creator: Option<EntityId>,
    pub destroy_ms: f32,
    pub active: bool,
    pub hit: bool,
}

/// The weapons' projectile state in the world.
#[derive(Debug, Clone)]
pub struct Projectiles {
    pub infos: Vec<ProjectileInfo>,
    pub shots: Vec<ShotInfo>,
    /// Weapon model id → (bound centre, bound radius) of its geometry (set by the app).
    pub model_bounds: HashMap<i32, (Vec3, f32)>,
    /// Area-effect weapon FX per creator (`CWeapon+0x18`).
    pub weapon_fx: HashMap<EntityId, (FxHandle, u32, u32)>,
}

impl Default for Projectiles {
    fn default() -> Self {
        Self {
            infos: vec![ProjectileInfo { ty: 16, ..Default::default() }; 32],
            shots: vec![ShotInfo { ty: 22, radius: 1.0, ..Default::default() }; 100],
            model_bounds: HashMap::new(),
            weapon_fx: HashMap::new(),
        }
    }
}

/// `ms_afRandTable[20]`: -0.05 + i * 0.005.
fn shot_rand(r: u32) -> f32 {
    -0.05 + (r % 20) as f32 * 0.005
}

fn heading_of(m: &Matrix) -> f32 {
    (-m.fwd.x).atan2(m.fwd.y)
}

/// `SetRotateZOnly(h)` + translation.
fn rot_z(h: f32, pos: Vec3) -> Matrix {
    let (s, c) = h.sin_cos();
    Matrix { right: Vec3::new(c, s, 0.0), fwd: Vec3::new(-s, c, 0.0), up: Vec3::Z, pos }
}

impl World {
    fn info(&self, ty: u32) -> Option<WeaponInfo> {
        self.weapon_infos.as_ref().map(|i| i.get(ty, 1).clone())
    }

    /// `CWeapon::FireProjectile` (0x741360). `effect` is the hand / muzzle point; `cam` the
    /// player's camera (front, up) for rockets. Returns false when nothing was fired.
    pub fn fire_projectile(&mut self, owner: EntityId, ty: u32, effect: Vec3, force: f32, cam: Option<(Vec3, Vec3)>) -> bool {
        let Some(om) = self.body(owner).map(|b| b.phys.matrix) else { return false };
        let mut effect = effect;
        let (start, spawn, pt) = if ty == wt::RLAUNCHER || ty == wt::RLAUNCHER_HS {
            let start = effect;
            match cam {
                Some((front, _)) => effect += front,
                None => effect += om.fwd,
            }
            (start, effect, ptype::ROCKET)
        } else {
            let fwd = om.fwd;
            let d = (effect - om.pos).dot(fwd);
            if d < 0.3 {
                effect += fwd * (0.3 - d);
            }
            let mut spawn = effect;
            if effect.z - om.pos.z > 0.0 {
                spawn += fwd * 0.6;
            }
            let d2 = (effect - om.pos).dot(fwd);
            (effect - fwd * d2, spawn, ty)
        };
        let opts = LosOpts { peds: false, ignore: Some(owner), ..Default::default() };
        let blocked = self.process_line_of_sight(start, spawn, &opts).is_some();
        if blocked {
            match pt {
                ptype::GRENADE | ptype::SATCHEL => {
                    let mut q = om.pos - om.fwd;
                    q.z -= 0.4;
                    self.add_projectile(owner, pt, q, 0.0, cam);
                }
                ptype::TEARGAS => {}
                _ => self.explode_in_hand(owner, pt, effect),
            }
        } else {
            self.add_projectile(owner, pt, effect, force, cam);
        }
        true
    }

    /// `ExplodeInHand` (0x737C00).
    fn explode_in_hand(&mut self, owner: EntityId, pt: u32, pos: Vec3) {
        let kind = match pt {
            ptype::GRENADE | ptype::SATCHEL => ExplosionType::Grenade,
            ptype::MOLOTOV => ExplosionType::Molotov,
            ptype::ROCKET | ptype::ROCKET_HS => ExplosionType::Rocket,
            _ => return,
        };
        self.add_explosion(None, Some(owner), kind, pos, 0, -1.0, false);
    }

    /// `CProjectileInfo::AddProjectile` (0x737C80).
    pub fn add_projectile(&mut self, creator: EntityId, pt: u32, pos: Vec3, force: f32, cam: Option<(Vec3, Vec3)>) -> bool {
        let Some(cm) = self.body(creator).map(|b| (b.phys.matrix, b.phys.kind)) else { return false };
        let (cmat, ckind) = cm;
        let now = self.now_ms;
        let mut h = heading_of(&cmat);
        let mut elasticity = 0.75;
        let mut gravity = true;
        let (m, v, destroy);
        match pt {
            ptype::GRENADE | ptype::SATCHEL | ptype::TEARGAS => {
                destroy = now + if pt == ptype::TEARGAS { 20_000 } else { 2000 };
                let mut s = if force == 0.0 { 0.0 } else { force * 0.22 + 0.15 };
                if pt == ptype::SATCHEL {
                    s *= 0.5;
                }
                if ckind == EntityType::Vehicle {
                    h = crate::ped::limit_radian_angle(h + std::f32::consts::PI);
                }
                m = rot_z(h, pos);
                v = Vec3::new(-h.sin() * s, h.cos() * s, (force + 1.0) * 0.4 * s);
                elasticity = if pt == ptype::SATCHEL { 0.03 } else { 0.5 };
            }
            ptype::MOLOTOV => {
                destroy = now + 2000;
                let s = (force * 0.22 + 0.15).max(0.2);
                m = rot_z(h, pos);
                v = Vec3::new(-h.sin() * s, h.cos() * s, (force * 0.2 + 0.4) * s);
            }
            ptype::ROCKET | ptype::ROCKET_HS => {
                destroy = now + if pt == ptype::ROCKET { 3000 } else { 10_000 };
                let speed = if pt == ptype::ROCKET { 0.4 } else { 0.2 };
                m = match cam {
                    Some((f, u)) => Matrix { right: u.cross(f), fwd: f, up: u, pos },
                    None => Matrix { pos: cmat.pos, ..cmat },
                };
                v = m.fwd * speed;
                gravity = false;
            }
            _ => return false,
        }
        let Some(i) = self.projectiles.infos.iter().position(|p| !p.active) else { return false };
        let model = self.info(pt).map_or(-1, |w| w.model1);
        let (centre, r) = self.projectiles.model_bounds.get(&model).copied().unwrap_or((Vec3::ZERO, 0.2));
        let col = ColModel {
            bbox_min: centre - Vec3::splat(r),
            bbox_max: centre + Vec3::splat(r),
            bound_center: centre,
            bound_radius: r,
            spheres: vec![ColSphere { center: centre, radius: r * 0.75, surf: Surf { material: 0x38, piece: 0, lighting: 0xFF } }],
            ..Default::default()
        };
        let mut p = Physical::new(EntityType::Object, m);
        p.mass = 1.0;
        p.turn_mass = 1.0;
        p.air_resistance = 0.99999;
        p.elasticity = elasticity;
        p.move_speed = v;
        p.flags = (p.flags & !pf::APPLY_GRAVITY) | if gravity { pf::APPLY_GRAVITY } else { 0 } | pf::KEEP_COLLISION_RECORDS;
        p.ignored = Some(creator);
        let info5 = matches!(pt, ptype::GRENADE | ptype::TEARGAS | ptype::SATCHEL);
        let id = self.add_body(p, col, Box::new(ProjectileLogic { model, radius: r, info5 }));
        let info = &mut self.projectiles.infos[i];
        *info = ProjectileInfo {
            ty: pt,
            creator: Some(creator),
            destroy_ms: destroy,
            active: true,
            last_pos: pos,
            fx: None,
            body: Some(id),
        };
        if pt == ptype::TEARGAS {
            let h = self.effects.create("teargasAD", Vec3::ZERO, Some(id), false);
            self.effects.play(h);
            self.projectiles.infos[i].fx = Some(h);
        }
        true
    }

    /// `CProjectileInfo::RemoveProjectile` (0x7388F0).
    fn remove_projectile(&mut self, i: usize) {
        let info = self.projectiles.infos[i].clone();
        let pos = info.body.and_then(|b| self.body(b)).map_or(info.last_pos, |b| b.phys.matrix.pos);
        let kind = match info.ty {
            ptype::GRENADE => Some(ExplosionType::Grenade),
            ptype::MOLOTOV => Some(ExplosionType::Molotov),
            ptype::ROCKET => Some(ExplosionType::Rocket),
            ptype::ROCKET_HS => Some(if info.creator.is_some() && info.creator == self.player_id() { ExplosionType::Rocket } else { ExplosionType::WeakRocket }),
            _ => None,
        };
        if let Some(k) = kind {
            self.add_explosion(None, info.creator, k, pos, 0, -1.0, false);
        }
        self.drop_projectile(i);
    }

    fn drop_projectile(&mut self, i: usize) {
        let info = &mut self.projectiles.infos[i];
        info.active = false;
        let fx = info.fx.take();
        let body = info.body.take();
        if let Some(h) = fx {
            self.effects.kill(h);
        }
        if let Some(b) = body {
            self.remove(b);
        }
    }

    /// `CWorld::UseDetonator` (0x5660B0): every satchel explodes, whoever threw it.
    pub fn use_detonator(&mut self) {
        for i in 0..self.projectiles.infos.len() {
            let info = &self.projectiles.infos[i];
            if !info.active || info.ty != ptype::SATCHEL {
                continue;
            }
            let pos = info.body.and_then(|b| self.body(b)).map_or(info.last_pos, |b| b.phys.matrix.pos);
            let creator = info.creator;
            self.add_explosion(None, creator, ExplosionType::Grenade, pos, 0, -1.0, false);
            self.drop_projectile(i);
        }
    }

    /// `CProjectileInfo::Update` (0x738B20), after the physics.
    pub(crate) fn update_projectiles(&mut self, ts: f32) {
        let now = self.now_ms;
        for i in 0..self.projectiles.infos.len() {
            let info = self.projectiles.infos[i].clone();
            if !info.active {
                continue;
            }
            let Some(body) = info.body.filter(|b| self.body(*b).is_some()) else {
                self.projectiles.infos[i].active = false;
                continue;
            };
            let (pos, vel, fwd, collided, hit) = {
                let b = self.body(body).unwrap();
                (b.phys.matrix.pos, b.phys.move_speed, b.phys.matrix.fwd, b.phys.has(pf::COLLIDED), b.phys.last_hit)
            };
            // B. thrown objects at rest.
            if matches!(info.ty, ptype::SATCHEL | ptype::GRENADE | ptype::TEARGAS) {
                let b = self.body_mut(body).unwrap();
                if b.phys.elasticity > 0.1 && vel.abs().max_element() < 0.05 {
                    b.phys.elasticity = 0.03;
                }
            }
            // C. rocket smoke trail.
            if matches!(info.ty, ptype::ROCKET | ptype::ROCKET_HS) {
                let step = vel * ts;
                let n = (step.length() as i32).max(1);
                for k in 0..n {
                    let mut mult = PrtMult::new(0.3, 0.3, 0.3, 0.3, 0.5, 1.0, 0.08);
                    let c = self.rng.rand01() * 0.25 + 0.25;
                    mult.rgba[0] = c;
                    mult.rgba[1] = c;
                    mult.rgba[2] = c;
                    mult.life = self.rng.rand01() * 0.04 + 0.08;
                    let p = pos - step * (1.0 - k as f32 / n as f32);
                    let r = Vec3::new(
                        2.0 * self.rng.rand01() - 1.0,
                        2.0 * self.rng.rand01() - 1.0,
                        2.0 * self.rng.rand01() - 1.0,
                    )
                    .normalize_or(Vec3::Z);
                    let pv = vel.normalize_or_zero().cross(r) * 1.5;
                    self.effects.add_particle("prt_smoke_huge", p, pv, 0.0, mult, -1.0, 1.2, 0.6, false);
                }
            }
            // D. timer.
            if now > info.destroy_ms && info.destroy_ms != 0 {
                if info.ty == ptype::SATCHEL {
                    // Satchels never time out (the timer is only cleared without a detonator).
                    self.projectiles.infos[i].destroy_ms = 0;
                } else {
                    self.remove_projectile(i);
                    continue;
                }
            }
            let los_blocked = |w: &mut World| {
                let o = LosOpts { ignore: info.creator, ignore2: Some(body), ..Default::default() };
                w.process_line_of_sight(info.last_pos, pos, &o).is_some()
            };
            match info.ty {
                ptype::ROCKET => {
                    let b = self.body_mut(body).unwrap();
                    b.phys.move_speed += fwd * ts * 0.008;
                    let l = b.phys.move_speed.length();
                    if l > 9.9 {
                        b.phys.move_speed *= 9.9 / l;
                    }
                    if collided || los_blocked(self) {
                        let skip = hit.is_some_and(|h| {
                            Some(h) == info.creator
                                || self.body(h).is_some_and(|hb| hb.logic.as_any().downcast_ref::<ProjectileLogic>().is_some_and(|p| p.model == 345))
                        });
                        if !skip {
                            self.remove_projectile(i);
                            continue;
                        }
                    }
                }
                ptype::ROCKET_HS => {
                    if collided || los_blocked(self) {
                        self.remove_projectile(i);
                        continue;
                    }
                }
                ptype::MOLOTOV => {
                    let far = info.creator.and_then(|c| self.body(c)).is_none_or(|c| (info.last_pos - c.phys.matrix.pos).length_squared() >= 2.0);
                    if far && (collided || los_blocked(self)) {
                        self.remove_projectile(i);
                        continue;
                    }
                }
                ptype::SATCHEL => {
                    // Sticks to what it hit (no attachment to moving entities in this port).
                    let b = self.body_mut(body).unwrap();
                    if b.phys.damage_intensity > 0.0 && !b.phys.is_static() {
                        b.phys.move_speed = Vec3::ZERO;
                        b.phys.turn_speed = Vec3::ZERO;
                        b.phys.eflags |= ef::IS_STATIC;
                    }
                }
                _ => {}
            }
            self.projectiles.infos[i].last_pos = self.body(body).map_or(pos, |b| b.phys.matrix.pos);
        }
    }

    // ------------------------------------------------------------------ area effect

    /// `CWeapon::FireAreaEffect` (0x73E800). `mouse_cam` = the player in the mouse follow
    /// camera (hip fire through the crosshair); `look_pitch` = pd+0x54.
    pub fn fire_area_effect(&mut self, owner: EntityId, ty: u32, src: Vec3, mouse_cam: bool, look_pitch: Option<f32>) {
        let Some(info) = self.info(ty) else { return };
        let Some(om) = self.body(owner).map(|b| b.phys.matrix) else { return };
        let (end, dir) = if mouse_cam {
            let (cs, end) = self.cam_info().target_vector(info.weapon_range, src);
            (end, (end - cs) / info.weapon_range)
        } else {
            let a = heading_of(&om);
            let mut dir = Vec3::new(-a.sin(), a.cos(), 0.0);
            if let Some(p) = look_pitch {
                dir.z = -p.tan();
                dir = dir.normalize();
            }
            (src + dir, dir)
        };
        self.add_shot(owner, ty, src, end, &info);
        // AddGunFxForAreaEffect (0x73E690): one FX per weapon, re-aimed every shot.
        let name = match ty {
            wt::FTHROWER => "flamethrower",
            wt::SPRAYCAN => "spraycan",
            _ => "extinguisher",
        };
        match self.projectiles.weapon_fx.get_mut(&owner) {
            Some((h, t, last)) if *t == ty => {
                self.effects.set_dir(*h, src, dir);
                *last = self.now_ms;
            }
            _ => {
                if let Some((h, _, _)) = self.projectiles.weapon_fx.remove(&owner) {
                    self.effects.kill(h);
                }
                let h = self.effects.create_dir(name, src, dir);
                self.effects.play(h);
                self.effects.set_const_time(h, true, 1.0);
                self.projectiles.weapon_fx.insert(owner, (h, ty, self.now_ms));
            }
        }
        if ty == wt::FTHROWER && self.rng.next() & 3 == 2 {
            let k = self.rng.rand01() * 2.5 + 3.5;
            let mut p = src + dir * k;
            p.z += 0.5;
            self.try_start_fire_at_coors(p, 0, false, 2.3);
        }
    }

    /// `CShotInfo::AddShot` (0x739C30).
    fn add_shot(&mut self, creator: EntityId, ty: u32, src: Vec3, end: Vec3, info: &WeaponInfo) {
        let Some(i) = self.projectiles.shots.iter().position(|s| !s.active) else { return };
        let mut vel = end - src;
        if info.spread != 0.0 {
            vel.x += shot_rand(self.rng.next()) * info.spread;
            vel.y += shot_rand(self.rng.next()) * info.spread;
            vel.z += shot_rand(self.rng.next());
        }
        let mut sp = info.speed;
        if info.has(wf::RANDSPEED) {
            sp += shot_rand(self.rng.next());
        }
        let vel = vel.normalize_or_zero() * sp;
        self.projectiles.shots[i] = ShotInfo {
            ty,
            pos: src,
            vel,
            radius: info.radius,
            creator: Some(creator),
            destroy_ms: (info.lifespan + self.now_ms as f32) as i64 as f32,
            active: true,
            hit: false,
        };
    }

    /// `CShotInfo::Update` (0x739E60) and the area-effect weapon FX timeout.
    pub(crate) fn update_shots(&mut self, ts: f32) {
        let now = self.now_ms;
        for i in 0..self.projectiles.shots.len() {
            let mut s = self.projectiles.shots[i];
            if !s.active {
                continue;
            }
            let Some(info) = self.info(s.ty) else { continue };
            if now as f32 > s.destroy_ms {
                s.active = false;
            }
            if info.has(wf::SLOWSDOWN) {
                s.vel *= 0.96f32.powf(ts);
            }
            s.pos += s.vel * ts;
            match s.ty {
                wt::SPRAYCAN => {}
                wt::EXTINGUISHER => {
                    if !s.hit && self.extinguish_point(s.pos, s.radius, 2.0, ts) {
                        s.hit = true;
                    }
                }
                _ => {
                    if (self.frame as usize + i) & 3 == 0 {
                        self.set_cars_on_fire(s.pos, 4.0, s.creator);
                    }
                    self.set_world_on_fire(s.pos, 0.1, s.creator);
                }
            }
            self.projectiles.shots[i] = s;
        }
        // CWeapon::Update kills the FX on the first frame the weapon is not FIRING.
        let stale: Vec<EntityId> = self
            .projectiles
            .weapon_fx
            .iter()
            .filter(|(_, (_, _, last))| now.wrapping_sub(*last) > 100)
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            if let Some((h, _, _)) = self.projectiles.weapon_fx.remove(&k) {
                self.effects.kill(h);
            }
        }
    }

    /// `CFireManager::ExtinguishPoint` (0x5394C0).
    fn extinguish_point(&mut self, pos: Vec3, radius: f32, rate: f32, ts: f32) -> bool {
        let mut any = false;
        for i in 0..self.fires.len() {
            if !self.fires[i].active || (self.fires[i].pos - pos).length_squared() >= radius * radius {
                continue;
            }
            let old = self.fires[i].strength;
            self.fires[i].strength -= rate * 0.02 * ts;
            let fp = self.fires[i].pos;
            let p = fp
                + Vec3::new(
                    ((self.rng.next() & 0xFF) as f32 - 128.0) * 0.01,
                    ((self.rng.next() & 0xFF) as f32 - 128.0) * 0.01,
                    (self.rng.next() & 0xFF) as f32 * 0.005,
                );
            let mult = PrtMult::new(1.0, 1.0, 1.0, 0.6, 0.75, 0.0, 0.4);
            self.effects.add_particle("prt_smokeII_3_expand", p, Vec3::new(0.0, 0.0, 0.8), 0.0, mult, -1.0, 1.2, 0.6, false);
            self.effects.add_particle("prt_smokeII_3_expand", p, Vec3::new(0.0, 0.0, 1.4), 0.0, mult, -1.0, 1.2, 0.6, false);
            if self.fires[i].strength < 0.0 {
                self.extinguish(i);
            } else if self.fires[i].strength as i32 != old as i32 {
                self.create_fire_fx(i);
            }
            any = true;
        }
        any
    }

    /// `CWorld::SetCarsOnFire` (0x5659F0).
    pub(crate) fn set_cars_on_fire(&mut self, pos: Vec3, r: f32, creator: Option<EntityId>) {
        let cars: Vec<EntityId> = self
            .body_ids()
            .into_iter()
            .filter(|&id| {
                self.body(id).is_some_and(|b| {
                    let d = b.phys.matrix.pos - pos;
                    b.phys.kind == EntityType::Vehicle
                        && b.phys.status != crate::physical::Status::Wrecked
                        && d.z.abs() < 5.0
                        && d.x.abs() < r
                        && d.y.abs() < r
                })
            })
            .collect();
        for c in cars {
            self.start_fire_on(c, creator);
        }
    }

    /// `CWorld::SetWorldOnFire` (0x56B910): a fire where world geometry is within `r`
    /// unless one burns within 2 m.
    pub(crate) fn set_world_on_fire(&mut self, pos: Vec3, r: f32, creator: Option<EntityId>) {
        if self.fires.iter().any(|f| f.active && (f.pos - pos).length_squared() < 4.0) {
            return;
        }
        let near = self
            .process_line_of_sight(pos + Vec3::new(0.0, 0.0, r), pos - Vec3::new(0.0, 0.0, r), &LosOpts { bodies: false, ..Default::default() })
            .is_some();
        if near {
            self.start_fire_at(pos, creator, 7000, 1);
        }
    }
}
