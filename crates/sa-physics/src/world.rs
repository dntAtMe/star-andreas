//! `CWorld`: entity storage, sector lists and the per-frame physics loop
//! (`CWorld::Process` 0x5684A0), with `CPhysical::ProcessCollision` (0x54DFB0),
//! `ProcessShift` (0x54DB10), `CheckCollision` (0x54D920),
//! `ProcessCollisionSectorList` (0x54BA60) and `ProcessShiftSectorList` (0x546670).
//!
//! Differences from the original, on purpose:
//! - Buildings live in the 120x120 x 50-unit sector grid like the game, but
//!   dynamic bodies are scanned as one list per type instead of the 16x16
//!   repeat sectors (same list order: vehicles, objects, peds; shift: objects,
//!   peds, vehicles, then buildings).
//! - Attachments, ignored entities, audio, damage gameplay and the
//!   ped/train kill hooks are omitted.
//!
//! After the physics, each frame runs the bodies' effect hooks (PreRender FX,
//! BlowUpCar's world side), then `CExplosion::Update` and `CFireManager::Update`
//! (see explosion.rs / fire.rs), like `CGame::Process`.

use std::sync::Arc;

use glam::Vec3;

use crate::{
    Ctx,
    clock::Clock,
    damage::Rand,
    shadows::Shadows,
    weather::Weather,
    effects::{Effects, FrameFx, WorldRequest},
    explosion::{Explosion, MAX_EXPLOSIONS},
    fire::{Fire, MAX_FIRES},
    collision::{ColModel, ColSphere, MAX_COLPOINTS, process_col_models},
    colpoint::ColPoint,
    pair::{self, PairInfo},
    ped::{NO_CEILING, PedLogic, ped_lines},
    physical::{EntityType, Matrix, Physical, Status, VehicleClass, ef, normalise, pf},
    surface::{SURFACE_WHEELBASE, SurfaceInfos},
};

pub const SECTORS: i32 = 120;
pub const SECTOR_SIZE: f32 = 50.0;
/// Regular collision passes before the final "stuck" pass.
const COLLISION_PASSES: usize = 5;
pub const MAX_LINES: usize = 8;

