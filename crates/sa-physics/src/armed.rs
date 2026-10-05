//! Armed NPC combat: `CTaskComplexKillPedOnFootArmed` (1002) with its `CTaskSimpleGunControl`
//! (1020) driving the ped's `CTaskSimpleUseGun`, and `IsTargetVisible`'s cached line of sight
//! (ped_combat.md §3).
//!
//! Not ported: cover points, ducking (pedFlags 1/4, the 50 % crouch while shooting), the
//! 8 s flank (it seeks to 1 m instead), the strafe-side line tests, the gun-into-a-wall
//! re-seek, pistol whipping, `IsInPlayersLineOfFire`, speech.

use glam::{Vec2, Vec3};

use crate::{
    anim::{AnimManager, Clump},
    gun::{GunCmd, UseGun},
    npc::{NpcIn, NpcState},
    ped::limit_radian_angle,
    pedevents::{PedNow, RespIn, rand_range},
    pedtask::{PedTasks, radian_angle_between_points},
    weapon::{fire, wf, ws},
    world::EntityId,
};

/// `IsTargetVisible`'s cache (1002 +0x38..+0x57), kept by the world.
#[derive(Debug, Clone, Copy, Default)]
pub struct LosCache {
    pub last_clear: u32,
    pub last_blocked: u32,
    pub ped_pos: Vec3,
    pub target_pos: Vec3,
}

impl LosCache {
    /// `IsTargetVisible` (0x621500) before the line test: Some(answer) from the cache.
    pub fn cached(&self, now: u32, ped: Vec3, target: Vec3) -> Option<bool> {
        let (period, thr2) = (10000, 9.0);
        if now.wrapping_sub(self.last_clear) < period && self.last_clear != 0 {
            return Some(true);
        }
        if now.wrapping_sub(self.last_blocked) <= period
            && self.last_blocked != 0
            && (target - self.target_pos).length_squared() <= thr2
            && (ped - self.ped_pos).length_squared() <= thr2
        {
            return Some(false);
        }
        None
    }

    pub fn store(&mut self, now: u32, clear: bool, ped: Vec3, target: Vec3) {
        if clear {
            self.last_clear = now;
            self.last_blocked = 0;
        } else {
            self.last_clear = 0;
            self.last_blocked = now;
            self.target_pos = target;
            self.ped_pos = ped;
        }
    }
}

/// `CTaskSimpleGunControl` (1020), firing types 0 (aim), 3 (timed bursts) and 6 (end).
#[derive(Debug, Clone)]
pub struct GunControl {
    finished: bool,
    firing_type: u8,
    /// +0x28 (−1 = none).
    time_left: i32,
    burst_length: i16,
    /// +0x34: 0 = schedule, u32::MAX = burst in progress.
    next_burst: u32,
}

impl GunControl {
    fn new(firing_type: u8) -> Self {
        Self { finished: false, firing_type, time_left: -1, burst_length: 5, next_burst: 0 }
    }

    /// `MakeAbortable` (0x61F530) for a non-event abort.
    fn make_abortable(&mut self, tasks: &mut PedTasks, urgent: bool) {
        if let Some(g) = tasks.gun.as_mut() {
            g.count_down = if urgent { 2 } else { 10 };
            g.has_fired = false;
        }
    }

