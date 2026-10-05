//! Instant-hit bullets: `CWeapon::FireInstantHit` (0x73FB10), `CWeapon::DoBulletImpact`
//! (0x73B550), `CVehicle::InflictDamage` for bullets, `CBulletTraces`, and the weapon
//! `Fx_c` helpers (AddBulletImpact, AddWood, AddBlood, AddTyreBurst, TriggerGunshot,
//! CWeapon::AddGunshell).
//!
//! Not ported: peds as victims beyond blood (no ped damage yet), CObject::ObjectDamage
//! (breaking / model swaps), CGlass, water splashes, the petrol cap, the AI tyre-burst
//! chance and spread (no armed AI yet), the lock-on paths, audio, stats and crime events.

use glam::Vec3;

use crate::{
    automobile::Automobile,
    colpoint::ColPoint,
    collision::{ColLine, ColModel, MAX_COLPOINTS, process_col_models},
    effects::{FrameFx, PrtMult},
    physical::{EntityType, Matrix, Status, VehicleClass},
    weapon::{WeaponInfo, wt},
    world::{EntityId, LosOpts, World},
};

// ------------------------------------------------------------------ CBulletTraces

#[derive(Debug, Clone, Copy)]
pub struct BulletTrace {
    pub start: Vec3,
    pub end: Vec3,
    pub created_ms: u32,
    pub life_ms: u32,
    /// Half width, metres.
    pub width: f32,
    pub alpha: u8,
}

/// `CBulletTraces::aTraces[16]` (0xC7C748).
#[derive(Debug, Clone, Default)]
pub struct BulletTraces {
    pub traces: [Option<BulletTrace>; 16],
}

impl BulletTraces {
    /// `AddTrace(start, end, width, lifeTime, alpha)` (0x723750).
    pub fn add(&mut self, start: Vec3, end: Vec3, width: f32, mut life_ms: u32, alpha: u8, now: u32) {
        let n = self.traces.iter().flatten().count();
        if n >= 10 {
            life_ms = (life_ms as f32 * 0.25) as u32;
        } else if n >= 5 {
            life_ms = (life_ms as f32 * 0.5) as u32;
        }
        if let Some(slot) = self.traces.iter_mut().find(|t| t.is_none()) {
            *slot = Some(BulletTrace { start, end, created_ms: now, life_ms, width, alpha });
        }
    }

    /// `Update` (0x723FB0).
    pub fn update(&mut self, now: u32) {
        for t in &mut self.traces {
            if t.is_some_and(|e| now.wrapping_sub(e.created_ms) >= e.life_ms) {
                *t = None;
            }
        }
    }
}

// ------------------------------------------------------------------ Fx_c helpers

/// `0x59F0C0`: a colpoint lighting byte (day low nibble, night high nibble) as a float.
pub fn col_lighting(lighting: u8, scale: f32, dn_balance: f32) -> f32 {
    let day = (lighting & 0xF) as f32 * scale / 15.0;
    let night = (lighting >> 4) as f32 * scale / 15.0;
    day * (1.0 - dn_balance) + night * dn_balance
}

