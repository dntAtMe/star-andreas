//! `CFireManager` / `CFire` (60 slots at 0xB71F80) and `CCreepingFire` (0xB71B68).
//!
//! StartFire (0x53A050 / 0x539F00), CFire::ProcessFire (0x53A570), Extinguish (0x5393F0),
//! CreateFxSysForStrength (0x539360), TryToStartFireAtCoors (0x53A450),
//! CCreepingFire::Update (0x539CE0).
//!
//! Not ported: peds catching fire (the player near a fire, BMX riders), object fire
//! damage, riot fires, fire-cluster shadows and coronas, water checks.

use glam::Vec3;

use crate::{
    automobile::Automobile,
    effects::{Corona, FxHandle},
    shadows::ShadowTex,
    physical::{EntityType, normalise},
    world::{EntityId, World},
};

pub(crate) const MAX_FIRES: usize = 60;
/// `m_nMaxFireGenerationsAllowed` as set by the main script.
const MAX_GENERATIONS: i32 = 99_999;
/// Firetruck: never catches fire from nearby flames.
const MODEL_FIRETRUCK: u16 = 407;

#[derive(Debug, Clone)]
pub(crate) struct Fire {
    pub(crate) active: bool,
    script: bool,
    extinguishing: bool,
    pub(crate) first_generation: bool,
    pos: Vec3,
    target: Option<EntityId>,
    creator: Option<EntityId>,
    time_to_burn: u32,
    pub(crate) strength: f32,
    generations: i8,
    /// +0x21, always 60.
    removal_dist: u8,
    fx: Option<FxHandle>,
}

impl Default for Fire {
    /// The ctor (0x538B60).
    fn default() -> Self {
        Self {
            active: false,
            script: false,
            extinguishing: false,
            first_generation: true,
            pos: Vec3::ZERO,
            target: None,
            creator: None,
            time_to_burn: 0,
            strength: 1.0,
            generations: 100,
            removal_dist: 60,
            fx: None,
        }
    }
}

impl World {
    /// `GetRandomNumberInRange(int, int)` (0x407180): [a, b).
    fn rand_int(&mut self, a: i32, b: i32) -> i32 {
        a + (self.rng.unit() * (b - a) as f32) as i32
    }

    fn free_fire(&self) -> Option<usize> {
        self.fires.iter().position(|f| !f.active && !f.script)
    }

    /// Number of burning fires.
    pub fn active_fires(&self) -> usize {
        self.fires.iter().filter(|f| f.active).count()
    }

    /// The fire burning on `id` (the entity's +0x490 back-pointer).
    pub(crate) fn fire_on(&self, id: EntityId) -> Option<usize> {
        self.fires.iter().position(|f| f.active && f.target == Some(id))
    }

    /// 0x539360: strength tiers fire / fire_med / fire_large, always world-positioned.
    fn create_fire_fx(&mut self, i: usize) {
        if let Some(h) = self.fires[i].fx.take() {
            self.effects.kill(h);
        }
        let s = self.fires[i].strength;
        let name = if s <= 1.0 {
            "fire"
        } else if s <= 2.0 {
            "fire_med"
        } else {
            "fire_large"
        };
        let h = self.effects.create(name, self.fires[i].pos, None, true);
        self.effects.play(h);
        self.fires[i].fx = Some(h);
    }

    /// `CFireManager::StartFire(CEntity*, ...)` (0x53A050), for vehicles. `size` and the
    /// lifetime argument are ignored by the original for vehicles; every vehicle caller
    /// we have passes 0 generations.
    pub fn start_fire_on(&mut self, target: EntityId, creator: Option<EntityId>) -> Option<usize> {
        if self.fire_on(target).is_some() {
            return None;
        }
        let b = self.body(target)?;
        if b.phys.kind != EntityType::Vehicle {
            return None;
        }
        if let Some(car) = b.logic.as_any().downcast_ref::<Automobile>() {
            if car.damage.dm.engine >= 225 {
                return None;
            }
        }
        let pos = b.phys.matrix.pos;
        let i = self.free_fire()?;
        let burn = self.now_ms + 3000 + self.rand_int(0, 1000) as u32;
        self.fires[i] = Fire {
            active: true,
            pos,
            target: Some(target),
            creator,
            time_to_burn: burn,
            generations: 0,
            ..Default::default()
        };
        self.create_fire_fx(i);
        Some(i)
    }

    /// `CFireManager::StartFire(CVector, ...)` (0x539F00).
    pub fn start_fire_at(&mut self, pos: Vec3, creator: Option<EntityId>, lifetime_ms: u32, generations: i32) -> Option<usize> {
        let i = self.free_fire()?;
        let burn = (self.now_ms as f32 + lifetime_ms as f32 * (self.rng.rand01() * 0.3 + 1.0)) as u32;
        self.fires[i] = Fire {
            active: true,
            pos,
            creator,
            time_to_burn: burn,
            generations: generations.min(MAX_GENERATIONS) as i8,
            ..Default::default()
        };
        self.create_fire_fx(i);
        Some(i)
    }