    /// `ProcessPed` (0x625270). Returns true when finished.
    #[allow(clippy::too_many_arguments)]
    fn process(&mut self, target: EntityId, tp: Vec3, aim: Vec3, me: &mut PedNow, tasks: &mut PedTasks, rng: &mut crate::damage::Rand, rate: u16, now: u32, ms_step: u32) -> bool {
        if self.finished {
            if let Some(g) = tasks.gun.as_mut() {
                g.count_down = g.count_down.min(4);
                g.has_fired = false;
            }
            return true;
        }
        if self.firing_type < 6 && tasks.gun.is_none() {
            tasks.gun = Some(UseGun::new(GunCmd::Aim));
            tasks.pd.attack_counter = 0.0;
            self.next_burst = 0;
        }
        let Some(info) = tasks.gun.as_ref().and_then(|g| g.info.clone()).or_else(|| tasks.active_info()) else {
            return true;
        };
        let mut end_now = false;
        let cmd = if tasks.active_weapon().state == ws::RELOADING && info.has(wf::RELOAD) {
            GunCmd::Reload
        } else {
            match self.firing_type {
                0 => GunCmd::Aim,
                3 => {
                    if now >= self.next_burst {
                        self.next_burst = u32::MAX;
                        let mut len = info.ammo_clip;
                        if len > 1 && rate < 100 {
                            len = len.min((rate as i32 * 4 / 30) as i16);
                            len = if len - rand_range(rng, 0, 2) as i16 >= 1 { len - rand_range(rng, 0, 2) as i16 } else { 1 };
                        }
                        self.burst_length = len;
                        if let Some(g) = tasks.gun.as_mut() {
                            g.burst_length = len;
                        }
                        GunCmd::FireBurst
                    } else {
                        if self.next_burst == u32::MAX && tasks.gun.as_ref().is_some_and(|g| g.last_cmd != GunCmd::FireBurst) {
                            self.next_burst = 0;
                        }
                        GunCmd::Aim
                    }
                }
                _ => {
                    end_now = true;
                    GunCmd::EndLeisure
                }
            }
        };
        if self.time_left > -1 {
            self.time_left = (self.time_left - ms_step as i32).max(0);
        }
        if end_now || self.time_left == 0 {
            match tasks.gun.as_mut() {
                None => self.finished = true,
                Some(g) if self.firing_type == 6 && g.finished => self.finished = true,
                Some(g) if g.has_fired && !matches!(g.last_cmd, GunCmd::Fire | GunCmd::FireBurst) => {
                    g.count_down = 2;
                    g.has_fired = false;
                    self.finished = true;
                }
                _ => {}
            }
        }
        if self.finished {
            return true;
        }
        let mut cmd = cmd;
        if self.time_left == 0 && tasks.gun.is_some() {
            self.firing_type = 6;
            cmd = GunCmd::EndLeisure;
        }
        if self.next_burst == 0 {
            let base = if info.fire_type == fire::PROJECTILE { 4000.0 } else { 2000.0 };
            let u = rng.rand01();
            self.next_burst = now + ((u * 0.5 + 0.75) * base / (rate as f32 * 1.0 * 0.04)) as i32 as u32;
        }
        if let Some(g) = tasks.gun.as_mut() {
            g.control_gun(cmd);
            g.target = Some((target, aim));
        }
        let d = tp - me.pos;
        *me.aim_rot = limit_radian_angle((-d.x).atan2(d.y));
        let d2 = d.length_squared();
        if let Some(g) = tasks.gun.as_mut() {
            if d2 < 6.0 && (g.last_cmd != GunCmd::FireBurst || info.has(wf::MOVEFIRE)) {
                g.control_gun_move(Vec2::new(0.0, 1.0), ms_step as f32 / 20.0);
                return false;
            }
        }
        if d2 > info.weapon_range * info.weapon_range {
            self.time_left = 0;
        }
        false
    }
}

#[derive(Debug, Clone)]
enum Sub {
    /// 907 CTaskComplexSeekEntity (run at the target until within `radius`).
    Seek { radius: f32 },
    /// 1020.
    Gun(GunControl),
    /// 202 CTaskSimplePause(100).
    Pause { until: u32 },
}

/// `CTaskComplexKillPedOnFootArmed` (1002), the parts without cover.
#[derive(Debug, Clone)]
pub struct Armed {
    sub: Option<Sub>,
    shoot_until: u32,
    last_shoot_start: u32,
    strafe_until: u32,
    /// 0 left, 1 right, 2 forward, 3 back.
    strafe_dir: u8,
    flip_strafe: bool,
    seek_start: u32,
}

impl Armed {
    pub fn new() -> Self {
        Self { sub: None, shoot_until: 0, last_shoot_start: 0, strafe_until: 0, strafe_dir: 0, flip_strafe: false, seek_start: 0 }
    }

    /// `MakeAbortable`: stop the gun control (the use-gun task winds down on its own).
    pub fn abort(&mut self, tasks: &mut PedTasks) {
        if let Some(Sub::Gun(g)) = self.sub.as_mut() {
            g.make_abortable(tasks, true);
        }
        self.sub = None;
    }