/// Sector index of a world coordinate (clamped to the grid).
pub fn sector_coord(c: f32) -> i32 {
    ((c * 0.02 + 60.0).floor() as i32).clamp(0, SECTORS - 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityId {
    Building(u32),
    Body(u32),
}

pub struct Building {
    pub matrix: Matrix,
    pub col: Arc<ColModel>,
    pub(crate) scan: u16,
    sectors: Vec<u32>,
}

/// Results of a body's wheel / suspension lines for the current frame.
#[derive(Debug, Clone, Copy)]
pub struct LineHits {
    pub points: [ColPoint; MAX_LINES],
    /// Best hit fraction along each line (1.0 = no hit).
    pub values: [f32; MAX_LINES],
    pub entities: [Option<EntityId>; MAX_LINES],
}

impl Default for LineHits {
    fn default() -> Self {
        Self { points: [ColPoint::default(); MAX_LINES], values: [1.0; MAX_LINES], entities: [None; MAX_LINES] }
    }
}

/// Per-type behaviour (the subclass overrides of CPhysical).
pub trait BodyLogic: Send + Sync + 'static {
    /// vtbl+0x28 ProcessControl. Default: CPhysical::ProcessControl.
    /// `lines` are the wheel-line results of the previous frame's collision passes.
    fn process_control(&mut self, phys: &mut Physical, col: &mut ColModel, ctx: &Ctx, lines: &LineHits) {
        let _ = (col, lines);
        phys.process_control(ctx);
    }

    /// vtbl+0x40 SpecialEntityCalcCollisionSteps: (steps, probe the full step first).
    fn collision_steps(&self, phys: &Physical, ts: f32) -> (u8, bool) {
        let _ = (phys, ts);
        (1, false)
    }

    /// object.dat uproot limit for static props (impulse in SA units).
    fn uproot_limit(&self) -> Option<f32> {
        None
    }

    /// Objects: `CObjectData` buoyancy `(100 / percentSubmerged)·mass·0.008`, if the object floats.
    fn buoyancy(&self, phys: &Physical) -> Option<f32> {
        let _ = phys;
        None
    }

    /// Tyre spheres for bullet line tests (`bIncludeCarTyres`), model space, pieces 13..16.
    fn tyre_spheres(&self, col: &ColModel) -> Vec<ColSphere> {
        let _ = col;
        Vec::new()
    }

    /// The body is leaving the world: stop any FX it owns.
    fn on_remove(&mut self, fx: &mut Effects) {
        let _ = fx;
    }

    /// After the physics: FX requests and world-side consequences (PreRender's
    /// smoke, BlowUpCar's explosion, ...). Runs for every body, static or not.
    fn process_effects(&mut self, id: EntityId, phys: &mut Physical, col: &ColModel, fx: &mut FrameFx) {
        let _ = (id, phys, col, fx);
    }

    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Plain CPhysical behaviour.
pub struct PlainLogic;

impl BodyLogic for PlainLogic {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub struct Body {
    pub phys: Physical,
    /// Per-body copy: vehicles own lines in their collision model.
    pub col: ColModel,
    pub logic: Box<dyn BodyLogic>,
    pub lines: LineHits,
    scan: u16,
}

/// What a sector-list entry produced.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    None,
    Hard,
}

pub struct World {
    buildings: Vec<Option<Building>>,
    free_buildings: Vec<u32>,
    sectors: Vec<Vec<u32>>,
    bodies: Vec<Option<Body>>,
    free_bodies: Vec<u32>,
    scan_code: u16,
    pub surfaces: SurfaceInfos,
    pub(crate) last_ts: f32,
    /// FX, lights and shakes requested by the gameplay code; drained by the app.
    pub effects: Effects,
    /// `CTimer::m_snTimeInMilliseconds` and `m_FrameCounter`.
    pub now_ms: u32,
    time_ms: f64,
    pub frame: u32,
    /// Camera position, look direction and heading (`TheCamera.m_fOrientation`,
    /// atan2(fwd.x, fwd.y)), set by the app.
    pub camera_pos: Vec3,
    pub camera_fwd: Vec3,
    /// Camera matrix right (TheCamera+0x9C4..), for the fire coronas.
    pub camera_right: Vec3,
    /// Camera side planes (outward normal, d), for on-screen tests.
    pub camera_planes: [(Vec3, f32); 4],
    pub camera_orientation: f32,
    /// Camera up vector, horizontal FOV (degrees), aspect and `CCam` mode (4 follow, 53 aim).
    pub camera_up: Vec3,
    pub camera_fov: f32,
    pub camera_aspect: f32,
    pub camera_mode: u8,
    pub clock: Clock,
    pub weather: Weather,
    /// Permanent and static shadows (scorch marks, fire glow).
    pub shadows: Shadows,
    /// `CTimeCycle` (loaded by the app from data/timecyc.dat).
    pub timecycle: Option<crate::timecycle::TimeCycle>,
    /// `CCoronas`.
    pub coronas: crate::coronas::Coronas,
    /// The CRT `rand()` shared by explosions and fires.
    pub rng: Rand,
    pub(crate) explosions: Vec<Explosion>,
    pub(crate) fires: Vec<Fire>,
    /// `CCreepingFire::m_aFireStatus[32][32]`.
    pub(crate) creeping: [[u8; 32]; 32],
    /// `CWeaponInfo` table (loaded by the app from data/weapon.dat).
    pub weapon_infos: Option<Arc<crate::weapon::WeaponInfos>>,
    /// `CBulletTraces`.
    pub bullet_traces: crate::bullet::BulletTraces,
    /// `CProjectileInfo` and `CShotInfo`.
    pub projectiles: crate::projectile::Projectiles,
    /// `CWaterLevel` (data/water.dat, set by the app).
    pub water: Option<Arc<crate::water::WaterLevel>>,
    /// Byte 0xC8A80C: every-second-shot gun FX toggle of the fast rifles.
    pub(crate) gun_fx_toggle: u8,
}

impl Default for World {
    fn default() -> Self {
        Self::new(SurfaceInfos::default())
    }
}

impl World {
    pub fn new(surfaces: SurfaceInfos) -> Self {
        Self {
            buildings: Vec::new(),
            free_buildings: Vec::new(),
            sectors: vec![Vec::new(); (SECTORS * SECTORS) as usize],
            bodies: Vec::new(),
            free_bodies: Vec::new(),
            scan_code: 0,
            surfaces,
            last_ts: 1.0,
            effects: Effects::default(),
            now_ms: 0,
            time_ms: 0.0,
            frame: 0,
            camera_pos: Vec3::ZERO,
            camera_fwd: Vec3::Y,
            camera_right: Vec3::X,
            camera_planes: [(Vec3::ZERO, 1e9); 4],
            camera_orientation: 0.0,
            camera_up: Vec3::Z,
            camera_fov: 70.0,
            camera_aspect: 16.0 / 9.0,
            camera_mode: 4,
            clock: Clock::new(0),
            weather: Weather::default(),
            shadows: Shadows::new(),
            timecycle: None,
            coronas: crate::coronas::Coronas::default(),
            rng: Rand::new(1),
            explosions: vec![Explosion::default(); MAX_EXPLOSIONS],
            fires: vec![Fire::default(); MAX_FIRES],
            creeping: [[0; 32]; 32],
            weapon_infos: None,
            bullet_traces: Default::default(),
            projectiles: Default::default(),
            water: None,
            gun_fx_toggle: 0,
        }
    }

    // ------------------------------------------------------------ entities

    pub fn add_building(&mut self, matrix: Matrix, col: Arc<ColModel>) -> EntityId {
        let c = matrix.transform(col.bound_center);
        let r = col.bound_radius;
        let mut sectors = Vec::new();
        for y in sector_coord(c.y - r)..=sector_coord(c.y + r) {
            for x in sector_coord(c.x - r)..=sector_coord(c.x + r) {
                sectors.push((y * SECTORS + x) as u32);
            }
        }
        let b = Building { matrix, col, scan: 0, sectors };
        let id = match self.free_buildings.pop() {
            Some(i) => {
                self.buildings[i as usize] = Some(b);
                i
            }
            None => {
                self.buildings.push(Some(b));
                (self.buildings.len() - 1) as u32
            }
        };
        for &s in &self.buildings[id as usize].as_ref().unwrap().sectors {
            self.sectors[s as usize].push(id);
        }
        EntityId::Building(id)
    }

    pub fn add_body(&mut self, mut phys: Physical, col: ColModel, logic: Box<dyn BodyLogic>) -> EntityId {
        phys.bound_radius = col.bound_radius;
        let b = Body { phys, col, logic, lines: LineHits::default(), scan: 0 };
        let id = match self.free_bodies.pop() {
            Some(i) => {
                self.bodies[i as usize] = Some(b);
                i
            }
            None => {
                self.bodies.push(Some(b));
                (self.bodies.len() - 1) as u32
            }
        };
        EntityId::Body(id)
    }

    pub fn remove(&mut self, id: EntityId) {
        match id {
            EntityId::Building(i) => {
                if let Some(b) = self.buildings.get_mut(i as usize).and_then(Option::take) {
                    for s in b.sectors {
                        self.sectors[s as usize].retain(|&x| x != i);
                    }
                    self.free_buildings.push(i);
                }
            }
            EntityId::Body(i) => {
                if let Some(mut b) = self.bodies.get_mut(i as usize).and_then(Option::take) {
                    b.logic.on_remove(&mut self.effects);
                    self.free_bodies.push(i);
                    if let Some(f) = self.fire_on(id) {
                        self.extinguish(f);
                    }
                }
            }
        }
    }

    pub fn body(&self, id: EntityId) -> Option<&Body> {
        match id {
            EntityId::Body(i) => self.bodies.get(i as usize)?.as_ref(),
            EntityId::Building(_) => None,
        }
    }

    pub fn body_mut(&mut self, id: EntityId) -> Option<&mut Body> {
        match id {
            EntityId::Body(i) => self.bodies.get_mut(i as usize)?.as_mut(),
            EntityId::Building(_) => None,
        }
    }

    pub fn building(&self, id: EntityId) -> Option<&Building> {
        match id {
            EntityId::Building(i) => self.buildings.get(i as usize)?.as_ref(),
            EntityId::Body(_) => None,
        }
    }

    pub fn body_ids(&self) -> Vec<EntityId> {
        self.bodies.iter().enumerate().filter(|(_, b)| b.is_some()).map(|(i, _)| EntityId::Body(i as u32)).collect()
    }

    pub(crate) fn next_scan_code(&mut self) -> u16 {
        self.next_scan()
    }

    pub(crate) fn sectors_ref(&self) -> &Vec<Vec<u32>> {
        &self.sectors
    }

    pub(crate) fn building_mut(&mut self, i: u32) -> Option<&mut Building> {
        self.buildings.get_mut(i as usize)?.as_mut()
    }

    fn next_scan(&mut self) -> u16 {
        if self.scan_code == u16::MAX {
            for b in self.buildings.iter_mut().flatten() {
                b.scan = 0;
            }
            for b in self.bodies.iter_mut().flatten() {
                b.scan = 0;
            }
            self.scan_code = 1;
        } else {
            self.scan_code += 1;
        }
        self.scan_code
    }

    fn b(&self, i: usize) -> &Body {
        self.bodies[i].as_ref().unwrap()
    }

    fn bm(&mut self, i: usize) -> &mut Body {
        self.bodies[i].as_mut().unwrap()
    }

    /// Two distinct bodies, mutably.
    fn two(&mut self, i: usize, j: usize) -> (&mut Body, &mut Body) {
        assert_ne!(i, j);
        if i < j {
            let (a, b) = self.bodies.split_at_mut(j);
            (a[i].as_mut().unwrap(), b[0].as_mut().unwrap())
        } else {
            let (a, b) = self.bodies.split_at_mut(i);
            (b[0].as_mut().unwrap(), a[j].as_mut().unwrap())
        }
    }

    // ------------------------------------------------------------ CWorld::Process

    /// One physics frame with timestep `ts` (in 1/50 s frames).
    pub fn process(&mut self, ts: f32) {
        self.last_ts = ts;
        // CTimer: 20 ms per 1/50 s tick.
        self.time_ms += ts as f64 * 20.0;
        self.now_ms = self.time_ms as u32;
        self.frame = self.frame.wrapping_add(1);
        self.effects.lights.clear();
        // CGame::Process: clock and weather before the world.
        self.update_clock_and_weather(ts);
        let mut ctx = Ctx::new(ts);
        ctx.wet_roads = self.weather.wet_roads;
        ctx.now_ms = self.now_ms;
        ctx.cam = self.cam_info();
        self.probe_ped_ground();
        let moving: Vec<usize> = (0..self.bodies.len())
            .filter(|&i| self.bodies[i].as_ref().is_some_and(|b| !b.phys.is_static()))
            .collect();

        // ProcessControl pass (uses last frame's line results), then reset lines.
        for &i in &moving {
            let b = self.bm(i);
            b.logic.process_control(&mut b.phys, &mut b.col, &ctx, &b.lines);
            b.lines = LineHits::default();
            self.process_buoyancy(i, ts);
        }

        // Up to 6 collision passes.
        ctx.later_collision_pass = false;
        ctx.keep_going_after_hit = true;
        for pass in 0..=COLLISION_PASSES {
            for &i in &moving {
                if self.b(i).phys.has_e(ef::IN_SAFE_POSITION) {
                    continue;
                }
                if pass == COLLISION_PASSES {
                    self.bm(i).phys.eflags |= ef::IS_STUCK;
                }
                self.process_collision(i, &ctx);
                if pass == COLLISION_PASSES && !self.b(i).phys.has_e(ef::IN_SAFE_POSITION) {
                    self.bm(i).phys.eflags |= ef::IS_STUCK;
                }
            }
            ctx.later_collision_pass = true;
        }

        // Two shift passes.
        for shift_pass in 0..2 {
            ctx.keep_going_after_hit = shift_pass == 1;
            for &i in &moving {
                if self.b(i).phys.has_e(ef::IN_SAFE_POSITION) {
                    continue;
                }
                self.process_shift(i, &ctx);
                let p = &mut self.bm(i).phys;
                if !p.has_e(ef::IN_SAFE_POSITION) {
                    p.eflags |= ef::IS_STUCK;
                    if shift_pass == 1 && p.status == Status::Player {
                        // Forced damped move for a stuck player (turn speed not damped).
                        p.move_speed *= 0.707f32.powf(ts);
                        p.apply_move_speed(ts);
                        p.apply_turn_speed(ts);
                    }
                }
            }
        }

        self.process_effects(ts);
    }

    /// Body effect hooks, their world requests, then explosions and fires.
    fn process_effects(&mut self, ts: f32) {
        let mut requests: Vec<WorldRequest> = Vec::new();
        let player_in_vehicle =
            self.bodies.iter().flatten().any(|b| b.phys.kind == EntityType::Vehicle && b.phys.status == Status::Player);
        for (i, b) in self.bodies.iter_mut().enumerate() {
            let Some(b) = b else { continue };
            let mut f = FrameFx {
                fx: &mut self.effects,
                requests: &mut requests,
                now_ms: self.now_ms,
                frame: self.frame,
                ts,
                rng: &mut self.rng,
                cam: self.camera_pos,
                cam_planes: self.camera_planes,
                wet_roads: self.weather.wet_roads,
                foggyness: self.weather.foggyness,
                hours: self.clock.hours,
                minutes: self.clock.minutes,
                cam_fwd: self.camera_fwd,
                sprite_brightness: self.timecycle.as_ref().map_or(10.0, |t| t.current.sprite_brightness),
                player_in_vehicle,
                surfaces: &self.surfaces,
            };
            // CPhysical::ApplyFriction scrape sparks (sparks.md 3.2), 8 per contact.
            for sc in std::mem::take(&mut b.phys.scrapes) {
                let fe_a = f.surfaces.info(sc.surface_a).friction_effect;
                let fe_b = f.surfaces.info(sc.surface_b).friction_effect;
                if fe_b == 0 || !(fe_a == 1 || b.phys.kind == EntityType::Vehicle) {
                    continue;
                }
                let sp = sc.dir * (sc.slip * 0.25);
                let force = sc.slip * 12.5;
                let d = sc.dir + sc.normal * 0.1;
                let across = sc.normal.cross(sc.move_speed).normalize_or_zero();
                for _ in 0..8 {
                    let k = f.rng.rand01() * 0.4 - 0.2;
                    f.add_sparks(sc.point + across * k, d, force, 1, sp, false, 0.1, 1.0);
                }
            }
            b.logic.process_effects(EntityId::Body(i as u32), &mut b.phys, &b.col, &mut f);
        }
        for r in requests {
            match r {
                WorldRequest::Explosion { victim, creator, kind, pos, lifetime_ms, cam_shake, no_damage } => {
                    self.add_explosion(victim, creator, kind, pos, lifetime_ms, cam_shake, no_damage);
                }
                WorldRequest::StartFire { target, creator } => {
                    self.start_fire_on(target, creator);
                }
                WorldRequest::Corona(a) => self.register_corona(a),
                WorldRequest::PointLight { ty, pos, dir, range, rgb, fog_type, shadows } => {
                    let cam = self.camera_pos;
                    self.effects.add_point_light(cam, ty, pos, dir, range, rgb, fog_type, shadows);
                }
                WorldRequest::CarLightShadow { car, id, tex, pos, front, side, rgb, max_view_angle } => {
                    self.store_car_light_shadow(car, id, tex, pos, front, side, rgb, max_view_angle);
                }
                WorldRequest::FireInstantHit(h) => self.fire_instant_hit(h),
                WorldRequest::FireProjectile { owner, ty, effect, force, cam } => {
                    self.fire_projectile(owner, ty, effect, force, cam);
                }
                WorldRequest::FireAreaEffect { owner, ty, src, mouse_cam, look_pitch } => {
                    self.fire_area_effect(owner, ty, src, mouse_cam, look_pitch);
                }
                WorldRequest::Detonate => self.use_detonator(),
            }
        }
        self.bullet_traces.update(self.now_ms);
        // CWeapon::UpdateWeapons: shots, explosions, projectiles.
        self.update_shots(ts);
        self.update_explosions(ts);
        self.update_projectiles(ts);
        self.update_fires(ts);
        self.update_creeping();
        self.update_permanent_shadows();
        // Render-time UpdateStaticShadows: drop shadows not re-stored this frame.
        self.update_static_shadows();
    }

    // ------------------------------------------------------------ ProcessCollision

    /// 0x54DFB0 (main path).
    fn process_collision(&mut self, i: usize, ctx: &Ctx) {
        let b = self.bm(i);
        let p = &mut b.phys;
        p.flags &= !(pf::UNK_1000 | pf::IN_SHIFT);
        p.moving_speed = 0.0;
        if !p.has_e(ef::USES_COLLISION) || p.has(pf::NO_COLLISION) || p.status == Status::Simple {
            p.eflags = (p.eflags & !ef::IS_STUCK) | ef::IN_SAFE_POSITION;
            return;
        }
        let saved_elasticity = p.elasticity;
        let saved_move = p.move_speed;
        let saved_matrix = p.matrix;
        let ts0 = ctx.ts;
        let (n_steps, probe_first) = b.logic.collision_steps(&b.phys, ctx.ts);
        let n_steps = n_steps.max(1);
        let step_ts = ts0 / n_steps as f32;

        if probe_first {
            let p = &mut self.bm(i).phys;
            p.apply_speed(ts0);
            p.matrix.reorthogonalise();
            p.flags = (p.flags & !(pf::UNK_1000 | pf::IN_SHIFT)) | pf::PROBE;
            let uses = p.eflags & ef::USES_COLLISION;
            p.eflags &= !ef::USES_COLLISION;
            let hit = self.check_collision(i, ctx);
            let p = &mut self.bm(i).phys;
            p.eflags |= uses;
            p.flags &= !pf::PROBE;
            if !hit {
                return self.mark_safe(i, saved_matrix.pos, saved_elasticity);
            }
            p.matrix = saved_matrix;
            p.move_speed = saved_move;
            if p.is_vehicle() {
                p.elasticity *= 2.0;
            }
        }

        // Swept probing at s/n of the step from the original pose.
        for s in 1..n_steps {
            let sub = Ctx { ts: (s as f32 * step_ts).max(1e-5), ..*ctx };
            self.bm(i).phys.apply_speed(sub.ts);
            let hit = self.check_collision(i, &sub);
            let p = &mut self.bm(i).phys;
            p.matrix = saved_matrix;
            if hit {
                p.elasticity = saved_elasticity;
                return;
            }
        }

        let p = &mut self.bm(i).phys;
        p.apply_speed(ts0);
        p.matrix.reorthogonalise();
        p.flags &= !(pf::UNK_1000 | pf::IN_SHIFT);
        let still = p.move_speed == Vec3::ZERO
            && p.turn_speed == Vec3::ZERO
            && !p.has(pf::UNK_800)
            && p.status != Status::Player
            && p.kind != EntityType::Vehicle
            && p.kind != EntityType::Ped;
        if still {
            return self.mark_safe(i, saved_matrix.pos, saved_elasticity);
        }
        if self.check_collision(i, ctx) {
            let p = &mut self.bm(i).phys;
            p.matrix = saved_matrix;
            p.elasticity = saved_elasticity;
            return;
        }
        self.mark_safe(i, saved_matrix.pos, saved_elasticity);
    }

    fn mark_safe(&mut self, i: usize, old_pos: Vec3, elasticity: f32) {
        let p = &mut self.bm(i).phys;
        p.eflags = (p.eflags & !ef::IS_STUCK) | ef::IN_SAFE_POSITION;
        p.flags &= !(pf::UNK_800 | pf::UNK_1000);
        p.elasticity = elasticity;
        p.moving_speed = (p.matrix.pos - old_pos).length();
    }

    fn bound(&self, i: usize) -> (Vec3, f32) {
        let b = self.b(i);
        (b.phys.matrix.transform(b.col.bound_center), b.col.bound_radius)
    }

    fn sector_range(&self, i: usize) -> (i32, i32, i32, i32) {
        let (c, r) = self.bound(i);
        (sector_coord(c.x - r), sector_coord(c.x + r), sector_coord(c.y - r), sector_coord(c.y + r))
    }

    fn dynamic_order(&self, shift: bool) -> Vec<usize> {
        let kinds: &[EntityType] = if shift {
            &[EntityType::Object, EntityType::Ped, EntityType::Vehicle]
        } else {
            &[EntityType::Vehicle, EntityType::Object, EntityType::Ped]
        };
        let mut out = Vec::new();
        for k in kinds {
            for (j, b) in self.bodies.iter().enumerate() {
                if b.as_ref().is_some_and(|b| b.phys.kind == *k) {
                    out.push(j);
                }
            }
        }
        out
    }

    /// Candidate other entities touching body `i` (buildings via sectors, then bodies).
    fn candidates(&mut self, i: usize, shift: bool) -> Vec<EntityId> {
        let scan = self.next_scan();
        let (c, r) = self.bound(i);
        let (x0, x1, y0, y1) = self.sector_range(i);
        let mut out = Vec::new();
        let mut buildings = Vec::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                for &bi in &self.sectors[(y * SECTORS + x) as usize] {
                    let b = self.buildings[bi as usize].as_mut().unwrap();
                    if b.scan == scan {
                        continue;
                    }
                    b.scan = scan;
                    let bc = b.matrix.transform(b.col.bound_center);
                    if touching(c, r, bc, b.col.bound_radius) {
                        buildings.push(EntityId::Building(bi));
                    }
                }
            }
        }
        if !shift {
            out.extend(buildings.iter().copied());
        }
        let me_ignored = self.b(i).phys.ignored;
        for j in self.dynamic_order(shift) {
            if j == i {
                continue;
            }
            let o = self.b(j);
            if !o.phys.has_e(ef::USES_COLLISION) {
                continue;
            }
            // SpecialEntityPreCollisionStuff: either side's m_pEntityIgnoredCollision.
            if me_ignored == Some(EntityId::Body(j as u32)) || o.phys.ignored == Some(EntityId::Body(i as u32)) {
                continue;
            }
            let oc = o.phys.matrix.transform(o.col.bound_center);
            if touching(c, r, oc, o.col.bound_radius) {
                out.push(EntityId::Body(j as u32));
            }
        }
        if shift {
            out.extend(buildings);
        }
        out
    }

    /// 0x546D00 ProcessEntityCollision: contacts of body `i` against `other`
    /// (also accumulates `i`'s wheel-line hits).
    fn process_entity_collision(&mut self, i: usize, other: EntityId, cps: &mut [ColPoint; MAX_COLPOINTS]) -> usize {
        if self.b(i).logic.as_any().is::<PedLogic>() {
            return self.ped_entity_collision(i, other, cps, self.last_ts);
        }
        let (other_mat, other_col): (Matrix, *const ColModel) = match other {
            EntityId::Building(bi) => {
                let b = self.buildings[bi as usize].as_ref().unwrap();
                (b.matrix, Arc::as_ptr(&b.col))
            }
            EntityId::Body(j) => {
                let b = self.b(j as usize);
                (b.phys.matrix, &b.col as *const ColModel)
            }
        };
        let a = self.bm(i);
        let nl = a.col.lines.len().min(MAX_LINES);
        let mut lp = a.lines.points;
        let mut lv = a.lines.values;
        // SAFETY: `other` is a different entity than body `i` (or a building), and
        // the collision model is only read for the duration of this call.
        let other_col = unsafe { &*other_col };
        let n = process_col_models(&a.phys.matrix, &a.col, &other_mat, other_col, cps, &mut lp[..nl], &mut lv[..nl], false);
        for l in 0..nl {
            if lv[l] < a.lines.values[l] {
                a.lines.values[l] = lv[l];
                a.lines.points[l] = lp[l];
                a.lines.entities[l] = Some(other);
            }
        }
        if n > 0 {
            a.phys.flags |= pf::COLLIDED;
            let other_static = match other {
                EntityId::Building(_) => true,
                EntityId::Body(j) => self.b(j as usize).phys.is_static(),
            };
            if other_static {
                self.bm(i).phys.eflags |= ef::HAS_HIT_WALL;
            }
            if let EntityId::Body(j) = other {
                self.bm(j as usize).phys.flags |= pf::COLLIDED;
            }
        }
        n
    }

    /// 0x54D920 CheckCollision (+ ProcessCollisionSectorList for every candidate).
    fn check_collision(&mut self, i: usize, ctx: &Ctx) -> bool {
        self.bm(i).phys.eflags &= !ef::COLLISION_PROCESSED;
        // Peds: forget the ground entity, standing -> was standing.
        {
            let b = self.bm(i);
            let probe_like = b.phys.flags & (pf::PROBE | pf::IN_SHIFT | pf::UNK_1000) != 0;
            if let (false, Some(ped)) = (probe_like, b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                ped.ground_entity = None;
                if ped.standing {
                    ped.standing = false;
                    ped.was_standing = true;
                }
            }
        }
        let mut result = false;
        for other in self.candidates(i, false) {
            if self.collide(i, other, ctx) == Hit::Hard {
                if self.b(i).phys.has(pf::PROBE) || !ctx.keep_going_after_hit {
                    return true;
                }
                result = true;
            }
        }
        result
    }

    /// One entry of ProcessCollisionSectorList (0x54BA60).
    fn collide(&mut self, i: usize, other: EntityId, ctx: &Ctx) -> Hit {
        self.bm(i).phys.flags &= !pf::UNK_1000;
        let stuck = match other {
            EntityId::Building(_) => {
                let p = &self.b(i).phys;
                p.has(pf::INFINITE_MASS) && p.has_e(ef::IS_STUCK)
            }
            EntityId::Body(_) => false,
        };
        let mut cps = [ColPoint::default(); MAX_COLPOINTS];
        let n = self.process_entity_collision(i, other, &mut cps);
        if n == 0 {
            return Hit::None;
        }
        if self.b(i).phys.has(pf::PROBE) {
            return Hit::Hard;
        }
        let static_path = match other {
            EntityId::Building(_) => true,
            EntityId::Body(j) => {
                let ob = self.b(j as usize);
                let o = &ob.phys;
                let ped_vs_resting_prop = self.b(i).phys.kind == EntityType::Ped
                    && o.kind == EntityType::Object
                    && (ob.logic.uproot_limit().is_some_and(|u| u > 0.0) || o.has(pf::DISABLE_COLLISION_FORCE))
                    && o.move_speed.abs().max_element() < 0.001;
                o.has(pf::COLLIDE_AS_STATIC)
                    || (o.is_static() && ob.logic.uproot_limit().is_none())
                    || ped_vs_resting_prop
            }
        };
        let hit = if static_path {
            self.static_response(i, other, &cps[..n], stuck, ctx)
        } else {
            let EntityId::Body(j) = other else { unreachable!() };
            self.physical_response(i, j as usize, &cps[..n], stuck, ctx)
        };
        if hit == Hit::Hard && self.b(i).phys.last_hit.is_none() {
            self.bm(i).phys.last_hit = Some(other);
        }
        hit
    }

    /// Static path of ProcessCollisionSectorList.
    fn static_response(&mut self, i: usize, other: EntityId, cps: &[ColPoint], stuck: bool, ctx: &Ctx) -> Hit {
        let n = cps.len();
        let other_kind = Some(match other {
            EntityId::Building(_) => EntityType::Building,
            EntityId::Body(j) => self.b(j as usize).phys.kind,
        });
        let other_is_static = true;
        let surfaces = &self.surfaces;
        let a = self.bodies[i].as_mut().unwrap();
        let p = &mut a.phys;
        let (mut count, mut max_imp) = (0usize, 0f32);
        let (mut move_acc, mut turn_acc) = (Vec3::ZERO, Vec3::ZERO);
        let _ = other;
        for cp in cps {
            if !stuck && !cp.is_wheel_a() {
                let Some(imp) = p.apply_collision_alt(ctx, cp, other_is_static, &mut move_acc, &mut turn_acc) else {
                    continue;
                };
                count += 1;
                max_imp = max_imp.max(imp);
                if p.has_e(ef::HAS_CONTACTED) {
                    p.set_damaged_piece_record(imp, cp, 1.0, other_kind);
                    continue;
                }
                let mut adh = surfaces.adhesive_limit(cp) / n as f32;
                if p.is_vehicle() {
                    let class = p.vclass();
                    if class == Some(VehicleClass::Boat) && cp.surface_b == 43 {
                        adh = 0.0;
                    } else {
                        p.set_damaged_piece_record(imp, cp, 1.0, other_kind);
                    }
                    let model = p.vehicle.map(|v| v.model).unwrap_or(0);
                    if model == 441 {
                        adh *= 0.2;
                    } else if class == Some(VehicleClass::Boat) {
                        adh = if cp.normal.z > 0.6 {
                            let g = surfaces.adhesion_group(cp.surface_b);
                            if g == 3 || g == 4 { adh * 3.0 } else { adh }
                        } else {
                            0.0
                        };
                    } else if class == Some(VehicleClass::Train) {
                    } else if p.status == Status::Wrecked {
                        adh *= 3.0;
                    } else if p.matrix.up.z > 0.3
                        && p.move_speed.length_squared() < 0.02
                        && p.turn_speed.length_squared() < 0.01
                    {
                        // Upright, slow car: the wheels handle it.
                        adh = 0.0;
                    } else if p.status == Status::Abandoned || cp.normal.dot(p.matrix.up) < 0.707 {
                        adh = 150.0 / p.mass * adh * imp;
                    }
                    if class == Some(VehicleClass::Train) {
                        adh *= 2.0;
                    }
                } else {
                    adh = 150.0 * adh * imp;
                    p.set_damaged_piece_record(imp, cp, 1.0, other_kind);
                }
                if p.apply_friction_static(ctx, adh, cp) {
                    p.eflags |= ef::HAS_CONTACTED;
                }
            } else if p.apply_soft_collision_static(ctx, cp).is_some()
                && !p.has_e(ef::HAS_CONTACTED)
                && !(cp.surface_a == SURFACE_WHEELBASE && cp.surface_b == SURFACE_WHEELBASE)
            {
                let adh = surfaces.adhesive_limit(cp);
                if p.apply_friction_static(ctx, adh, cp) {
                    p.eflags |= ef::HAS_CONTACTED;
                }
            }
        }
        if count == 0 {
            return Hit::None;
        }
        let inv = 1.0 / count as f32;
        p.move_speed += move_acc * inv;
        p.turn_speed += turn_acc * inv;
        if !ctx.later_collision_pass
            && p.status == Status::Player
            && p.is_vehicle()
            && p.move_speed.x.abs() < 0.2
            && p.move_speed.y.abs() < 0.2
            && !p.has(pf::IN_WATER)
        {
            let k = 0.3 / n as f32;
            p.friction_move.x -= move_acc.x * k;
            p.friction_move.y -= move_acc.y * k;
            p.friction_turn += turn_acc * -0.3 * (1.0 / n as f32);
        }
        Hit::Hard
    }

    /// Physical path of ProcessCollisionSectorList (A and B both movable).
    fn physical_response(&mut self, i: usize, j: usize, cps: &[ColPoint], stuck: bool, ctx: &Ctx) -> Hit {
        let n = cps.len();
        // Static prop that can be knocked loose (object.dat uproot limit).
        if self.b(j).phys.is_static() {
            let limit = self.b(j).logic.uproot_limit().unwrap_or(f32::MAX);
            let a = &self.b(i).phys;
            let mut impulse = 0f32;
            for cp in cps {
                let r = cp.point - a.matrix.pos;
                let v = if a.has(pf::DISABLE_TURN_FORCE) { a.move_speed } else { a.get_speed(r) };
                let vn = v.dot(cp.normal);
                if vn < 0.0 {
                    impulse = impulse.max(-vn * a.mass);
                }
            }
            if impulse > limit {
                let o = &mut self.bm(j).phys;
                o.eflags &= !(ef::IS_STATIC | ef::STATIC_WAITING_FOR_COLLISION);
            } else {
                return self.static_response(i, EntityId::Body(j as u32), cps, stuck, ctx);
            }
        }
        let surfaces = self.surfaces.clone();
        let (a, b) = self.two(i, j);
        let (pa, pb) = (&mut a.phys, &mut b.phys);
        let a_contacted = pa.has_e(ef::HAS_CONTACTED);
        let b_contacted = pb.has_e(ef::HAS_CONTACTED);
        let mut num_soft = 0usize;
        let mut max_imp_b = 0f32;
        let info = PairInfo::default();
        if a_contacted && b_contacted {
            for cp in cps {
                if !stuck && !cp.is_wheel_a() && !cp.is_wheel_b() {
                    if let Some((ia, ib)) = pair::apply_collision(ctx, pa, pb, cp, info, false) {
                        pa.set_damaged_piece_record(ia, cp, 1.0, Some(pb.kind));
                        pb.set_damaged_piece_record(ib, cp, -1.0, Some(pa.kind));
                        max_imp_b = max_imp_b.max(ib.abs());
                    }
                } else {
                    num_soft += 1;
                    let _ = pair::apply_collision(ctx, pa, pb, cp, info, true);
                }
            }
        } else {
            // Save and clear the friction of whichever side already had contact.
            let save_a = (pa.friction_move, pa.friction_turn);
            let save_b = (pb.friction_move, pb.friction_turn);
            if a_contacted {
                pa.eflags &= !ef::HAS_CONTACTED;
                pa.friction_move = Vec3::ZERO;
                pa.friction_turn = Vec3::ZERO;
            }
            if b_contacted {
                pb.eflags &= !ef::HAS_CONTACTED;
                pb.friction_move = Vec3::ZERO;
                pb.friction_turn = Vec3::ZERO;
            }
            let neither = !a_contacted && !b_contacted;
            for cp in cps {
                // Quirk: with neither contacted, the wheel test checks piece A twice.
                let wheel = if neither { cp.is_wheel_a() } else { cp.is_wheel_a() || cp.is_wheel_b() };
                if !stuck && !wheel {
                    if let Some((ia, ib)) = pair::apply_collision(ctx, pa, pb, cp, info, false) {
                        pa.set_damaged_piece_record(ia, cp, 1.0, Some(pb.kind));
                        pb.set_damaged_piece_record(ib, cp, -1.0, Some(pa.kind));
                        max_imp_b = max_imp_b.max(ib.abs());
                        let mut adh = surfaces.adhesive_limit(cp) / n as f32;
                        if pa.is_vehicle()
                            && pb.is_vehicle()
                            && (pa.move_speed.length_squared() > 0.02 || pa.turn_speed.length_squared() > 0.01)
                        {
                            adh = adh * 1.0 * ia;
                        }
                        if pb.is_static() {
                            if pa.apply_friction_static(ctx, adh, cp) {
                                pa.eflags |= ef::HAS_CONTACTED;
                            }
                        } else if pair::apply_friction(ctx, pa, pb, adh, cp) {
                            pa.eflags |= ef::HAS_CONTACTED;
                            pb.eflags |= ef::HAS_CONTACTED;
                        }
                    }
                } else {
                    num_soft += 1;
                    let _ = pair::apply_collision(ctx, pa, pb, cp, info, true);
                }
            }
            if a_contacted && !pa.has_e(ef::HAS_CONTACTED) {
                pa.eflags |= ef::HAS_CONTACTED;
                (pa.friction_move, pa.friction_turn) = save_a;
            }
            if b_contacted && !pb.has_e(ef::HAS_CONTACTED) {
                pb.eflags |= ef::HAS_CONTACTED;
                (pb.friction_move, pb.friction_turn) = save_b;
            }
        }
        if pb.status == Status::Simple {
            pb.status = Status::Physics;
        }
        // A vehicle hitting a ped (0x5F0360 KillPedWithCar).
        if pa.is_vehicle() && pb.kind == EntityType::Ped && max_imp_b > 0.0 {
            let hit_wall = pb.has_e(ef::HAS_HIT_WALL);
            let fast = pa.move_speed.length_squared() > 0.0025;
            if let Some(ped) = b.logic.as_any_mut().downcast_mut::<PedLogic>() {
                if !ped.is_player || (hit_wall && fast) {
                    kill_ped_with_car(pa, pb, ped, max_imp_b);
                }
            }
        }
        if n > num_soft { Hit::Hard } else { Hit::None }
    }

    // ------------------------------------------------------------ ProcessShift

    /// 0x54DB10
    fn process_shift(&mut self, i: usize, ctx: &Ctx) {
        let (x0, x1, y0, y1) = self.sector_range(i);
        let _ = (x0, x1, y0, y1);
        let p = &mut self.bm(i).phys;
        p.moving_speed = 0.0;
        if p.status == Status::Simple || p.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS | pf::DISABLE_Z) != 0 {
            if p.flags & (pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS | pf::DISABLE_Z) != 0 {
                p.turn_speed = Vec3::ZERO;
            }
            p.eflags = (p.eflags & !ef::IS_STUCK) | ef::IN_SAFE_POSITION;
            return;
        }
        if p.has_e(ef::HAS_HIT_WALL) && (p.kind != EntityType::Ped || ctx.keep_going_after_hit) {
            let k = 0.707f32.powf(ctx.ts);
            p.move_speed *= k;
            p.turn_speed *= k;
        }
        let saved = p.matrix;
        p.apply_speed(ctx.ts);
        p.matrix.reorthogonalise();
        let is_vehicle = p.is_vehicle();
        if is_vehicle {
            p.flags |= pf::IN_SHIFT;
        }
        let shifted = self.process_shift_list(i, ctx);
        self.bm(i).phys.flags &= !pf::IN_SHIFT;
        if shifted || is_vehicle {
            let mut hit = false;
            for other in self.candidates(i, false) {
                if self.collide(i, other, ctx) == Hit::Hard {
                    if !ctx.keep_going_after_hit {
                        self.bm(i).phys.matrix = saved;
                        return;
                    }
                    hit = true;
                }
            }
            if hit {
                self.bm(i).phys.matrix = saved;
                return;
            }
        }
        let p = &mut self.bm(i).phys;
        p.eflags = (p.eflags & !ef::IS_STUCK) | ef::IN_SAFE_POSITION;
        p.moving_speed = (p.matrix.pos - saved.pos).length();
    }

    /// 0x546670 ProcessShiftSectorList (over all candidates).
    fn process_shift_list(&mut self, i: usize, ctx: &Ctx) -> bool {
        let mut shift = Vec3::ZERO;
        let mut max_depth = 0f32;
        let mut count = 0usize;
        let hit_wall = self.b(i).phys.has_e(ef::HAS_HIT_WALL);
        let self_kind = self.b(i).phys.kind;
        for other in self.candidates(i, true) {
            let (blocky, other_kind) = match other {
                EntityId::Building(_) => (true, EntityType::Building),
                EntityId::Body(j) => {
                    let o = &self.b(j as usize).phys;
                    let blocky = (o.kind == EntityType::Object && o.has(pf::DISABLE_COLLISION_FORCE))
                        || (self_kind == EntityType::Ped && o.kind == EntityType::Object && o.is_static());
                    (blocky, o.kind)
                }
            };
            if hit_wall && !blocky {
                continue;
            }
            let mut cps = [ColPoint::default(); MAX_COLPOINTS];
            let n = self.process_entity_collision(i, other, &mut cps);
            for cp in &cps[..n] {
                if cp.depth <= 0.0 || cp.is_wheel_b() {
                    continue;
                }
                count += 1;
                let nrm = cp.normal;
                if self_kind == EntityType::Vehicle && other_kind == EntityType::Ped && nrm.z < 0.0 {
                    shift += Vec3::new(nrm.x, nrm.y, 0.0);
                } else if self_kind == EntityType::Ped && other_kind == EntityType::Object && nrm.z.abs() > 0.1 {
                    // counted, not added
                } else {
                    shift += nrm;
                }
                max_depth = max_depth.max(cp.depth);
            }
        }
        if count == 0 {
            return false;
        }
        let l = shift.length();
        if l > 1.0 {
            shift /= l;
        }
        let delta = if shift.z < -0.5 {
            shift * max_depth * 0.75
        } else if self_kind == EntityType::Ped {
            shift * (1.5 * max_depth).clamp(0.005, 0.3)
        } else {
            shift * max_depth * 1.5
        };
        let p = &mut self.bm(i).phys;
        p.matrix.pos += delta;
        if self_kind == EntityType::Vehicle {
            p.move_speed += Vec3::new(shift.x, shift.y, shift.z.max(0.0)) * 0.008 * ctx.ts;
        }
        true
    }

    // ------------------------------------------------------------ queries

    /// Simplified `CWorld::ProcessLineOfSight` over buildings and bodies:
    /// nearest hit along `start..end` as (entity, fraction, colpoint).
    pub fn line_of_sight(
        &mut self,
        start: Vec3,
        end: Vec3,
        buildings_only: bool,
        ignore: Option<EntityId>,
    ) -> Option<(EntityId, f32, ColPoint)> {
        self.process_line_of_sight(start, end, &LosOpts { bodies: !buildings_only, ignore, ..Default::default() })
    }

    /// `CWeather::Wavyness`.
    pub fn wavyness(&self) -> f32 {
        (self.weather.wind_clipped + 0.3).min(1.0)
    }

    /// `GetWaterLevel` with the world's water and weather.
    pub fn water_level(&self, p: Vec3, touching: bool) -> Option<(f32, Vec3)> {
        self.water.as_ref()?.level(p.x, p.y, p.z, touching, self.wavyness(), self.now_ms)
    }

    /// The ProcessBuoyancy calls of CAutomobile / CPed / CObject::ProcessControl.
    fn process_buoyancy(&mut self, i: usize, ts: f32) {
        use crate::water::{BuoyancyIn, process_buoyancy};
        let Some(water) = self.water.clone() else { return };
        let wavy = self.wavyness();
        let now = self.now_ms;
        let b = self.bm(i);
        let kind = b.phys.kind;
        let touching = b.phys.flags & pf::TOUCHING_WATER != 0;
        let bconst = match kind {
            EntityType::Vehicle => match b.logic.as_any().downcast_ref::<crate::automobile::Automobile>() {
                Some(car) => car.buoyancy,
                None => return,
            },
            EntityType::Ped => {
                let Some(ped) = b.logic.as_any().downcast_ref::<PedLogic>() else { return };
                if !b.phys.has_e(ef::USES_COLLISION) {
                    return; // in a vehicle
                }
                let k = if ped.tasks.health.alive() { 1.1 } else { 1.8 };
                k * b.phys.mass * 0.008
            }
            _ => match b.logic.buoyancy(&b.phys) {
                Some(v) => v,
                None => return,
            },
        };
        let input = BuoyancyIn {
            matrix: &b.phys.matrix,
            bbox_min: b.col.bbox_min,
            bbox_max: b.col.bbox_max,
            is_ped: kind == EntityType::Ped,
            touching,
            b: bconst,
            mass: b.phys.mass,
            move_z: b.phys.move_speed.z,
            ts,
        };
        let Some((turn, force, level)) = process_buoyancy(&water, &input, wavy, now) else {
            b.phys.flags &= !(pf::TOUCHING_WATER | pf::IN_WATER);
            if let Some(car) = b.logic.as_any_mut().downcast_mut::<crate::automobile::Automobile>() {
                car.sinking = false;
                car.buoyancy = car.h.buoyancy_constant;
            }
            return;
        };
        let b = self.bm(i);
        match kind {
            EntityType::Ped => {
                b.phys.flags |= pf::TOUCHING_WATER | pf::IN_WATER;
                b.phys.apply_move_force(force);
                let mass = b.phys.mass;
                let deep = force.z / mass > ts * 0.008 || b.phys.matrix.pos.z + 0.6 < level;
                let Body { phys, logic, .. } = b;
                let ped = logic.as_any_mut().downcast_mut::<PedLogic>().unwrap();
                if !deep {
                    // Wading: the player's head under water loses breath.
                    if ped.is_player {
                        ped.handle_breath(phys.matrix.pos.z + 0.8 < level, ts);
                    }
                    return;
                }
                // Swimming (the swim task is not ported: the ped floats and drifts).
                ped.standing = false;
                let f = 0.9f32.powf(ts);
                phys.move_speed.x *= f;
                phys.move_speed.y *= f;
                if phys.move_speed.z < 0.0 {
                    phys.move_speed.z *= f;
                }
                if ped.is_player {
                    ped.handle_breath(phys.matrix.pos.z + 0.8 < level, ts);
                }
            }
            EntityType::Vehicle => {
                // CAutomobile::ProcessBuoyancy (0x6A8C00).
                b.phys.flags |= pf::TOUCHING_WATER;
                let mass = b.phys.mass;
                let Body { phys, logic, .. } = b;
                let car = logic.as_any_mut().downcast_mut::<crate::automobile::Automobile>().unwrap();
                let mut r = force.z / (ts.max(0.01) * mass * 0.008);
                if mass * 0.008 > car.buoyancy {
                    r *= (mass * 0.008 / car.buoyancy) * 1.05;
                }
                if phys.flags & pf::HEAVY != 0 {
                    r *= 1.5;
                }
                let damp = (1.0 - r * 0.05).max(0.5).powf(ts);
                phys.move_speed *= damp;
                phys.turn_speed *= damp;
                phys.apply_move_force(force);
                phys.apply_turn_force(force, turn);
                let airborne = car.comp.iter().any(|&c| c >= 1.0);
                let deep = r >= 1.0 || (r > 0.6 && airborne);
                if !deep {
                    phys.flags &= !pf::IN_WATER;
                    car.sinking = false;
                    return;
                }
                car.sinking = true;
                phys.flags |= pf::IN_WATER;
                if phys.move_speed.z < -0.1 {
                    phys.move_speed.z = -0.1;
                }
                if car.buoyancy > mass * 0.0064 {
                    car.buoyancy -= mass * 8e-6;
                }
                if car.buoyancy < mass * 0.008 {
                    car.engine_on = false;
                }
            }
            _ => {
                // CObject::ProcessControl.
                b.phys.flags |= pf::TOUCHING_WATER | pf::IN_WATER;
                b.phys.eflags &= !ef::IS_STATIC;
                b.phys.apply_move_force(force);
                b.phys.apply_turn_force(force, turn);
                let f = 0.97f32.powf(ts);
                b.phys.move_speed *= f;
                b.phys.turn_speed *= f;
            }
        }
    }

    /// The player ped's body.
    pub fn player_id(&self) -> Option<EntityId> {
        self.bodies.iter().enumerate().find_map(|(i, b)| {
            let b = b.as_ref()?;
            b.logic.as_any().downcast_ref::<PedLogic>().filter(|p| p.is_player).map(|_| EntityId::Body(i as u32))
        })
    }

    /// The camera as the ped tasks see it.
    pub fn cam_info(&self) -> crate::CamInfo {
        crate::CamInfo {
            pos: self.camera_pos,
            front: self.camera_fwd,
            up: self.camera_up,
            fov: self.camera_fov,
            aspect: self.camera_aspect,
            mode: self.camera_mode,
            orientation: self.camera_orientation,
        }
    }

    /// `CWorld::ProcessVerticalLine(pos, pos.z - 4.0)` under every moving ped, for the
    /// in-air test (`IsInAir`, 1.5) and `CTaskSimpleInAir` (4.0 / 1.3).
    fn probe_ped_ground(&mut self) {
        let peds: Vec<(u32, Vec3)> = self
            .bodies
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.as_ref().filter(|b| b.phys.kind == EntityType::Ped && !b.phys.is_static()).map(|b| (i as u32, b.phys.matrix.pos)))
            .collect();
        for (i, pos) in peds {
            let id = EntityId::Body(i);
            let hit = self
                .process_line_of_sight(pos, pos - Vec3::new(0.0, 0.0, 4.0), &LosOpts { ignore: Some(id), ..Default::default() })
                .map(|(_, _, cp)| cp.point.z);
            if let Some(ped) = self.bodies[i as usize].as_mut().and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                ped.ground_below = hit;
            }
        }
    }

    /// `CWorld::ProcessLineOfSight` with the options the weapon code uses: skipping
    /// see-through / shoot-through surfaces (per primitive, as CCollision::ProcessLineOfSight)
    /// and testing vehicles' tyres (`bIncludeCarTyres`).
    pub fn process_line_of_sight(&mut self, start: Vec3, end: Vec3, o: &LosOpts) -> Option<(EntityId, f32, ColPoint)> {
        use crate::collision::{ColLine, process_line_box, process_line_sphere, process_line_triangle};
        let scan = self.next_scan();
        let mut best: Option<(EntityId, f32, ColPoint)> = None;
        let mut min_t = 1.0f32;
        let seg = end - start;
        let seg_len2 = seg.length_squared().max(1e-12);
        let surfaces = &self.surfaces;
        let skip = |m: u8| {
            let i = surfaces.info(m);
            (o.see_through && i.see_through) || (o.shoot_through && i.shoot_through)
        };
        let filtering = o.see_through || o.shoot_through;
        let mut test = |id: EntityId, mat: &Matrix, col: &ColModel, tyres: &[ColSphere], min_t: &mut f32| {
            // Reject by bounding sphere (distance from its centre to the segment).
            let c = mat.transform(col.bound_center);
            let t0 = ((c - start).dot(seg) / seg_len2).clamp(0.0, 1.0);
            let r = col.bound_radius + if tyres.is_empty() { 0.0 } else { 1.0 };
            if (start + seg * t0 - c).length_squared() > r * r {
                return;
            }
            let inv = mat.inverse();
            let l = ColLine { start: inv.transform(start), end: inv.transform(end) };
            if tyres.is_empty() && !line_hits_box(l.start, l.end, col.bbox_min, col.bbox_max) {
                return;
            }
            let mut cp = ColPoint::default();
            let mut t = *min_t;
            for sp in col.spheres.iter().chain(tyres) {
                if !filtering || !skip(sp.surf.material) {
                    process_line_sphere(&l, sp, &mut cp, &mut t);
                }
            }
            for bx in &col.boxes {
                if !filtering || !skip(bx.surf.material) {
                    process_line_box(&l, bx, &mut cp, &mut t);
                }
            }
            for k in 0..col.tris.len() {
                if !filtering || !skip(col.tris[k].material) {
                    process_line_triangle(&l, col, k, &mut cp, &mut t);
                }
            }
            if t < *min_t {
                *min_t = t;
                cp.point = mat.transform(cp.point);
                cp.normal = mat.rotate(cp.normal);
                best = Some((id, t, cp));
            }
        };
        if o.buildings {
            let (lo, hi) = (start.min(end), start.max(end));
            for y in sector_coord(lo.y)..=sector_coord(hi.y) {
                for x in sector_coord(lo.x)..=sector_coord(hi.x) {
                    for &bi in &self.sectors[(y * SECTORS + x) as usize] {
                        let b = self.buildings[bi as usize].as_mut().unwrap();
                        if b.scan == scan || o.ignore == Some(EntityId::Building(bi)) {
                            continue;
                        }
                        b.scan = scan;
                        test(EntityId::Building(bi), &b.matrix, &b.col, &[], &mut min_t);
                    }
                }
            }
        }
        if o.bodies {
            for (j, b) in self.bodies.iter().enumerate() {
                let id = EntityId::Body(j as u32);
                let Some(b) = b.as_ref().filter(|_| o.ignore != Some(id) && o.ignore2 != Some(id)) else { continue };
                if !b.phys.has_e(ef::USES_COLLISION) && b.phys.kind != EntityType::Ped {
                    continue;
                }
                if !o.peds && b.phys.kind == EntityType::Ped {
                    continue;
                }
                let tyres = if o.car_tyres && b.phys.kind == EntityType::Vehicle {
                    b.logic.tyre_spheres(&b.col)
                } else {
                    Vec::new()
                };
                test(id, &b.phys.matrix, &b.col, &tyres, &mut min_t);
            }
        }
        best
    }
}

