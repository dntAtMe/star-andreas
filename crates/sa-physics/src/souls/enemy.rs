//! Souls mode enemies: peds that fight the player (attackers, cops in pursuit) get an
//! Elden Ring-style melee brain in place of GTA's fight task.
//!
//! They close in, circle at a distance while they wait their turn (at most a couple swing
//! at once), and attack with real ER combos from their weapon's one-handed moveset:
//! the swing tracks the player until shortly before it lands (so a late roll works), hits
//! during the animation's own hit windows with the blade sweep, and chains on with some
//! chance. Hurt reactions and poise (`Reaction`) interrupt them.

use std::sync::Arc;

use glam::{Vec2, Vec3};

use super::anim::{Pose, Retarget, sample};
use super::player::{ANIM_FPS, ActiveHit, SWEEP_STEPS, angle_diff, heading_of, turn_toward};
use super::{ActionDef, SoulsData};
use crate::anim::Clump;

/// ER attack ratings → GTA player health per hit (a club swing takes ~14 of 100).
const ENEMY_DAMAGE_SCALE: f32 = 0.12;
const TURN: f32 = 360.0;
/// Frames before a hit window when the swing stops tracking.
const TRACK_STOP: f32 = 6.0;
/// Distance kept while circling, and the run / walk switch.
const CIRCLE_RANGE: f32 = 3.6;
const RUN_FROM: f32 = 5.0;

#[derive(Clone, Debug)]
enum State {
    Approach,
    Circle,
    Attack { kind: String, f: f32, landed: u32 },
}

/// The animation of an enemy: the clip, its frame, and the pose faded out of.
#[derive(Clone, Debug, Default)]
struct Player {
    clip: String,
    from: Option<Pose>,
    shown: Option<Pose>,
    fade: f32,
    fade_len: f32,
    phase: f32,
    clock: f32,
}

pub struct Enemy {
    data: Arc<SoulsData>,
    /// ER weapon (one-handed moveset) and the GTA weapon type standing in for it.
    pub weapon: usize,
    pub gta_weapon: u32,
    heading: f32,
    state: State,
    /// Seconds before the next attack may start.
    cooldown: f32,
    combo: u8,
    strafe: f32,
    speed: f32,
    move_dir: Vec2,
    rng: u32,
    /// Set by the world each step: the player's position and whether this enemy may swing.
    pub target: Vec3,
    pub may_attack: bool,
    pub active_hit: Option<ActiveHit>,
    hit_stop: f32,
    anim: Player,
    retarget: Option<Retarget>,
    feet: f32,
}

impl Enemy {
    pub fn new(data: Arc<SoulsData>, gta_weapon: u32, heading: f32, seed: u32) -> Self {
        let weapon = data.weapon_index(super::er_weapon_for(gta_weapon)).or_else(|| data.weapon_index("Fist")).unwrap_or(0);
        Self {
            data,
            weapon,
            gta_weapon,
            heading,
            state: State::Approach,
            cooldown: 0.4,
            combo: 0,
            strafe: if seed & 1 == 0 { 1.0 } else { -1.0 },
            speed: 0.0,
            move_dir: Vec2::ZERO,
            rng: seed | 1,
            target: Vec3::ZERO,
            may_attack: false,
            active_hit: None,
            hit_stop: 0.0,
            anim: Player::default(),
            retarget: None,
            feet: 1.0,
        }
    }

    fn rand(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(214013).wrapping_add(2531011);
        ((self.rng >> 16) & 0x7FFF) as f32 / 32767.0
    }

    pub fn attacking(&self) -> bool {
        matches!(self.state, State::Attack { .. })
    }

    fn def(&self, kind: &str) -> Option<&ActionDef> {
        self.data.attacks.get(&(self.weapon, false, kind.to_string()))
    }

    /// How close it must be to swing.
    fn reach(&self) -> f32 {
        match self.data.weapons.get(self.weapon).map_or("", |w| w.name.as_str()) {
            "Fist" | "Claw" => 1.3,
            "Dagger" => 1.5,
            "Club" | "Flail" | "Whip" => 1.9,
            _ => 2.3,
        }
    }

    /// A hurt reaction cut in: drop the swing, and come back quickly.
    pub fn interrupt(&mut self) {
        if self.attacking() {
            self.state = State::Approach;
        }
        self.active_hit = None;
        self.cooldown = self.cooldown.max(0.6);
        self.speed = 0.0;
    }

    /// The blade connected (or was blocked): it can't hit again this window.
    pub fn mark_hit(&mut self, stop: f32) {
        if let State::Attack { kind, f, landed } = &mut self.state {
            let (k, at) = (kind.clone(), *f);
            if let Some(i) = self.data.attacks.get(&(self.weapon, false, k)).and_then(|d| d.hits.iter().position(|h| at >= h.from && at < h.to)) {
                *landed |= 1 << i;
            }
        }
        self.hit_stop = stop * 0.5;
    }