    pub fn seeking(&self) -> bool {
        matches!(self.sub, Some(Sub::Seek { .. }))
    }

    /// The running sub-task, for debug logs.
    pub fn describe(&self) -> String {
        match &self.sub {
            None => "-".into(),
            Some(Sub::Seek { radius }) => format!("seek {radius:.1}"),
            Some(Sub::Pause { .. }) => "pause".into(),
            Some(Sub::Gun(g)) => format!("gun t{} left {} next {}", g.firing_type, g.time_left, g.next_burst),
        }
    }

    /// `CreateSubTask(1020)`.
    fn gun_control(&mut self, ri: &RespIn, rng: &mut crate::damage::Rand, now: u32) -> Sub {
        let mode = if ri.threat_visible { 3 } else { 0 };
        self.shoot_until = now + rand_range(rng, 4000, 8000) as u32;
        self.last_shoot_start = now;
        Sub::Gun(GunControl::new(mode))
    }

    /// `CreateSubTask(907)`: the seek radius shrinks from 6 m to 1 m after 3..8 s.
    fn seek(&self, now: u32) -> Sub {
        let e = if self.seek_start != 0 { now.wrapping_sub(self.seek_start) as f32 } else { 0.0 };
        let radius = if self.seek_start == 0 || e < 3000.0 {
            6.0
        } else if e <= 8000.0 {
            6.0 - (e - 3000.0) * 0.001
        } else {
            1.0
        };
        Sub::Seek { radius }
    }

    /// `CreateFirstSubTask` (0x62BF00) without cover.
    fn create_first(&mut self, dist: f32, range: f32, ri: &RespIn, rng: &mut crate::damage::Rand, now: u32) -> Sub {
        if dist * dist <= range * range * 0.25 && ri.threat_visible {
            return self.gun_control(ri, rng, now);
        }
        self.seek(now)
    }

    /// `CreateNextSubTask` (0x62C190): DECIDE after a gun control, the seek / pause rules.
    fn create_next(&mut self, finished: &Sub, dist: f32, range: f32, me: &PedNow, ri: &RespIn, rng: &mut crate::damage::Rand, now: u32) -> Sub {
        match finished {
            Sub::Pause { .. } => {
                let s = self.seek(now);
                if self.seek_start == 0 {
                    self.seek_start = now;
                }
                return s;
            }
            Sub::Seek { .. } => {
                if ri.threat_visible {
                    self.seek_start = 0;
                    return self.gun_control(ri, rng, now);
                }
                if self.seek_start == 0 {
                    self.seek_start = now;
                }
                return Sub::Pause { until: now + 100 };
            }
            Sub::Gun(_) => {}
        }
        // DECIDE.
        let _ = me;
        let res = 'decide: {
            if dist < 3.0 {
                break 'decide self.gun_control(ri, rng, now);
            }
            let x = (range * 0.8).min(23.0);
            if dist > x {
                break 'decide Sub::Seek { radius: (range * 0.6).min(20.0) };
            }
            let mut res = None;
            if dist > 10.0 {
                match rng.next() & 3 {
                    0 => res = Some(Sub::Seek { radius: dist - 4.0 }),
                    1 => {
                        self.strafe_dir = 2;
                        self.strafe_until = now + 2000;
                        self.flip_strafe = false;
                    }
                    _ => {}
                }
            }
            if dist > 5.0 && rng.next() & 3 == 0 {
                // The side line tests (2.5 m) are not ported: the side is kept.
                self.strafe_dir = (rng.next() & 1) as u8;
                self.strafe_until = now + 2000;
                self.flip_strafe = false;
            }
            if let Some(r) = res {
                break 'decide r;
            }
            if ri.threat_visible {
                break 'decide self.gun_control(ri, rng, now);
            }
            if rng.next() & 3 == 0 {
                self.strafe_dir = (rng.next() < 0x3FFF) as u8;
                self.strafe_until = now + 2000;
                self.flip_strafe = false;
            }
            self.last_shoot_start = now;
            Sub::Pause { until: now + 100 }
        };
        self.seek_start = 0;
        res
    }
}