impl FrameFx<'_> {
    fn u4w(&mut self) -> f32 {
        (self.rng.next() % 10000) as f32 * 1e-4
    }

    /// `Fx_c::AddBulletImpact` (0x49F3D0).
    pub fn add_bullet_impact(&mut self, pos: Vec3, normal: Vec3, surface: u8, count: i32, light_mult: f32) {
        let fx = self.surfaces.info(surface).bullet_fx;
        if fx == 0 || (self.cam - pos).length_squared() > 22500.0 {
            return;
        }
        match fx {
            1 => {
                self.add_sparks(pos, normal, 3.0, count, Vec3::ZERO, true, 0.4, 1.0);
                let mut mult = PrtMult::new(1.0, 1.0, 1.0, 0.15, 0.4, 0.0, 0.075);
                let mut n = 2;
                if count >= 8 {
                    n = 1;
                    mult.rgba[3] *= 2.0;
                }
                for i in 0..n {
                    self.fx.add_particle("prt_smokeII_3_expand", pos, normal, i as f32 * 0.05, mult, -1.0, light_mult, 0.6, false);
                }
            }
            2 | 4 => {
                let mut mult = PrtMult::new(0.81, 0.67, 0.57, 0.15, 0.4, 0.0, 0.3);
                if fx == 4 {
                    mult.rgba[0] = 0.6;
                    mult.rgba[1] = 0.6;
                    mult.rgba[2] = 0.6;
                }
                let mut n = 4;
                if count >= 8 {
                    n = 2;
                    mult.rgba[3] *= 2.0;
                }
                for i in 0..n {
                    let vel = normal * 0.3;
                    self.fx.add_particle("prt_sand", pos, vel, i as f32 * 0.05, mult, -1.0, light_mult, 0.6, false);
                }
            }
            3 => self.add_wood(pos, normal, (count as f32 * 0.5) as i32, 1.0),
            _ => {}
        }
    }

    /// `Fx_c::AddWood` (0x49EE10): chips on the blood system.
    pub fn add_wood(&mut self, pos: Vec3, normal: Vec3, count: i32, light_mult: f32) {
        if (self.cam - pos).length_squared() > 625.0 {
            return;
        }
        let mut mult = PrtMult::new(0.5, 0.25, 0.0, 1.0, 0.3, 0.0, 1.0);
        for _ in 0..count.max(0) {
            mult.rgba[0] = self.u4w() * 0.12 + 0.13;
            mult.rgba[1] = self.u4w() * 0.030_000_009 + 0.12;
            mult.rgba[2] = self.u4w() * 0.03 + 0.04;
            mult.size = self.u4w() * 0.3 + 0.7;
            let mut vel = normal * 4.0;
            vel.x += self.u4w() * 4.0 - 2.0;
            vel.y += self.u4w() * 4.0 - 2.0;
            vel.z += self.u4w() * 4.0 - 2.0;
            self.fx.add_particle("prt_blood", pos, vel, 0.0, mult, -1.0, light_mult, 0.6, false);
        }
    }

    /// `Fx_c::AddBlood` (0x49EB00), without the blood pool shadow.
    pub fn add_blood(&mut self, pos: Vec3, dir: Vec3, count: i32, light_mult: f32) {
        if (self.cam - pos).length_squared() > 625.0 {
            return;
        }
        let mut mult = PrtMult::new(0.5, 0.0, 0.0, 1.0, 0.8, 0.0, 0.8);
        for _ in 0..count.max(0) {
            mult.size = self.u4w() * 0.3 + 0.7;
            let mut vel = dir * 1.5;
            vel.x += self.u4w() * 2.0 - 1.0;
            vel.y += self.u4w() * 2.0 - 1.0;
            vel.z += self.u4w() * 2.0 - 1.0;
            self.fx.add_particle("prt_blood", pos, vel, 0.0, mult, -1.0, light_mult, 0.6, false);
        }
        // The pool position rands are always drawn.
        self.u4w();
        self.u4w();
    }

    /// `Fx_c::AddPunchImpact` (0x49F670).
    pub fn add_punch_impact(&mut self, pos: Vec3, vel: Vec3) {
        if (self.cam - pos).length_squared() > 625.0 {
            return;
        }
        let mult = PrtMult::new(1.0, 1.0, 1.0, 0.4, 0.1, 0.0, 0.1);
        for i in 0..2 {
            self.fx.add_particle("prt_smokeII_3_expand", pos, vel, i as f32 * 0.05, mult, -1.0, 1.2, 0.6, false);
        }
    }

    /// `Fx_c::AddTyreBurst` (0x49F300).
    pub fn add_tyre_burst(&mut self, pos: Vec3, vel: Vec3) {
        if (self.cam - pos).length_squared() > 625.0 {
            return;
        }
        let mult = PrtMult::new(1.0, 1.0, 1.0, 0.4, 0.12, 0.0, 0.1);
        for i in 0..3 {
            self.fx.add_particle("prt_smokeII_3_expand", pos, vel, i as f32 * 0.05, mult, -1.0, 1.2, 0.6, false);
        }
    }

    /// `Fx_c::TriggerGunshot` (0x4A0DE0) with a firer: both systems attached to the firer's
    /// frame at the muzzle's local offset.
    pub fn trigger_gunshot(&mut self, firer: EntityId, firer_mat: &Matrix, pos: Vec3, do_gunflash: bool) {
        if (self.cam - pos).length_squared() > 625.0 {
            return;
        }
        let d = pos - firer_mat.pos;
        let offset = Vec3::new(d.dot(firer_mat.right), d.dot(firer_mat.fwd), d.dot(firer_mat.up));
        if do_gunflash {
            let h = self.fx.create("gunflash", offset, Some(firer), false);
            self.fx.play_and_kill(h);
        }
        let h = self.fx.create("gunsmoke", offset, Some(firer), false);
        self.fx.play_and_kill(h);
    }

    /// `CWeapon::AddGunshell` (0x73A3E0). `on_screen` is the firer's GetIsOnScreen.
    pub fn add_gunshell(&mut self, ty: u32, firer_pos: Vec3, on_screen: bool, pos: Vec3, dir: glam::Vec2, size: f32) {
        if !on_screen || (self.cam - firer_pos).length_squared() > 100.0 {
            return;
        }
        let vel = Vec3::new(dir.x, dir.y, self.rand01_w() * 0.06 * 20.0 + 0.4);
        self.rand01_w(); // GetRandomNumberInRange(-20, 20), discarded
        let mut mult = PrtMult::new(0.5, 0.5, 0.5, 1.0, size, 1.0, 1.0);
        if ty == wt::SHOTGUN || ty == wt::SPAS12 {
            mult.rgba[0] = 0.6;
            mult.rgba[1] = 0.1;
            mult.rgba[2] = 0.1;
        }
        self.fx.add_particle("prt_gunshell", pos, vel, 0.0, mult, -1.0, 1.2, 0.6, false);
    }

    fn rand01_w(&mut self) -> f32 {
        self.rng.rand01()
    }
}