    fn start_attack(&mut self, kind: &str) -> bool {
        if self.def(kind).is_none() {
            return false;
        }
        self.state = State::Attack { kind: kind.into(), f: 0.0, landed: 0 };
        self.speed = 0.0;
        true
    }

    /// One physics step: returns (heading, local anim velocity in m per ts unit).
    pub fn step(&mut self, pos: Vec3, clump: &mut Clump, ts: f32) -> (f32, Vec2) {
        let dt = ts * 0.02;
        let df = dt * ANIM_FPS;
        self.active_hit = None;
        if self.retarget.is_none() {
            self.retarget = Retarget::new(&self.data, clump);
            if let Some(r) = &self.retarget {
                self.feet = r.feet;
            }
        }
        if self.hit_stop > 0.0 {
            self.hit_stop -= dt;
            self.animate(clump, 0.0);
            return (self.heading, Vec2::ZERO);
        }
        self.cooldown -= dt;
        let to = Vec2::new(self.target.x - pos.x, self.target.y - pos.y);
        let dist = to.length();
        let face = heading_of(to);
        let reach = self.reach();
        let mut vel: Vec2;
        match self.state.clone() {
            State::Approach | State::Circle => {
                let ready = self.may_attack && self.cooldown <= 0.0;
                if ready && dist <= reach + 0.4 {
                    self.combo = 0;
                    let r = self.rand();
                    let started = (r < 0.18 && self.start_attack("Heavy1")) || self.start_attack("Light1");
                    if started {
                        return self.finish(clump, dt, Vec2::ZERO);
                    }
                }
                if ready && dist > reach + 0.4 && dist < reach + 2.6 && self.speed > self.data.run_speed * 0.8 && self.rand() < 0.04 {
                    // A running lunge.
                    if self.start_attack("RunLight") {
                        return self.finish(clump, dt, Vec2::ZERO);
                    }
                }
                self.heading = turn_toward(self.heading, face, TURN.to_radians() * dt);
                let dir = to.normalize_or_zero();
                let (want, mdir) = if ready || dist > CIRCLE_RANGE + 1.5 {
                    self.state = State::Approach;
                    let run = dist > RUN_FROM;
                    (if run { self.data.run_speed } else { self.data.walk_speed }, dir)
                } else {
                    // Waiting their turn: circle, keeping the distance.
                    self.state = State::Circle;
                    if self.rand() < 0.005 {
                        self.strafe = -self.strafe;
                    }
                    let side = Vec2::new(-dir.y, dir.x) * self.strafe;
                    let radial = dir * ((dist - CIRCLE_RANGE) * 0.8).clamp(-1.0, 1.0);
                    (self.data.walk_speed, (side + radial).normalize_or_zero())
                };
                let rate = if want > self.speed { 8.0 } else { 12.0 };
                self.speed += (want - self.speed).clamp(-rate * dt, rate * dt);
                self.move_dir = mdir;
                vel = mdir * self.speed;
            }
            State::Attack { kind, mut f, landed } => {
                let Some(def) = self.def(&kind).cloned() else {
                    self.state = State::Approach;
                    return self.finish(clump, dt, Vec2::ZERO);
                };
                let prev = f;
                f += df;
                // Tracks until shortly before the first live hit.
                let next_hit = def.hits.iter().enumerate().find(|(i, h)| landed & (1 << i) == 0 && f < h.to).map(|(_, h)| h.from);
                if next_hit.is_some_and(|from| f < from - TRACK_STOP) && def.can_turn(f) {
                    self.heading = turn_toward(self.heading, face, def.turn_rate(f).to_radians() * dt);
                }
                let (m0, m1) = (def.motion_at(prev), def.motion_at(f));
                let fwd = Vec2::new(-self.heading.sin(), self.heading.cos());
                let left = Vec2::new(-fwd.y, fwd.x);
                vel = (fwd * (m1[2] - m0[2]) + left * (m1[0] - m0[0])) / dt.max(1e-4);
                // Don't walk through the player.
                if dist < 0.9 && vel.dot(to) > 0.0 {
                    vel -= to.normalize_or_zero() * vel.dot(to.normalize_or_zero());
                }
                if let Some((_, hit)) = def.hits.iter().enumerate().find(|(i, h)| f >= h.from && f < h.to && landed & (1 << i) == 0) {
                    let attack = self.data.weapons.get(self.weapon).map_or(100.0, |w| w.attack);
                    let sweep: Vec<(Vec3, Vec3)> = (0..SWEEP_STEPS)
                        .filter_map(|s| {
                            let start = (f - df).max(hit.from);
                            let at = start + (f - start) * s as f32 / (SWEEP_STEPS - 1) as f32;
                            super::player::blade_at(&hit.blade, hit.from, at).map(|(a, b)| (self.place(pos, a), self.place(pos, b)))
                        })
                        .collect();
                    if !sweep.is_empty() {
                        self.active_hit = Some(ActiveHit {
                            damage: attack * hit.mv * ENEMY_DAMAGE_SCALE,
                            poise: hit.guard_damage * 5.0,
                            stop: hit.stop,
                            radius: hit.radius,
                            heavy: kind.contains("Heavy"),
                            sweep,
                        });
                    }
                }
                // Chain on, or recover.
                let next = match kind.as_str() {
                    "Light1" | "RunLight" => Some("Light2"),
                    "Light2" => Some("Light3"),
                    "Heavy1" => Some("Light1"),
                    _ => None,
                };
                if f >= def.cancel_light && self.combo < 2 && dist <= reach + 1.2 {
                    if let Some(n) = next {
                        self.combo += 1;
                        let r = self.rand();
                        if r < 0.55 && self.start_attack(n) {
                            return self.finish(clump, dt, vel);
                        }
                        self.combo = 9;
                    }
                }
                if f >= def.total || (f >= def.cancel_move && self.combo >= 9) {
                    self.state = State::Approach;
                    self.cooldown = 1.0 + self.rand() * 1.4;
                    self.speed = 0.0;
                } else {
                    self.state = State::Attack { kind, f, landed };
                }
            }
        }
        self.finish(clump, dt, vel)
    }