/// `CWorld::ProcessLineOfSight` flags plus the line-test globals.
#[derive(Debug, Clone, Copy)]
pub struct LosOpts {
    pub buildings: bool,
    /// Vehicles, peds and objects.
    pub bodies: bool,
    /// Peds among the bodies.
    pub peds: bool,
    /// `seeThrough`: skip see-through surfaces.
    pub see_through: bool,
    /// `shootThrough`: skip shoot-through surfaces.
    pub shoot_through: bool,
    /// `CWorld::bIncludeCarTyres`.
    pub car_tyres: bool,
    /// `CWorld::pIgnoreEntity` (and the caller's own entity).
    pub ignore: Option<EntityId>,
    pub ignore2: Option<EntityId>,
}

impl Default for LosOpts {
    fn default() -> Self {
        Self {
            buildings: true,
            bodies: true,
            peds: true,
            see_through: false,
            shoot_through: false,
            car_tyres: false,
            ignore: None,
            ignore2: None,
        }
    }
}

impl World {
    /// 0x5E2530 CPed::ProcessEntityCollision (ground line, snap, ceiling probe,
    /// wall-normal flattening). Ped2 sensors and slope-push contacts not ported.
    fn ped_entity_collision(&mut self, i: usize, other: EntityId, cps: &mut [ColPoint; MAX_COLPOINTS], ts: f32) -> usize {
        let (other_mat, other_col, other_kind, other_static_like): (Matrix, *const ColModel, EntityType, bool) = match other {
            EntityId::Building(bi) => {
                let b = self.buildings[bi as usize].as_ref().unwrap();
                (b.matrix, Arc::as_ptr(&b.col), EntityType::Building, true)
            }
            EntityId::Body(j) => {
                let b = self.b(j as usize);
                let st = b.phys.is_static() || b.phys.has(pf::COLLIDE_AS_STATIC);
                (b.phys.matrix, &b.col as *const ColModel, b.phys.kind, st)
            }
        };
        let other_mv = match other {
            EntityId::Body(j) => self.b(j as usize).phys.move_speed,
            EntityId::Building(_) => Vec3::ZERO,
        };
        let soft_surfaces = self.surfaces.clone();
        let dn = self.clock.dn_balance();
        // SAFETY: `other` is never body `i`; its model is only read during this call.
        let other_col = unsafe { &*other_col };
        let body = self.bodies[i].as_mut().unwrap();
        let Body { phys, logic, col: base_col, .. } = body;
        let ped = logic.as_any_mut().downcast_mut::<PedLogic>().unwrap();

        let lines_on = phys.flags & (pf::PROBE | pf::IN_SHIFT | pf::UNK_1000) == 0 && other_kind != EntityType::Ped;
        let mut col = base_col.clone();
        let top = 0.95f32;
        if lines_on {
            col.lines = ped_lines(ped.was_standing, ped.ceiling_probe, ts);
            col.bbox_min.z = col.lines[0].end.z;
            if col.lines.len() == 2 {
                col.bound_radius = col.lines[1].end.z;
                col.bbox_max.z = col.lines[1].end.z;
            } else {
                col.bound_radius = col.lines[0].end.z.abs();
            }
        }
        let nl = col.lines.len();
        let mut lp = [ColPoint::default(); 2];
        let mut lv = [1.0f32; 2];
        let mut n = process_col_models(&phys.matrix, &col, &other_mat, other_col, cps, &mut lp[..nl], &mut lv[..nl], false);
        let z_before = phys.matrix.pos.z;
        let ceiling_ok = matches!(other_kind, EntityType::Building) || other_static_like;
        if lines_on {
            if lv[0] < 1.0 {
                let gz = lp[0].point.z;
                if !ped.standing || gz + 1.0 > phys.matrix.pos.z {
                    let down_facing = cps[..n].iter().any(|c| c.normal.z < -0.867);
                    if !ped.standing && matches!(other_kind, EntityType::Vehicle | EntityType::Object) {
                        ped.ground_entity = Some(other);
                    }
                    if nl == 2 && lv[1] < 1.0 && lp[1].point.z < ped.ceiling_z && ceiling_ok {
                        ped.ceiling_z = lp[1].point.z;
                    }
                    if !down_facing {
                        if ped.ceiling_z >= NO_CEILING {
                            phys.matrix.pos.z = gz + 1.0;
                        } else {
                            let mut z = gz + 1.0;
                            if gz + top + 1.0 > ped.ceiling_z {
                                z = z.min(ped.ceiling_z - top);
                            }
                            phys.matrix.pos.z = z;
                        }
                    }
                    // Contact-surface brightness (+0x12C): the day/night colpoint lighting / 30;
                    // players ease toward it at ts·0.1, other peds take it at once.
                    // [Not ported: standing on a vehicle copies the vehicle's value.]
                    let target = crate::bullet::col_lighting(lp[0].lighting_b, 0.5, dn);
                    ped.lighting = if ped.is_player {
                        (1.0 - ts * 0.1) * ped.lighting + target * ts * 0.1
                    } else {
                        target
                    };
                    ped.ground_normal = lp[0].normal;
                    ped.ground_surface = lp[0].surface_b;
                }
                // Landing (ped.md §4.1 / §4.5): fall damage, type 54, piece 3.
                if !ped.was_standing && !ped.standing {
                    let rel = phys.move_speed - other_mv;
                    let h = (rel.x * rel.x + rel.y * rel.y).sqrt();
                    let ignored = phys.ignored == Some(other);
                    let dir = if rel.x.abs() <= 0.01 && rel.y.abs() <= 0.01 {
                        2
                    } else {
                        crate::peddamage::local_direction(ped.cur_rot, -glam::Vec2::new(rel.x, rel.y))
                    };
                    let mut dmg = None;
                    if (h > 0.33 || rel.z < -0.25) && !ignored {
                        let soft = soft_surfaces.info(lp[0].surface_b).soft_landing;
                        let (sv, v0) = if soft { (0.375, -0.375) } else { (0.25, -0.25) };
                        let mut d = (h - sv).max(0.0) * 100.0 + (v0 - rel.z).max(0.0) * 400.0;
                        if rel.z < -0.6 {
                            d = 500.0;
                        }
                        dmg = Some(d);
                    } else if ped.clump.as_deref().is_some_and(|c| c.get(crate::anim::anim_id::FALL_FALL).is_some()) && rel.z < ts * -0.016 {
                        dmg = Some(15.0);
                    }
                    if let Some(d) = dmg.filter(|d| *d > 0.0) {
                        ped.pending_damage.push(crate::peddamage::DamageIn {
                            src: None,
                            src_pos: None,
                            ty: 54,
                            damage: d,
                            piece: 3,
                            dir,
                        });
                    }
                }
                ped.standing = true;
                phys.move_speed.z = 0.0;
                if z_before + 0.1 < phys.matrix.pos.z && ped.is_player {
                    // Re-test the spheres at the stepped-up height.
                    let mut plain = col.clone();
                    plain.lines.clear();
                    n = process_col_models(&phys.matrix, &plain, &other_mat, other_col, cps, &mut [], &mut [], false);
                }
            } else if nl == 2 && lv[1] < 1.0 && lp[1].point.z < ped.ceiling_z && ceiling_ok {
                ped.ceiling_z = lp[1].point.z;
                if ped.standing && top + phys.matrix.pos.z > lp[1].point.z {
                    phys.matrix.pos.z = lp[1].point.z - top;
                }
            }
        }
        // Against static geometry, wall normals get a unit horizontal part (was standing only).
        if other_static_like && ped.was_standing && n > 0 {
            for cp in cps[..n].iter_mut() {
                let mut nn = cp.normal;
                let l = (nn.x * nn.x + nn.y * nn.y).sqrt();
                if l != 0.0 {
                    nn.x /= l;
                    nn.y /= l;
                }
                cp.normal = normalise(nn);
            }
        }
        if n > 0 || lv[0] < 1.0 {
            phys.flags |= pf::COLLIDED;
            if n > 0 && other_static_like {
                phys.eflags |= ef::HAS_HIT_WALL;
            }
        }
        n
    }
}