// ------------------------------------------------------------------ firing

/// What `CTaskSimpleUseGun::FireGun` hands to `CWeapon::Fire` for an instant-hit gun.
#[derive(Debug, Clone, Copy)]
pub struct InstantHit {
    pub owner: EntityId,
    pub ty: u32,
    pub skill: u8,
    /// Line-of-sight start (the hand bone) and the muzzle.
    pub origin: Vec3,
    pub effect: Vec3,
    pub is_player: bool,
    /// `ped+0x71A` (100 for the player).
    pub accuracy: u8,
    pub ducking: bool,
    /// `playerData+0x2C` (the spread counter; only read when accuracy < 100).
    pub attack_counter: f32,
    /// The ped's weapon model has a `gunflash` frame (no fx gunflash then).
    pub model_flash: bool,
}

/// `CWeapon::TargetWeaponRangeMultiplier` (0x73B380), for the victims this port has.
fn range_multiplier(w: &World, victim: Option<EntityId>) -> f32 {
    match victim.and_then(|v| w.body(v)) {
        Some(b) if b.phys.kind == EntityType::Vehicle && b.phys.vclass() != Some(VehicleClass::Bike) => 3.0,
        _ => 1.0,
    }
}

/// `CWeapon::SetUpPelletCol` (0x73C710): `n` lines parallel to the shot in a disc of
/// radius `r` around the hit, and the matrix that places them.
fn set_up_pellet_col(
    w: &mut World,
    n: usize,
    owner_fwd: Vec3,
    victim_is_building: bool,
    start: Vec3,
    cp: &ColPoint,
    spread_rate: f32,
) -> (ColModel, Matrix) {
    let r = (cp.point - start).length() * spread_rate * 1.3;
    let mut lines = vec![ColLine { start: Vec3::new(0.0, -r, 0.0), end: Vec3::new(0.0, r, 0.0) }];
    for _ in 1..n {
        let a = w.rng.rand01() * std::f32::consts::TAU - std::f32::consts::PI;
        let rad = w.rng.rand01() * (r - 0.2 * r) + 0.2 * r;
        let (s, c) = a.sin_cos();
        lines.push(ColLine {
            start: Vec3::new(c * rad, -2.0 * r, s * rad),
            end: Vec3::new(c * rad, 2.0 * r, s * rad),
        });
    }
    let col = ColModel {
        bbox_min: Vec3::new(-r, -2.0 * r, -r),
        bbox_max: Vec3::new(r, 2.0 * r, r),
        bound_center: Vec3::ZERO,
        bound_radius: 2.5 * r,
        lines,
        ..Default::default()
    };
    let fwd = if victim_is_building { -cp.normal } else { (cp.point - start).normalize_or(Vec3::Y) };
    let right = if victim_is_building {
        let refv = if cp.normal.x.abs() >= 0.9 { Vec3::Y } else { Vec3::X };
        fwd.cross(refv).normalize_or(Vec3::X)
    } else if fwd.z.abs() > 0.9 {
        owner_fwd.cross(fwd).normalize_or(Vec3::X)
    } else {
        fwd.cross(Vec3::Z).normalize_or(Vec3::X)
    };
    let up = right.cross(fwd);
    let mut pos = cp.point;
    if !victim_is_building {
        pos -= fwd * (r * cp.normal.dot(fwd));
    }
    (col, Matrix { right, fwd, up, pos })
}