    fn finish(&mut self, clump: &mut Clump, dt: f32, vel: Vec2) -> (f32, Vec2) {
        self.animate(clump, dt);
        let h = self.heading;
        let (f, r) = (Vec2::new(-h.sin(), h.cos()), Vec2::new(h.cos(), h.sin()));
        (h, Vec2::new(vel.dot(r), vel.dot(f)) * 0.02)
    }

    /// Character-space [left, up, forward] → GTA world.
    fn place(&self, pos: Vec3, v: Vec3) -> Vec3 {
        let f = Vec2::new(-self.heading.sin(), self.heading.cos());
        let l = Vec2::new(-f.y, f.x);
        Vec3::new(pos.x + l.x * v.x + f.x * v.z, pos.y + l.y * v.x + f.y * v.z, pos.z - self.feet + v.y)
    }

    fn animate(&mut self, clump: &mut Clump, dt: f32) {
        let d = self.data.clone();
        let stance = d.weapons.get(self.weapon).map_or(0, |w| w.stance[0]);
        self.anim.clock += dt * ANIM_FPS;
        let (name, frame, looped) = match &self.state {
            State::Attack { kind, f, .. } => (self.def(kind).map(|d| d.source.clone()).unwrap_or_default(), *f, false),
            _ if self.speed < 0.05 => (format!("a{stance:03}_000000"), self.anim.clock, true),
            _ => {
                let fwd = Vec2::new(-self.heading.sin(), self.heading.cos());
                let left = Vec2::new(-fwd.y, fwd.x);
                let (along, across) = (self.move_dir.dot(fwd), self.move_dir.dot(left));
                let dir = if along.abs() >= across.abs() { (along < 0.0) as u32 } else { 2 + (across < 0.0) as u32 };
                let running = self.speed > (d.walk_speed + d.run_speed) / 2.0;
                let (id, native) = if running {
                    (20100 + dir, [d.run_speed, d.run_back_speed, d.run_side_speed, d.run_side_speed][dir as usize])
                } else {
                    (20000 + dir, d.walk_speed)
                };
                let clip = format!("a000_{id:06}");
                let last = d.clips.get(&clip).map_or(1.0, |c| (c.frames - 1) as f32);
                self.anim.phase = (self.anim.phase + self.speed / native.max(0.1) * ANIM_FPS * dt / last).fract();
                (clip, self.anim.phase * last, true)
            }
        };
        let _ = angle_diff;
        let Some(target) = sample(&d, &name, frame, looped) else { return };
        let a = &mut self.anim;
        if name != a.clip {
            a.clip = name;
            a.from = a.shown.clone();
            a.fade = 0.0;
            a.fade_len = d.clips.get(&a.clip).map_or(4.0, |c| c.blend.max(1.0)) / ANIM_FPS;
        }
        a.fade += dt;
        let t = if a.from.is_some() && a.fade_len > 0.0 { (a.fade / a.fade_len).clamp(0.0, 1.0) } else { 1.0 };
        let pose: Pose = match &a.from {
            Some(fp) if t < 1.0 => (fp.0.lerp(target.0, t), fp.1.iter().zip(&target.1).map(|(x, y)| x.slerp(*y, t)).collect()),
            _ => target,
        };
        a.shown = Some(pose.clone());
        if let Some(rt) = &self.retarget {
            rt.apply(clump, &pose);
        }
    }
}
