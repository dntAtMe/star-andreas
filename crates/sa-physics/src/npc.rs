//! NPC pedestrian intelligence for random walkers (population.md §3–§4): the
//! `CTaskComplexWanderStandard` → `CTaskSimpleGoToPoint` loop over the ped path nodes, junction
//! turns (UpdateDir), dead-end u-turns, road crossings with the ped lights, scratch-head when
//! stuck, and the NPC `CPed::SetMoveAnim` (move_anim.md §6).
//!
//! Not ported: ped-ped avoidance (917), ScanForStuff (attractors, chats), the cop / medic /
//! criminal / prostitute wander variants, event responses (flee, fight), ambient speech.

use std::sync::Arc;

use glam::{Vec2, Vec3};

use crate::{
    anim::{AnimManager, Clump, af, anim_id},
    paths::{NodeAddr, PathFind},
    pedtask::radian_angle_between_points,
};

/// `CTrafficLights::LightForPeds` (0x49D400): 0 walk, 1 flashing, 2 don't walk.
pub fn light_for_peds(now_ms: u32) -> u8 {
    let t = (now_ms >> 1) & 0x3FFF;
    if t < 12000 {
        2
    } else if t < 15384 {
        0
    } else {
        1
    }
}

#[derive(Debug, Clone)]
enum Sub {
    /// `CTaskSimpleGoToPoint` (900).
    GoTo { target: Vec3, prev: Vec2 },
    /// `CTaskSimpleScratchHead` (421) [I: ~2 s standing].
    ScratchHead { until: u32 },
    /// The u-turn sequence: stand still 500 ms, then go back.
    UTurn { until: u32 },
    /// ObserveTrafficLights / CrossRoadLook + AchieveHeading (225 / 227).
    Cross { heading: f32, lights: bool, wait_until: u32 },
}

/// `CTaskComplexWanderStandard` (912).
#[derive(Debug, Clone)]
pub struct Wander {
    move_state: u8,
    dir: u8,
    radius: f32,
    last: Option<NodeAddr>,
    next: Option<NodeAddr>,
    last_dir_frame: u32,
    sub: Option<Sub>,
}

impl Wander {
    pub fn new(dir: u8) -> Self {
        Self { move_state: 4, dir, radius: 0.5, last: None, next: None, last_dir_frame: u32::MAX, sub: None }
    }
}

/// What the NPC's tasks need besides the ped.
pub struct NpcIn<'a> {
    pub paths: &'a PathFind,
    pub anims: &'a AnimManager,
    pub now_ms: u32,
    pub frame: u32,
    pub ts: f32,
}

/// Per-NPC state (`CCivilianPed` additions the port needs).
#[derive(Debug, Clone)]
pub struct NpcState {
    pub model: u32,
    /// eCopType / ped type (4 CIVMALE, 5 CIVFEMALE …).
    pub ped_type: u8,
    /// entity+0x20 random seed.
    pub seed: u16,
    /// ped+0x4D4 move anim group.
    pub anim_group: usize,
    /// ped+0x534 / +0x538.
    pub move_state: u8,
    pub last_move_state: u8,
    /// ped+0x54C off-screen removal timer.
    pub remove_at_ms: u32,
    /// Clump alpha and the fade-out flag (ped+0x470 & 8).
    pub alpha: u8,
    pub fading_out: bool,
    pub wander: Option<Wander>,
    pub paths: Option<Arc<PathFind>>,
    /// Stuck counter (intelligence+0x274) [I: frames without progress].
    stuck: u32,
}

impl NpcState {
    pub fn new(model: u32, ped_type: u8, seed: u16, anim_group: usize, dir: u8, paths: Arc<PathFind>, now_ms: u32) -> Self {
        Self {
            model,
            ped_type,
            seed,
            anim_group,
            move_state: 1,
            last_move_state: 0,
            remove_at_ms: now_ms + 4000,
            alpha: 0,
            fading_out: false,
            wander: Some(Wander::new(dir)),
            paths: Some(paths),
            stuck: 0,
        }
    }

    /// `UpdatePathNodes` + `FindNextNodeWandering`.
    fn update_path_nodes(w: &mut Wander, paths: &PathFind, pos: Vec3, dir: u8) -> u8 {
        w.last = w.next;
        w.next = None;
        paths.find_next_node_wandering(pos, &mut w.last, &mut w.next, dir)
    }

    /// `UpdateDir` (0x669DA0): at junctions turn ±90° (10 % each), if the new way differs
    /// by at most 3 octants from the current direction.
    fn update_dir(w: &mut Wander, paths: &PathFind, pos: Vec3, seed: u16, frame: u32) {
        let mut nd = w.dir;
        if let Some(n) = w.next.and_then(|n| paths.node(n)) {
            if n.num_links() >= 3 && frame != w.last_dir_frame {
                w.last_dir_frame = frame;
                let r = (seed as u32 + 3 * frame) % 100;
                if r > 90 {
                    nd = (w.dir + 6) % 8;
                } else if r > 80 {
                    nd = (w.dir + 2) % 8;
                }
            }
        }
        if nd != w.dir {
            let (mut l, mut n) = (w.next, None);
            let o = paths.find_next_node_wandering(pos, &mut l, &mut n, nd);
            if (o.max(w.dir) - o.min(w.dir) + 8) % 8 <= 3 {
                w.dir = nd;
            }
        }
    }

    fn goto(&self, w: &Wander, paths: &PathFind, pos: Vec3) -> Sub {
        let t = w.next.map_or(pos, |n| paths.wander_target(n, self.seed) + Vec3::Z);
        Sub::GoTo { target: t, prev: pos.truncate() }
    }