    /// `CCreepingFire::TryToStartFireAtCoors` (0x53A450).
    pub(crate) fn try_start_fire_at_coors(&mut self, pos: Vec3, generations: i32, script: bool, z_search: f32) -> bool {
        let (cx, cy) = (((pos.x as i32) & 31) as usize, ((pos.y as i32) & 31) as usize);
        if self.creeping[cx][cy] != 0 {
            return false;
        }
        if self.fires.iter().filter(|f| !f.active && !f.script).take(6).count() < 6 {
            return false;
        }
        let Some((_, _, cp)) = self.line_of_sight(pos, pos - Vec3::new(0.0, 0.0, z_search), true, None) else {
            return false;
        };
        self.creeping[cx][cy] = 6;
        let Some(f) = self.start_fire_at(cp.point, None, 20_000, generations) else {
            return false;
        };
        self.fires[f].first_generation = false;
        self.fires[f].script = script;
        true
    }

    /// `CCreepingFire::Update`: one grid cell per frame.
    pub(crate) fn update_creeping(&mut self) {
        let (r, c) = ((self.frame & 31) as usize, ((self.frame >> 5) & 31) as usize);
        let v = &mut self.creeping[r][c];
        match *v {
            5 | 6 => *v -= 1,
            4 => *v = 0,
            _ => {}
        }
    }

    /// `CFire::Extinguish` (0x5393F0).
    pub(crate) fn extinguish(&mut self, i: usize) {
        let f = &mut self.fires[i];
        if !f.active {
            return;
        }
        f.active = false;
        f.extinguishing = false;
        f.first_generation = true;
        f.time_to_burn = 0;
        f.target = None;
        if let Some(h) = f.fx.take() {
            self.effects.kill(h);
        }
    }

    /// `CFireManager::Update` (0x53AF00): ProcessFire, then the fire-cluster glow and coronas.
    pub(crate) fn update_fires(&mut self, ts: f32) {
        for i in 0..MAX_FIRES {
            if self.fires[i].active {
                self.process_fire(i, ts);
            }
        }
        self.fire_clusters();
    }

    /// Fire clusters (visualfx.md B.9): additive shad_exp glow under strong clusters,
    /// plus four coronastar coronas above very strong ones.
    fn fire_clusters(&mut self) {
        let mut processed = [false; MAX_FIRES];
        let mut n = self.fires.iter().filter(|f| f.active).count();
        while n > 0 {
            let mut best = None;
            let mut m = -1.0;
            for (i, f) in self.fires.iter().enumerate() {
                if !processed[i] && f.active && f.strength > m {
                    m = f.strength;
                    best = Some(i);
                }
            }
            let Some(best) = best else { break };
            let bp = self.fires[best].pos;
            let (mut sum, mut wsum) = (0.0f32, 0i32);
            for (i, f) in self.fires.iter().enumerate() {
                if !processed[i] && f.active && (f.pos - bp).truncate().length() < 6.0 {
                    sum += f.strength;
                    wsum += f.strength.ceil() as i32;
                    n -= 1;
                    processed[i] = true;
                }
            }
            if sum > 4.0 && wsum != 0 {
                let size = (sum - 6.0 + 3.0).min(7.0);
                let k = self.rng.next() as f32 * (1.0 / 32767.0) * 0.4 + 0.6;
                let rgb = [(64.0 * k) as i32 as u8, (50.0 * k) as i32 as u8, (32.0 * k) as i32 as u8];
                let id = 0x2_0000_0000 | best as u64;
                self.store_static_shadow(
                    id,
                    2,
                    ShadowTex::Exp,
                    bp + Vec3::new(0.0, 0.0, 5.0),
                    glam::Vec2::new(size * 1.2, 0.0),
                    glam::Vec2::new(0.0, size * -1.2),
                    0,
                    rgb,
                    10.0,
                    1.0,
                    40.0,
                    false,
                    0.0,
                );
                if sum > 6.0 {
                    let kc = 0.8 * k;
                    let color = [(64.0 * kc) as i32 as u8, (50.0 * kc) as i32 as u8, (32.0 * kc) as i32 as u8];
                    let p0 = bp + Vec3::new(0.0, 0.0, 2.6);
                    let p1 = p0 + 3.5 * (self.camera_pos - p0).normalize_or_zero();
                    let r = Vec3::new(self.camera_right.x, self.camera_right.y, 0.0).normalize_or_zero();
                    for (j, (pos, flare)) in
                        [(p1, 2), (p1 + Vec3::Z * 2.0, 0), (p1 + r * 2.0, 0), (p1 - r * 2.0, 0)].into_iter().enumerate()
                    {
                        self.effects.coronas.push(Corona {
                            id: id * 4 + j as u64,
                            pos,
                            color,
                            radius: size * 0.5,
                            far_clip: 70.0,
                            near_clip: 1.5,
                            flare,
                        });
                    }
                }
            }
        }
    }