/// 0x5F0360 CPed::KillPedWithCar (velocity assignment + braking impulse on the car).
fn kill_ped_with_car(car: &mut Physical, ped: &mut Physical, state: &mut PedLogic, impulse: f32) {
    let big = impulse > 12.0 && !state.is_player;
    if !big {
        let threshold = if state.is_player { 10.0 } else { 6.0 };
        if impulse <= threshold && !(ped.last_collision_impact_velocity.z < -0.8 && impulse > 3.0) {
            return;
        }
    }
    let v = ped.matrix.pos - car.matrix.pos;
    if big {
        ped.move_speed = car.move_speed * 0.9;
    } else if ped.has_e(ef::HAS_HIT_WALL) {
        ped.move_speed = Vec3::ZERO;
    } else {
        ped.move_speed = car.move_speed * 0.75;
    }
    ped.move_speed.z = 0.0;
    state.standing = false;
    state.knocked_down = 1.0;
    // Damage: the big hit 1000 (NPCs), the small hit 30, type 49 rammed by car, piece 3.
    let to_car = car.matrix.pos - ped.matrix.pos;
    let dir = crate::peddamage::local_direction(state.cur_rot, glam::Vec2::new(to_car.x, to_car.y));
    state.pending_damage.push(crate::peddamage::DamageIn {
        src: None,
        src_pos: Some(car.matrix.pos),
        ty: 49,
        damage: if big { 1000.0 } else { 30.0 },
        piece: 3,
        dir,
    });
    // Braking reaction on the car.
    let up = car.matrix.up;
    let vp = v - up * v.dot(up);
    let mut n = normalise(vp);
    let vd = n.dot(car.move_speed);
    n.z -= 0.2;
    let k = if car.vclass() == Some(VehicleClass::Bike) { -0.75 } else { -0.5 };
    let m = car.mass.min(1600.0);
    let j = n * (vd * k * m * (car.mass / 1600.0).min(1.0));
    car.apply_force(j, vp * 0.25, true);
}