impl World {
    /// Build a `FrameFx` for weapon effects.
    pub(crate) fn weapon_fx<R>(&mut self, f: impl FnOnce(&mut FrameFx) -> R) -> R {
        let mut requests = Vec::new();
        let mut fx = FrameFx {
            fx: &mut self.effects,
            requests: &mut requests,
            now_ms: self.now_ms,
            frame: self.frame,
            ts: self.last_ts,
            rng: &mut self.rng,
            cam: self.camera_pos,
            cam_planes: self.camera_planes,
            wet_roads: self.weather.wet_roads,
            foggyness: self.weather.foggyness,
            hours: self.clock.hours,
            minutes: self.clock.minutes,
            cam_fwd: self.camera_fwd,
            sprite_brightness: 10.0,
            player_in_vehicle: false,
            surfaces: &self.surfaces,
        };
        f(&mut fx)
    }

    fn sphere_visible(&self, c: Vec3, r: f32) -> bool {
        self.camera_planes.iter().all(|(n, d)| n.dot(c) - d <= r)
    }

    /// `CWeapon::FireInstantHit` (0x73FB10) for a ped on foot without a target entity: the
    /// player's free aim through the crosshair (camera modes 53/55/65/49) or a straight shot
    /// along the ped's heading.
    pub fn fire_instant_hit(&mut self, h: InstantHit) {
        let Some(infos) = self.weapon_infos.clone() else { return };
        let info = infos.get(h.ty, h.skill).clone();
        let Some(owner_mat) = self.body(h.owner).map(|b| b.phys.matrix) else { return };
        let mut spread = (100.0 - h.accuracy as f32) / info.accuracy;
        if h.is_player && h.ducking {
            spread *= 0.5;
        }
        let mut spread_rate = 0.0;
        if matches!(h.ty, wt::SHOTGUN | wt::SAWNOFF | wt::SPAS12) {
            spread = 0.0;
            spread_rate = 0.05 / info.accuracy;
        }
        let start = h.origin;
        let opts = LosOpts { shoot_through: true, ignore: Some(h.owner), ..Default::default() };
        let (end, dir, hit) = if h.is_player && matches!(self.camera_mode, 53 | 55 | 65 | 49) {
            let (cam_src, mut end) = self.cam_info().target_vector(info.weapon_range * 3.0, start);
            let dir = (end - start).normalize_or(owner_mat.fwd);
            if spread != 0.0 {
                let f = (15.0 / info.weapon_range).min(1.0);
                let r = f * spread * h.attack_counter * 0.75;
                let right = self.camera_fwd.cross(self.camera_up).normalize_or(Vec3::X);
                let up = self.camera_up;
                let t = self.now_ms as f32 * 0.006_283_185_4;
                end += right * (r * t.sin()) + up * (r * t.cos());
            }
            let hit = self
                .process_line_of_sight(cam_src, end, &LosOpts { car_tyres: true, ..opts })
                .filter(|(id, _, cp)| {
                    let d2 = (cp.point.truncate() - cam_src.truncate()).length();
                    info.weapon_range * range_multiplier(self, Some(*id)) >= d2
                });
            (end, dir, hit)
        } else {
            let end = h.effect + owner_mat.fwd * info.weapon_range;
            let hit = self.process_line_of_sight(start, end, &opts);
            (end, owner_mat.fwd, hit)
        };

        // Gun FX (point light, gunsmoke / gunflash, shell).
        let sizes = match h.ty {
            22..=24 | 34 => Some((0.2, 0.25)),
            25..=27 => Some((0.3, 0.45)),
            28 | 29 | 32 => Some((0.2, 0.3)),
            30 | 31 | 38 => {
                let fast = (((info.anim_loop_end - info.anim_loop_start) * 900.0) as i32) < 50;
                if fast {
                    self.gun_fx_toggle = self.gun_fx_toggle.wrapping_add(1);
                }
                (!fast || self.gun_fx_toggle & 1 == 0).then_some((0.65, 0.25))
            }
            _ => None,
        };
        if let Some((a, b)) = sizes {
            let cam = self.camera_pos;
            self.effects.add_point_light(cam, 0, h.effect, Vec3::ZERO, 3.0, Vec3::new(0.25, 0.22, 0.0), 0, false);
            let on_screen = self.sphere_visible(owner_mat.pos, 1.0);
            let effect = h.effect;
            self.weapon_fx(|f| {
                f.trigger_gunshot(h.owner, &owner_mat, effect, !h.model_flash);
                let right = glam::Vec2::new(owner_mat.right.x, owner_mat.right.y);
                f.add_gunshell(h.ty, owner_mat.pos, on_screen, effect - dir * a, right, b);
            });
        }

        // Pellets (shotguns) and the impact.
        let Some((mut victim, _, mut cp)) = hit else {
            self.do_bullet_impact(&h, &info, None, h.effect, end, &ColPoint::default(), 0);
            return;
        };
        let pellet_able = |w: &World, v: EntityId, cp: &ColPoint| {
            !(w.body(v).is_some_and(|b| b.phys.kind == EntityType::Vehicle) && cp.is_wheel_b())
        };
        if spread_rate > 0.0 && pellet_able(self, victim, &cp) {
            let mut iter = 0;
            let mut los_start = start;
            loop {
                iter += 1;
                let n = if h.ty == wt::SPAS12 { 8 } else { 15 };
                let is_building = matches!(victim, EntityId::Building(_));
                let (pellets, m) = set_up_pellet_col(self, n, owner_mat.fwd, is_building, start, &cp, spread_rate);
                let (vmat, vcol) = match victim {
                    EntityId::Building(_) => {
                        let b = self.building(victim).unwrap();
                        (b.matrix, (*b.col).clone())
                    }
                    EntityId::Body(_) => {
                        let b = self.body(victim).unwrap();
                        (b.phys.matrix, b.col.clone())
                    }
                };
                let mut pts = [ColPoint::default(); MAX_COLPOINTS];
                let mut line_pts = vec![ColPoint::default(); n];
                let mut dist = vec![1.0f32; n];
                process_col_models(&m, &pellets, &vmat, &vcol, &mut pts, &mut line_pts, &mut dist, false);
                let hits = dist.iter().filter(|&&d| d < 1.0).count();
                let last = dist.iter().rposition(|&d| d < 1.0);
                for i in 0..n {
                    if dist[i] < 1.0 {
                        let inc = if Some(i) == last { -(hits as i32) } else { 1 };
                        let p = line_pts[i];
                        self.do_bullet_impact(&h, &info, Some(victim), h.effect, p.point, &p, inc);
                    }
                }
                let kind = self.body(victim).map(|b| b.phys.kind);
                if matches!(kind, Some(EntityType::Ped | EntityType::Vehicle)) {
                    if (dist[0] == 1.0 || (hits as f32 / n as f32) < 0.5) && iter < 2 {
                        los_start = cp.point;
                        let next = self.process_line_of_sight(
                            los_start,
                            end,
                            &LosOpts { ignore2: Some(victim), ..opts },
                        );
                        if let Some((v, _, c)) = next {
                            victim = v;
                            cp = c;
                            continue;
                        }
                    }
                } else {
                    self.do_bullet_impact(&h, &info, Some(victim), h.effect, end, &cp, 0);
                }
                break;
            }
            let _ = los_start;
        } else {
            self.do_bullet_impact(&h, &info, Some(victim), h.effect, end, &cp, 0);
        }
    }

