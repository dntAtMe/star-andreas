//! Souls in the world: lock-on targeting, the player's blade against the peds, and the
//! peds' poise and Elden Ring hit reactions (retargeted onto their own skeletons).

use std::sync::Arc;

use glam::{Quat, Vec2, Vec3};

use super::anim::{Pose, Retarget, sample};
use super::player::{ANIM_FPS, Dir, LOCK_BREAK_RANGE, LOCK_ON_RANGE, angle_diff, heading_of, segment_distance};
use super::{ActionDef, SoulsData, Souls};
use crate::anim::Clump;
use crate::ped::PedLogic;
use crate::world::{EntityId, World};

const PED_MAX_POISE: f32 = 40.0;
/// Seconds without being hit before poise is whole again.
const POISE_RESET: f32 = 5.0;
const HIT_STOP_SCALE: f32 = 0.5;
/// Frames to blend into and back out of a reaction.
const BLEND_OUT: f32 = 4.0;

/// A ped's poise and the ER hurt animation it is playing.
pub struct Reaction {
    data: Arc<SoulsData>,
    pub poise: f32,
    since_hit: f32,
    hit_stop: f32,
    /// The hurt action, its frame, and the heading it plays at.
    act: Option<(ActionDef, f32)>,
    heading: f32,
    retarget: Option<Retarget>,
    /// The GTA pose the reaction blends from and back to.
    gta_pose: Vec<(Quat, Vec3)>,
}

impl Reaction {
    pub fn new(data: Arc<SoulsData>) -> Self {
        Self { data, poise: PED_MAX_POISE, since_hit: 0.0, hit_stop: 0.0, act: None, heading: 0.0, retarget: None, gta_pose: Vec::new() }
    }

    /// A hit for `poise` damage from `from`; `heading` is the ped's.
    fn hit(&mut self, poise: f32, damage: f32, heavy: bool, stop: f32, pos: Vec3, from: Vec3, heading: f32) {
        self.since_hit = 0.0;
        self.poise -= poise;
        let level = if self.poise <= 0.0 {
            self.poise = PED_MAX_POISE;
            if heavy { "Knockdown" } else { "Large" }
        } else if damage < 15.0 {
            "Small"
        } else {
            "Middle"
        };
        let to = Vec2::new(from.x - pos.x, from.y - pos.y);
        let side = if to.length_squared() < 1e-6 { Dir::Front } else { Dir::of(heading, heading_of(to)) };
        if let Some(def) = self.data.base.get(&format!("Hurt(HurtLevel::{level}, Dir::{})", side.name())) {
            self.act = Some((def.clone(), 0.0));
            self.heading = heading;
        }
        self.hit_stop = stop * HIT_STOP_SCALE;
    }

    pub fn active(&self) -> bool {
        self.act.is_some()
    }

    /// One physics step: poses the clump and returns (heading, local anim velocity in m
    /// per ts) while a reaction plays.
    pub fn step(&mut self, clump: &mut Clump, ts: f32) -> Option<(f32, Vec2)> {
        let dt = ts * 0.02;
        self.since_hit += dt;
        if self.since_hit > POISE_RESET {
            self.poise = PED_MAX_POISE;
        }
        let (def, f) = self.act.as_mut()?;
        if self.retarget.is_none() {
            self.retarget = Retarget::new(&self.data, clump);
        }
        let df = if self.hit_stop > 0.0 {
            self.hit_stop -= dt;
            0.0
        } else {
            dt * ANIM_FPS
        };
        let prev = *f;
        *f += df;
        let (f, total) = (*f, def.total);
        let (m0, m1) = (def.motion_at(prev), def.motion_at(f));
        let source = def.source.clone();
        if f >= total {
            self.act = None;
            return None;
        }
        // Blend in from the GTA pose and back out at the end.
        if prev == 0.0 || self.gta_pose.len() != clump.pose.len() {
            self.gta_pose = clump.pose.clone();
        }
        let blend = self.data.clips.get(&source).map_or(4.0, |c| c.blend.max(1.0));
        let w = (f / blend).min(1.0).min((total - f) / BLEND_OUT).clamp(0.0, 1.0);
        let pose: Option<Pose> = sample(&self.data, &source, f, false);
        if let (Some(pose), Some(rt)) = (pose, &self.retarget) {
            rt.apply(clump, &pose);
            for (k, g) in self.gta_pose.iter().enumerate() {
                let (q, t) = clump.pose[k];
                clump.pose[k] = (g.0.slerp(q, w), g.1.lerp(t, w));
            }
        }
        // Root motion: [left, up, forward] → local (x right, y forward), per ts.
        let k = if dt > 0.0 { 0.02 / dt } else { 0.0 };
        let v = Vec2::new(-(m1[0] - m0[0]), m1[2] - m0[2]) * k;
        Some((self.heading, v))
    }
}

impl World {
    fn souls_of(&mut self, id: EntityId) -> Option<&mut Souls> {
        self.body_mut(id)?.logic.as_any_mut().downcast_mut::<PedLogic>()?.souls.as_deref_mut()
    }