/// Segment vs AABB (slab test), inclusive.
fn line_hits_box(a: Vec3, b: Vec3, min: Vec3, max: Vec3) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for k in 0..3 {
        if d[k].abs() < 1e-9 {
            if a[k] < min[k] || a[k] > max[k] {
                return false;
            }
        } else {
            let inv = 1.0 / d[k];
            let (mut n, mut f) = ((min[k] - a[k]) * inv, (max[k] - a[k]) * inv);
            if n > f {
                std::mem::swap(&mut n, &mut f);
            }
            t0 = t0.max(n);
            t1 = t1.min(f);
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

fn touching(c1: Vec3, r1: f32, c2: Vec3, r2: f32) -> bool {
    // CEntity::GetIsTouching (0x5344B0)
    (c1 - c2).length_squared() < (r1 + r2) * (r1 + r2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::{ColSphere, ColTriangle, Surf, TrianglePlane};

    fn ground() -> Arc<ColModel> {
        let verts = vec![
            Vec3::new(-50.0, -50.0, 0.0),
            Vec3::new(50.0, -50.0, 0.0),
            Vec3::new(50.0, 50.0, 0.0),
            Vec3::new(-50.0, 50.0, 0.0),
        ];
        let tris = vec![
            ColTriangle { v: [0, 2, 1], material: 1, light: 0 },
            ColTriangle { v: [0, 3, 2], material: 1, light: 0 },
        ];
        let planes = tris
            .iter()
            .map(|t| TrianglePlane::new(verts[t.v[0] as usize], verts[t.v[1] as usize], verts[t.v[2] as usize]))
            .collect();
        Arc::new(ColModel {
            bbox_min: Vec3::new(-50.0, -50.0, -0.1),
            bbox_max: Vec3::new(50.0, 50.0, 0.1),
            bound_center: Vec3::ZERO,
            bound_radius: 71.0,
            verts,
            tris,
            planes,
            ..Default::default()
        })
    }

    fn crate_box(z: f32) -> (Physical, ColModel) {
        let mut p = Physical::new(EntityType::Object, Matrix { pos: Vec3::new(0.0, 0.0, z), ..Matrix::IDENTITY });
        p.mass = 50.0;
        p.turn_mass = 20.0;
        p.air_resistance = 0.99;
        p.elasticity = 0.1;
        let col = ColModel {
            bbox_min: Vec3::splat(-0.5),
            bbox_max: Vec3::splat(0.5),
            bound_radius: 0.5,
            spheres: vec![ColSphere { center: Vec3::ZERO, radius: 0.5, surf: Surf::default() }],
            ..Default::default()
        };
        (p, col)
    }

    #[test]
    fn dropped_ball_comes_to_rest_on_ground() {
        let mut w = World::default();
        w.add_building(Matrix::IDENTITY, ground());
        let (p, col) = crate_box(3.0);
        let id = w.add_body(p, col, Box::new(PlainLogic));
        for _ in 0..300 {
            w.process(1.0);
        }
        let b = w.body(id).unwrap();
        let z = b.phys.matrix.pos.z;
        assert!(z > 0.3 && z < 0.6, "resting height {z}");
        assert!(b.phys.move_speed.length() < 0.02, "speed {:?}", b.phys.move_speed);
    }

    #[test]
    fn explosion_throws_a_crate_and_leaves_ground_fires() {
        use crate::effects::{ExplosionType, FxCmd};
        let mut w = World::default();
        w.add_building(Matrix::IDENTITY, ground());
        let (p, col) = crate_box(0.5);
        let id = w.add_body(p, col, Box::new(PlainLogic));
        w.process(1.0);
        // A rocket 2 units away: full strength, pushed away and upward.
        w.add_explosion(None, None, ExplosionType::Rocket, Vec3::new(-2.0, 0.0, 0.2), 0, -1.0, false);
        let v = w.body(id).unwrap().phys.move_speed;
        // impulse = m/1400 * f * F = 50/1400 * 300 -> 0.214 units/tick along the blast direction.
        assert!((v.length() - 300.0 / 1400.0).abs() < 0.01 && v.z > 0.0, "crate speed {v:?}");
        let names: Vec<&str> = w
            .effects
            .cmds
            .iter()
            .filter_map(|c| if let FxCmd::Create { name, .. } = c { Some(*name) } else { None })
            .collect();
        assert_eq!(names[0], "explosion_small");
        assert!(!w.effects.cam_shakes.is_empty());
        // Ground fires (5.6..8.4 s, x1..1.3) may spread for 3 generations of 20..26 s
        // creeping fires, and merges add 7 s per tier; all gone within 3 minutes.
        for _ in 0..(50 * 180) {
            w.process(1.0);
        }
        assert!(w.fires.iter().all(|f| !f.active));
    }

    #[test]
    fn strong_fire_decays_through_the_tiers() {
        use crate::effects::FxCmd;
        let mut w = World::default();
        w.add_building(Matrix::IDENTITY, ground());
        let i = w.start_fire_at(Vec3::new(3.0, 3.0, 0.0), None, 1000, 0).unwrap();
        w.fires[i].strength = 2.5;
        w.effects.cmds.clear();
        for _ in 0..(50 * 16) {
            w.process(1.0);
        }
        let names: Vec<&str> = w
            .effects
            .cmds
            .iter()
            .filter_map(|c| if let FxCmd::Create { name, .. } = c { Some(*name) } else { None })
            .collect();
        assert_eq!(names, ["fire_med", "fire"]);
        assert!(!w.fires[i].active);
    }

    #[test]
    fn explosion_scorch_projects_onto_the_ground() {
        use crate::effects::ExplosionType;
        let mut w = World::default();
        w.add_building(Matrix::IDENTITY, ground());
        w.add_explosion(None, None, ExplosionType::Grenade, Vec3::new(1.0, 2.0, 0.5), 0, -1.0, true);
        w.process(1.0);
        let s: Vec<_> = w.shadows.statics.iter().flatten().collect();
        assert_eq!(s.len(), 1, "one scorch");
        let polys = &s[0].polys;
        assert!(!polys.is_empty(), "polygons on the up-facing ground");
        let area: f32 = polys
            .iter()
            .map(|p| {
                let v = &p.verts;
                (1..v.len() - 1).map(|k| (v[k].0 - v[0].0).cross(v[k + 1].0 - v[0].0).length() * 0.5).sum::<f32>()
            })
            .sum();
        assert!((area - 256.0).abs() < 1.0, "16x16 scorch, area {area}");
        assert!(polys.iter().all(|p| p.verts.iter().all(|v| v.0.z.abs() < 1e-3)));
    }

    #[test]
    fn line_of_sight_hits_ground() {
        let mut w = World::default();
        w.add_building(Matrix::IDENTITY, ground());
        let hit = w.line_of_sight(Vec3::new(1.0, 1.0, 10.0), Vec3::new(1.0, 1.0, -10.0), true, None).unwrap();
        assert!((hit.1 - 0.5).abs() < 1e-4);
        assert!(hit.2.point.z.abs() < 1e-4);
    }
}