    /// `CBulletTraces::AddTrace(start, end, weaponType, firer)` (0x726AF0): a random 2..5 m
    /// piece of the path. (No first-person modes in this port, so no player exception.)
    fn add_weapon_trace(&mut self, start: Vec3, end: Vec3) {
        let d = end - start;
        let len = d.length();
        let d = d.normalize_or_zero();
        let r1 = self.rng.rand01() * len;
        let s = start + d * r1;
        let rem = len - r1;
        let l = if self.rng.rand01() * 3.0 + 2.0 < rem { self.rng.rand01() * 3.0 + 2.0 } else { rem };
        let now = self.now_ms;
        self.bullet_traces.add(s, s + d * l, 0.01, 300, 70, now);
    }

    /// `CWeapon::DoBulletImpact` (0x73B550). `start` is the muzzle; `inc` 0 for a bullet,
    /// 1 for a pellet, `-hits` for the last pellet.
    #[allow(clippy::too_many_arguments)]
    fn do_bullet_impact(
        &mut self,
        h: &InstantHit,
        info: &WeaponInfo,
        victim: Option<EntityId>,
        start: Vec3,
        end: Vec3,
        cp: &ColPoint,
        inc: i32,
    ) {
        let Some(victim) = victim else {
            self.add_weapon_trace(start, end);
            return;
        };
        let trace_start = if inc != 0 { start + (cp.point - start) * 0.4 } else { start };
        self.add_weapon_trace(trace_start, cp.point);

        let kind = match victim {
            EntityId::Building(_) => EntityType::Building,
            EntityId::Body(_) => self.body(victim).map_or(EntityType::Building, |b| b.phys.kind),
        };
        let fx_count = if inc != 0 { 2 } else { 8 };
        let dn = self.clock.dn_balance();
        let light = col_lighting(cp.lighting_b, 0.5, dn);
        let visible = self.sphere_visible(cp.point, 1.0);
        let (point, normal, surface) = (cp.point, cp.normal, cp.surface_b);
        match kind {
            EntityType::Ped => {
                if victim == h.owner {
                    return;
                }
                let n = if inc != 0 { 4 } else if cp.piece_b == 9 { 16 } else { 8 };
                self.weapon_fx(|f| f.add_blood(point, normal, n, 1.0));
            }
            EntityType::Object => {
                if visible {
                    self.weapon_fx(|f| f.add_bullet_impact(point, normal, surface, fx_count, light));
                }
                let damager = self.body(h.owner).map(|b| (b.phys.kind, b.phys.status == crate::physical::Status::Player, b.phys.vehicle.map_or(0, |v| v.model)));
                if let Some(b) = self.body_mut(victim) {
                    let crate::world::Body { phys, logic, .. } = b;
                    if let Some(o) = logic.as_any_mut().downcast_mut::<crate::objects::ObjectLogic>() {
                        let d = o.bullet_damage();
                        o.object_damage(phys, d, Some(point), Some(normal), damager, h.ty as u8);
                    }
                }
                let uproot = self.body(victim).and_then(|b| b.logic.uproot_limit());
                if let Some(b) = self.body_mut(victim) {
                    if b.phys.is_static() && uproot.is_some_and(|u| u <= 0.0) {
                        b.phys.eflags &= !crate::physical::ef::IS_STATIC;
                    }
                    if !b.phys.is_static() {
                        let mut k = if b.phys.flags & 0xA0 != 0 { -0.2 } else { -2.0 };
                        if inc != 0 {
                            k *= 0.2;
                        }
                        let r = point - b.phys.matrix.pos;
                        b.phys.apply_force(normal * k, r, true);
                    }
                }
            }
            EntityType::Vehicle => {
                if cp.is_wheel_b() {
                    if let Some(b) = self.body_mut(victim) {
                        let crate::world::Body { phys, logic, .. } = b;
                        if let Some(car) = logic.as_any_mut().downcast_mut::<Automobile>() {
                            car.burst_tyre(phys, cp.piece_b, true);
                        }
                    }
                    self.weapon_fx(|f| f.add_tyre_burst(point, normal));
                } else {
                    let dmg = info.damage as f32;
                    let owner = h.owner;
                    if let Some(b) = self.body_mut(victim) {
                        let crate::world::Body { phys, logic, .. } = b;
                        if let Some(car) = logic.as_any_mut().downcast_mut::<Automobile>() {
                            // CanVehicleBeDamaged: a burning player car ignores bullets.
                            if !(phys.status == Status::Player && car.damage.health < 250.0) {
                                car.damage.inflict_damage(phys, Some(owner), false, dmg);
                            }
                        }
                    }
                    if visible {
                        self.weapon_fx(|f| f.add_bullet_impact(point, normal, surface, fx_count, light));
                    }
                    let mut k = match h.ty {
                        wt::DESERT_EAGLE | wt::MINIGUN => -20.0,
                        wt::SHOTGUN | wt::SPAS12 => -4.0,
                        _ => -10.0,
                    };
                    if let Some(b) = self.body_mut(victim) {
                        k *= (b.phys.mass * 0.001).min(1.0);
                        let r = point - b.phys.matrix.pos;
                        b.phys.apply_force(normal * k, r, true);
                    }
                }
            }
            EntityType::Building => {
                if visible {
                    self.weapon_fx(|f| f.add_bullet_impact(point, normal, surface, fx_count, light));
                }
            }
        }
    }
}