    fn alive_ped(&self, id: EntityId) -> Option<Vec3> {
        let b = self.body(id)?;
        let l = b.logic.as_any().downcast_ref::<PedLogic>()?;
        (!l.is_player && l.tasks.health.alive() && l.vehicle.is_none()).then_some(b.phys.matrix.pos)
    }

    /// Before ProcessControl: the lock-on target, and switching it.
    pub(crate) fn souls_pre(&mut self) {
        let Some(pid) = self.player_id() else { return };
        let Some(ppos) = self.body(pid).map(|b| b.phys.matrix.pos) else { return };
        let Some(s) = self.souls_of(pid) else { return };
        let (want_new, switch, cur, heading) = (s.input.lock && !s.locked, s.input.switch_target, s.target_id, s.heading);
        s.input.switch_target = 0;
        let mut target = cur.and_then(|id| self.alive_ped(id).map(|p| (id, p))).filter(|(_, p)| p.distance(ppos) <= LOCK_BREAK_RANGE);
        let candidates: Vec<(EntityId, Vec3)> = self
            .body_ids()
            .into_iter()
            .filter_map(|id| self.alive_ped(id).map(|p| (id, p)))
            .filter(|(_, p)| p.distance(ppos) <= LOCK_ON_RANGE)
            .collect();
        let bearing = |p: Vec3| heading_of(Vec2::new(p.x - ppos.x, p.y - ppos.y));
        if want_new {
            let fwd = Vec2::new(-heading.sin(), heading.cos());
            target = candidates
                .iter()
                .copied()
                .min_by(|a, b| {
                    let score = |p: Vec3| {
                        let d = Vec2::new(p.x - ppos.x, p.y - ppos.y);
                        d.length() * (2.0 - d.normalize_or_zero().dot(fwd))
                    };
                    score(a.1).total_cmp(&score(b.1))
                });
        } else if switch != 0 {
            if let Some((cid, cp)) = target {
                // The nearest bearing on that side (right = clockwise = negative).
                let base = bearing(cp);
                let next = candidates
                    .iter()
                    .copied()
                    .filter(|&(id, _)| id != cid)
                    .map(|(id, p)| (id, p, angle_diff(base, bearing(p)) * -(switch as f32)))
                    .filter(|&(_, _, d)| d > 0.0)
                    .min_by(|a, b| a.2.total_cmp(&b.2));
                if let Some((id, p, _)) = next {
                    target = Some((id, p));
                }
            }
        }
        let Some(s) = self.souls_of(pid) else { return };
        s.target_id = target.map(|t| t.0);
        // Aim at the chest.
        s.target = target.map(|t| t.1 + Vec3::Z * 0.3);
    }

    /// After ProcessControl: the player's blade against the other peds.
    pub(crate) fn souls_post(&mut self) {
        let Some(pid) = self.player_id() else { return };
        let Some(ppos) = self.body(pid).map(|b| b.phys.matrix.pos) else { return };
        let Some(s) = self.souls_of(pid) else { return };
        if !s.locked {
            s.target_id = None;
        }
        let Some(hit) = s.active_hit.take() else { return };
        let (weapon_ty, data) = (s.gta_weapon, s.data.clone());
        let mut landed = false;
        for id in self.body_ids() {
            if id == pid {
                continue;
            }
            let Some(p) = self.alive_ped(id) else { continue };
            if p.distance(ppos) > 6.0 {
                continue;
            }
            // The ped as a capsule from shin to shoulders, plus the head.
            let (lo, hi, head) = (p - Vec3::Z * 0.6, p + Vec3::Z * 0.4, p + Vec3::Z * 0.65);
            let touches = hit.sweep.iter().any(|&(a, c)| {
                segment_distance(a, c, lo, hi) <= hit.radius + 0.3 || segment_distance(a, c, head, head) <= hit.radius + 0.15
            });
            if !touches {
                continue;
            }
            let Some(b) = self.body_mut(id) else { continue };
            let m = b.phys.matrix;
            let heading = heading_of(Vec2::new(m.fwd.x, m.fwd.y));
            // Which side the attacker is on: 0 front, 1 left, 2 back, 3 right.
            let to = (ppos - m.pos).truncate();
            let (fwd, right) = (m.fwd.truncate(), m.right.truncate());
            let dir = if to.dot(fwd).abs() >= to.dot(right).abs() {
                if to.dot(fwd) >= 0.0 { 0 } else { 2 }
            } else if to.dot(right) >= 0.0 {
                3
            } else {
                1
            };
            let Some(l) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { continue };
            l.pending_damage.push(crate::peddamage::DamageIn {
                src: Some(pid),
                src_pos: Some(ppos),
                ty: weapon_ty,
                damage: hit.damage,
                piece: 3,
                dir,
                fight: None,
                force_death: false,
            });
            let r = l.souls_react.get_or_insert_with(|| Box::new(Reaction::new(data.clone())));
            r.hit(hit.poise, hit.damage, hit.heavy, hit.stop, m.pos, ppos, heading);
            landed = true;
        }
        if landed {
            if let Some(s) = self.souls_of(pid) {
                s.mark_hit(hit.stop);
            }
        }
    }
}