    /// `HeadingToNextNode` (0x66F530).
    fn heading_to_next(&self, w: &Wander, paths: &PathFind, pos: Vec3) -> f32 {
        let t = w.next.map_or(pos, |n| paths.wander_target(n, self.seed));
        crate::ped::limit_radian_angle(radian_angle_between_points(t.x - pos.x, t.y - pos.y, 0.0, 0.0))
    }

    /// One step of the wander task tree; sets the move state and the aimed heading.
    pub fn process(&mut self, pos: Vec3, move_speed: Vec3, aim_rot: &mut f32, cur_rot: f32, i: &NpcIn) {
        let Some(mut w) = self.wander.take() else { return };
        let paths = i.paths;
        if w.sub.is_none() {
            // CreateFirstSubTask.
            Self::update_dir(&mut w, paths, pos, self.seed, i.frame);
            let d = w.dir;
            w.dir = Self::update_path_nodes(&mut w, paths, pos, d);
            w.sub = Some(self.goto(&w, paths, pos));
        }
        let mut done = false;
        match w.sub.as_mut().unwrap() {
            Sub::GoTo { target, prev } => {
                self.move_state = w.move_state;
                let d_raw = target.truncate() - pos.truncate();
                let dz = (pos.z - target.z).abs();
                let reached = d_raw.length_squared() < w.radius * w.radius;
                *prev = pos.truncate();
                let nxt = pos.truncate() + move_speed.truncate() * i.ts;
                if (reached && dz < 2.0) || (target.truncate() - nxt).dot(d_raw) <= 0.0 {
                    done = true;
                } else {
                    let dn = d_raw.normalize_or_zero();
                    *aim_rot = crate::ped::limit_radian_angle(radian_angle_between_points(dn.x, dn.y, 0.0, 0.0));
                }
                // Stuck: no progress while trying to walk.
                if move_speed.truncate().length() < 0.002 {
                    self.stuck += 1;
                } else {
                    self.stuck = 0;
                }
                if self.stuck > 30 {
                    self.stuck = 0;
                    w.sub = Some(Sub::ScratchHead { until: i.now_ms + 2000 });
                }
            }
            Sub::ScratchHead { until } | Sub::UTurn { until } => {
                self.move_state = 1;
                if i.now_ms >= *until {
                    done = true;
                }
            }
            Sub::Cross { heading, lights, wait_until } => {
                // AchieveHeading (rate ×0.5), then wait for the walk light / look.
                self.move_state = 1;
                *aim_rot = *heading;
                let d = crate::ped::limit_radian_angle(*heading - cur_rot).abs();
                if d < 0.2 {
                    if *lights {
                        if light_for_peds(i.now_ms) == 0 {
                            done = true;
                        }
                    } else if *wait_until == 0 {
                        *wait_until = i.now_ms + 1000;
                    } else if i.now_ms >= *wait_until {
                        done = true;
                    }
                }
            }
        }
        if done {
            // CreateNextSubTask (0x674140).
            let sub = w.sub.take().unwrap();
            w.sub = Some(match sub {
                Sub::ScratchHead { .. } => {
                    let d = w.dir.wrapping_add(1) % 8;
                    w.dir = Self::update_path_nodes(&mut w, paths, pos, d);
                    if w.last.is_some() && w.next.is_some() && w.next != w.last {
                        self.goto(&w, paths, pos)
                    } else {
                        Sub::ScratchHead { until: i.now_ms + 2000 }
                    }
                }
                Sub::Cross { .. } | Sub::UTurn { .. } => self.goto(&w, paths, pos),
                Sub::GoTo { .. } => {
                    Self::update_dir(&mut w, paths, pos, self.seed, i.frame);
                    let prev = w.last;
                    let d = w.dir;
                    w.dir = Self::update_path_nodes(&mut w, paths, pos, d);
                    if w.next == prev && prev.is_some() {
                        Sub::UTurn { until: i.now_ms + 500 }
                    } else if !(w.last.is_some() && w.next.is_some() && w.last != w.next) {
                        Sub::ScratchHead { until: i.now_ms + 2000 }
                    } else {
                        // Crossings on the link last → next (sensible wanderers).
                        let inter = w
                            .last
                            .map(|l| paths.links(l).into_iter().find(|(nb, _)| Some(*nb) == w.next).map_or(0, |x| x.1))
                            .unwrap_or(0);
                        if inter & 2 != 0 {
                            Sub::Cross { heading: self.heading_to_next(&w, paths, pos), lights: true, wait_until: 0 }
                        } else if inter & 1 != 0 {
                            Sub::Cross { heading: self.heading_to_next(&w, paths, pos), lights: false, wait_until: 0 }
                        } else {
                            self.goto(&w, paths, pos)
                        }
                    }
                }
            });
        }
        self.wander = Some(w);
    }

    /// NPC `CPed::SetMoveAnim` (0x5E4A00).
    pub fn set_move_anim(&mut self, clump: &mut Clump, m: &AnimManager) {
        if self.move_state == self.last_move_state {
            return;
        }
        self.last_move_state = self.move_state;
        if matches!(self.move_state, 4 | 6 | 7) {
            for a in &mut clump.assocs {
                if a.has(af::PARTIAL) && a.flags & (af::FADE_OUT_FINISHED | af::NO_ROOT_PARTIAL_SUM) == 0 {
                    a.blend_delta = -2.0;
                    a.flags |= af::DELETE_BLENDED_OUT;
                }
            }
        }
        let g = self.anim_group;
        match self.move_state {
            1 => _ = clump.blend_animation(m, g, anim_id::IDLE, 4.0),
            4 => _ = clump.blend_animation(m, g, anim_id::WALK, 1.0),
            6 => _ = clump.blend_animation(m, g, anim_id::RUN, 1.0),
            7 => _ = clump.blend_animation(m, g, 2, 1.0),
            _ => {}
        }
    }
}