impl Default for Armed {
    fn default() -> Self {
        Self::new()
    }
}

impl NpcState {
    /// 1002 Control + the running sub-task. Returns true when the kill task ends.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kill_ped_on_foot_armed(
        &mut self,
        a: &mut Armed,
        target: EntityId,
        me: &mut PedNow,
        _clump: &mut Clump,
        _m: &AnimManager,
        tasks: &mut PedTasks,
        ri: &RespIn,
        i: &NpcIn,
    ) -> bool {
        let now = i.now_ms;
        let Some(tp) = ri.threat_pos.filter(|_| ri.threat_alive) else {
            a.abort(tasks);
            return true;
        };
        let Some(info) = tasks.active_info() else { return true };
        let range = info.target_range;
        let d = tp - me.pos;
        let dist = d.length();
        let ms_step = (i.ts * 0.02 * 1000.0) as i32 as u32;
        let mut rng = std::mem::replace(&mut self.rng, crate::damage::Rand::new(1));
        if a.sub.is_none() {
            a.sub = Some(a.create_first(dist, range, ri, &mut rng, now));
        }
        // ControlSubTask (0x62CCE0).
        match a.sub.as_mut().unwrap() {
            Sub::Gun(g) => {
                let stop = dist > range || (dist > range * 0.5 && now > a.last_shoot_start + 2500) || (dist > 4.0 && now > a.shoot_until) || !ri.threat_visible;
                if stop && g.firing_type != 6 {
                    g.firing_type = 6;
                    g.next_burst = 0;
                }
                if let Some(gun) = tasks.gun.as_mut() {
                    let mv = if now < a.strafe_until {
                        [Vec2::new(-1.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, -1.0), Vec2::new(0.0, 1.0)][a.strafe_dir as usize]
                    } else if dist < 4.0 {
                        Vec2::new(0.0, 1.0)
                    } else {
                        if a.flip_strafe {
                            a.strafe_dir ^= 1;
                            a.flip_strafe = false;
                            a.strafe_until = now + 2500;
                        }
                        Vec2::ZERO
                    };
                    gun.control_gun_move(mv, i.ts);
                }
            }
            Sub::Seek { .. } => {
                let fwd = Vec3::new(-me.cur_rot.sin(), me.cur_rot.cos(), 0.0);
                let ok = dist < range * 0.5 && (ri.threat_move_speed.dot(fwd) < 0.0 || now.wrapping_sub(a.last_shoot_start) > 2000);
                if ok && ri.threat_visible {
                    a.sub = Some(a.gun_control(ri, &mut rng, now));
                }
            }
            Sub::Pause { .. } => {}
        }
        // The sub-task's ProcessPed; a finished one makes the next.
        let done = match a.sub.as_mut().unwrap() {
            Sub::Seek { radius } => {
                self.move_state = 6;
                *me.aim_rot = limit_radian_angle(radian_angle_between_points(tp.x, tp.y, me.pos.x, me.pos.y));
                dist < *radius
            }
            Sub::Pause { until } => {
                self.move_state = 1;
                now >= *until
            }
            Sub::Gun(g) => {
                self.move_state = 1;
                {
                    let rate = tasks.shooting_rate;
                    g.process(target, tp, ri.threat_aim.unwrap_or(tp), me, tasks, &mut rng, rate, now, ms_step)
                }
            }
        };
        if done {
            let fin = a.sub.take().unwrap();
            a.sub = Some(a.create_next(&fin, dist, range, me, ri, &mut rng, now));
        }
        self.rng = rng;
        false
    }
}

impl crate::world::World {
    /// `CPed::GetTransformedBonePosition(offset, tag)`: a ped bone's point in world space.
    pub fn ped_bone_world(&self, id: EntityId, tag: i32, offset: Vec3) -> Option<Vec3> {
        let b = self.body(id)?;
        let clump = b.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?.clump.as_deref()?;
        let f = clump.frame_of_tag(tag)?;
        Some(b.phys.matrix.transform(clump.ltm(f).transform_point3(offset)))
    }
}