    /// `CFire::ProcessFire` (0x53A570).
    fn process_fire(&mut self, i: usize, ts: f32) {
        let now = self.now_ms;
        // Strength creeps up but never crosses a whole number.
        {
            let f = &mut self.fires[i];
            let s1 = (f.strength + ts * 0.002).min(3.0);
            if s1 as i32 == f.strength as i32 {
                f.strength = s1;
            }
        }

        // Follow the target.
        if let Some(t) = self.fires[i].target {
            let (script, creator) = (self.fires[i].script, self.fires[i].creator);
            let Some(b) = self.body_mut(t) else {
                self.extinguish(i);
                return;
            };
            let mut pos = b.phys.matrix.pos;
            if b.phys.kind == EntityType::Vehicle {
                if let Some(car) = b.logic.as_any_mut().downcast_mut::<Automobile>() {
                    if !script {
                        car.damage.inflict_damage(&mut b.phys, creator, false, ts * 1.2);
                    }
                    pos = b.phys.matrix.transform(car.headlights_pos + Vec3::new(0.0, 0.0, 0.15));
                }
            }
            let lead = b.phys.move_speed * (2.0 * ts);
            self.fires[i].pos = pos;
            if let Some(h) = self.fires[i].fx {
                self.effects.set_offset_pos(h, pos + lead);
            }
        }
        let pos = self.fires[i].pos;

        // Nearby vehicles catch fire: 1/32 per frame.
        if self.rng.next() & 0x1F == 0 {
            let creator = self.fires[i].creator;
            for id in self.body_ids().into_iter().rev() {
                let Some(b) = self.body(id) else { continue };
                if b.phys.kind != EntityType::Vehicle || self.fire_on(id).is_some() {
                    continue;
                }
                let model = b.logic.as_any().downcast_ref::<Automobile>().map_or(0, |a| a.model);
                if model != MODEL_FIRETRUCK && (b.phys.matrix.pos - pos).length() < 2.0 {
                    self.start_fire_on(id, creator);
                }
            }
        }

        // Spreading.
        if self.fires[i].generations > 0
            && self.rng.next() & 0x7F == 0
            && self.fires.iter().filter(|f| f.active).count() < 25
        {
            let (gens, script) = (self.fires[i].generations as i32 - 1, self.fires[i].script);
            let ry = self.rng.rand01();
            let rx = self.rng.rand01();
            let v = normalise(Vec3::new(2.0 * rx - 1.0, 2.0 * ry - 1.0, 0.0));
            let k = self.rng.rand01() + 2.0;
            self.try_start_fire_at_coors(Vec3::new(pos.x + v.x * k, pos.y + v.y * k, pos.z + 2.0), gens, script, 10.0);
        }

        // Merging with a weaker neighbour.
        if self.fires[i].strength <= 2.0 && self.fires[i].generations != 0 && self.rng.next() & 0xF == 0 {
            let j = (self.rng.unit() * 60.0) as usize;
            let o = &self.fires[j];
            if j != i && o.active && !o.script && o.strength <= 1.0 && (o.pos - pos).length() < 3.5 {
                let (opos, ogens) = (o.pos, o.generations);
                let f = &mut self.fires[i];
                f.pos = pos * 0.7 + opos * 0.3;
                f.strength += 1.0;
                f.time_to_burn = f.time_to_burn.max(now + 7000);
                self.create_fire_fx(i);
                let f = &mut self.fires[i];
                f.generations = f.generations.min(ogens);
                self.extinguish(j);
            }
        }

        let f = &self.fires[i];
        if let Some(h) = f.fx {
            let frac = f.strength - (f.strength as i32) as f32;
            let left = (f.time_to_burn as i64 - now as i64) as f32 * (1.0 / 3500.0);
            self.effects.set_const_time(h, true, frac.min(left));
        }

        // Lifetime.
        let f = &self.fires[i];
        let d = (self.camera_pos - f.pos).truncate().length();
        let alive = (now < f.time_to_burn && d < f.removal_dist as f32) || f.script;
        let (p, strength) = (f.pos, f.strength);
        if alive {
            let c = (self.rng.next() & 0x7F) as f32 / 512.0;
            self.effects.add_light(p, 8.0, Vec3::new(c, c, 0.0), false);
        } else if strength <= 1.0 {
            self.extinguish(i);
        } else {
            let f = &mut self.fires[i];
            f.strength -= 1.0;
            f.time_to_burn = now + 7000;
            self.create_fire_fx(i);
        }
    }
}
