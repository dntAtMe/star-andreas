//! `CTaskSimpleDuck` (the player's crouch, TASK_CONTROLLED, secondary slot 1),
//! `PlayerControlDucked` (0x687F30), `CanPedDuck` (0x692610) and the crouch rolls.

use glam::Vec2;

use crate::{
    Ctx,
    anim::{AnimManager, Clump, af, anim_id, group},
    pedtask::{PedCore, PedTasks},
    weapon::{fire, wf, wt},
};

/// Default-group anims of the crouch.
pub mod duck_anim {
    pub const WEAPON_CROUCH: i16 = 55;
    pub const CROUCH_FWD: i16 = 56;
    pub const ROLL_L: i16 = 57;
    pub const CROUCH_BWD: i16 = 58;
    pub const ROLL_R: i16 = 59;
}

#[derive(Debug, Clone)]
pub struct DuckTask {
    pub finished: bool,
    pub aborting: bool,
    need_set_flag: bool,
    in_control: bool,
    /// x = roll ±1 / 0, y = forward (-) / back (+).
    pub move_cmd: Vec2,
    duck_anim: Option<u32>,
    /// (uid, anim id) of the crouch walk or roll.
    move_anim: Option<(u32, i16)>,
    count_down: u8,
}

impl DuckTask {
    pub fn new() -> Self {
        Self {
            finished: false,
            aborting: false,
            need_set_flag: true,
            in_control: true,
            move_cmd: Vec2::ZERO,
            duck_anim: None,
            move_anim: None,
            count_down: 0xFF,
        }
    }

    fn rolling(&self) -> bool {
        self.move_anim.is_some_and(|(_, id)| id == duck_anim::ROLL_L || id == duck_anim::ROLL_R)
    }

    fn duck_down(&self, clump: &Clump) -> bool {
        self.duck_anim.and_then(|u| clump.by_uid(u)).is_some_and(|a| a.blend >= 1.0)
    }

    /// 0x61C3D0: busy for AIM / pistol-whip / the reload without a gun task.
    pub fn busy_aim(&self, clump: &Clump) -> bool {
        !(self.move_cmd.x == 0.0 && self.move_cmd.y == 0.0 && self.duck_down(clump) && !self.aborting)
    }

    /// 0x61C420: busy for FIRE / BURST / the gun-task RELOAD.
    pub fn busy_fire(&self, clump: &Clump) -> bool {
        !(self.move_cmd.x == 0.0 && self.duck_down(clump) && !self.aborting)
    }

    /// `ForceStopMove` (0x6924B0).
    pub fn force_stop_move(&mut self) {
        self.in_control = true;
        self.move_cmd.y = 0.0;
    }

    /// `ControlDuckMove(x, y)` (0x6923F0).
    pub fn control_duck_move(&mut self, x: f32, y: f32, ts: f32) {
        self.in_control = true;
        if self.move_cmd.x == 1.0 || self.move_cmd.x == -1.0 {
            return;
        }
        let step = ts * 0.07;
        let d = y - self.move_cmd.y;
        self.move_cmd.y = if d > step {
            self.move_cmd.y + step
        } else if d < -step {
            self.move_cmd.y - step
        } else {
            y
        };
        if y.abs() < 0.1 && x.abs() > 0.9 {
            self.move_cmd.y = 0.0;
            self.move_cmd.x = if x > 0.0 { 1.0 } else { -1.0 };
        }
    }

    /// `DeleteAnimCB` (0x692550) for the deletions / finish callbacks of the last anim update.
    fn anim_callbacks(&mut self, clump: &Clump) {
        if let Some(u) = self.duck_anim {
            if clump.deleted.contains(&u) || clump.by_uid(u).is_none() {
                self.duck_anim = None;
                if self.move_anim.is_none() || !self.aborting {
                    self.finished = true;
                }
            }
        }
        if let Some((u, id)) = self.move_anim {
            let roll = id == duck_anim::ROLL_L || id == duck_anim::ROLL_R;
            let fired = if roll { clump.finished.contains(&u) || clump.by_uid(u).is_none() } else { clump.deleted.contains(&u) || clump.by_uid(u).is_none() };
            if fired {
                if roll {
                    self.move_cmd.x = 0.0;
                }
                self.move_anim = None;
                if self.aborting {
                    self.finished = true;
                }
            }
        }
    }

    /// `MakeAbortable(ped, prio)` (0x692100): LEISURE (urgent = false) or URGENT.
    pub fn make_abortable(&mut self, t: &mut PedTasks, clump: &mut Clump, m: &AnimManager, urgent: bool) -> bool {
        if self.rolling() {
            return false;
        }
        let d = if urgent { -8.0 } else { -4.0 };
        if let Some(u) = self.duck_anim {
            if let Some(a) = clump.by_uid_mut(u) {
                if a.blend > 0.0 && a.blend_delta >= 0.0 {
                    if a.has(af::PARTIAL) {
                        a.blend_delta = d;
                    }
                    clump.blend_animation(m, t.anim_group, anim_id::IDLE, -d);
                }
            }
            if urgent {
                self.duck_anim = None;
            }
        }
        if let Some((u, id)) = self.move_anim {
            if let Some(a) = clump.by_uid_mut(u) {
                if a.blend > 0.0 && a.blend_delta >= 0.0 && (urgent || id == duck_anim::CROUCH_FWD || id == duck_anim::CROUCH_BWD) {
                    a.blend_delta = d;
                    a.flags &= !af::PLAYING;
                }
            }
            if urgent {
                self.move_anim = None;
            }
        }
        if let Some(mut g) = t.gun.take() {
            g.clear_anim_for_duck(t, clump);
            t.gun = Some(g);
        }
        if urgent {
            self.finished = true;
            t.ducking = false;
            self.need_set_flag = true;
            return true;
        }
        self.aborting = true;
        false
    }

    /// `CTaskSimpleDuck::ProcessPed` (0x694390). Returns true when done.
    pub fn process_ped(&mut self, t: &mut PedTasks, clump: &mut Clump, m: &AnimManager) -> bool {
        self.anim_callbacks(clump);
        if self.finished {
            if !self.aborting {
                self.make_abortable(t, clump, m, true);
            }
            t.ducking = false;
            return true;
        }
        if self.need_set_flag {
            if let Some(mut g) = t.gun.take() {
                g.clear_anim_for_duck(t, clump);
                t.gun = Some(g);
            }
            t.ducking = true;
            self.need_set_flag = false;
        } else if !t.ducking {
            self.finished = true;
        }
        if self.aborting {
            return false;
        }
        if !self.in_control {
            let c = self.count_down;
            self.count_down = c.wrapping_sub(1);
            if c != 0 {
                return false;
            }
            if self.make_abortable(t, clump, m, true) {
                t.ducking = false;
                return true;
            }
            return false;
        }
        self.count_down = 4;
        match self.duck_anim {
            None => {
                if let Some(a) = clump.get_mut(10) {
                    a.blend_delta = -2.0;
                }
                if let Some(i) = clump.blend_animation(m, group::DEFAULT, duck_anim::WEAPON_CROUCH, 4.0) {
                    self.duck_anim = Some(clump.assocs[i].uid);
                }
            }
            Some(u) => {
                if clump.by_uid(u).is_some_and(|a| a.blend > 0.99) {
                    self.set_move_anim(clump, m);
                }
            }
        }
        self.in_control = false;
        false
    }

    /// `CTaskSimpleDuck::SetMoveAnim` (0x6939F0).
    fn set_move_anim(&mut self, clump: &mut Clump, m: &AnimManager) {
        let blend = |clump: &mut Clump, id: i16, finish: bool| {
            clump.blend_animation(m, group::DEFAULT, id, 8.0).map(|i| {
                clump.assocs[i].finish_cb = finish;
                (clump.assocs[i].uid, id)
            })
        };
        if self.move_cmd.x != 0.0 {
            if self.move_anim.is_some_and(|(_, id)| id != duck_anim::CROUCH_FWD && id != duck_anim::CROUCH_BWD) {
                return;
            }
            let id = if self.move_cmd.x > 0.0 { duck_anim::ROLL_R } else { duck_anim::ROLL_L };
            self.move_anim = blend(clump, id, true);
            return;
        }
        let y = self.move_cmd.y;
        if y != 0.0 {
            let (want, other) = if y > 0.0 {
                (duck_anim::CROUCH_BWD, duck_anim::CROUCH_FWD)
            } else {
                (duck_anim::CROUCH_FWD, duck_anim::CROUCH_BWD)
            };
            if self.move_anim.is_some_and(|(_, id)| id == other) {
                self.move_anim = None;
            }
            if self.move_anim.is_none() {
                self.move_anim = blend(clump, want, false);
            }
            if let Some(a) = self.move_anim.and_then(|(u, _)| clump.by_uid_mut(u)) {
                a.speed = y.abs().max(0.6);
            }
        } else if let Some((u, id)) = self.move_anim {
            if id == duck_anim::CROUCH_FWD || id == duck_anim::CROUCH_BWD {
                if let Some(a) = clump.by_uid_mut(u) {
                    a.flags &= !af::PLAYING;
                    a.blend_delta = -4.0;
                }
            }
        }
    }
}

impl Default for DuckTask {
    fn default() -> Self {
        Self::new()
    }
}

impl PedTasks {
    /// `CanPedDuck` (0x692610).
    pub fn can_ped_duck(&self) -> bool {
        if self.move_state >= 6 {
            return false;
        }
        let ty = self.active_weapon().ty;
        self.info_of(ty).is_none_or(|i| {
            i.fire_type == fire::MELEE || i.fire_type == fire::USE || ty == wt::SPRAYCAN || i.has(wf::CROUCHFIRE)
        })
    }

    /// The camera's `IsPedDucking` (0x50CEB0): ducking and not standing up.
    pub fn ducking_for_camera(&self) -> bool {
        self.ducking && self.duck.as_ref().is_some_and(|d| !d.aborting)
    }

    /// `SetTaskDuckSecondary(0)` (0x601230).
    pub fn set_task_duck(&mut self, clump: &mut Clump, m: &AnimManager) {
        let mut d = DuckTask::new();
        if let Some(mut g) = self.gun.take() {
            g.clear_anim_for_duck(self, clump);
            self.gun = Some(g);
        }
        let done = d.process_ped(self, clump, m);
        if !done {
            self.duck = Some(d);
        }
    }

    /// `PlayerControlDucked` (0x687F30), single-player branches.
    pub fn player_control_ducked(&mut self, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) {
        let Some(mut duck) = self.duck.take() else { return };
        let x = self.pad.walk_lr / 128.0;
        let y = self.pad.walk_ud / 128.0;
        let mag = (x * x + y * y).sqrt().min(1.0);
        if duck.finished || duck.aborting {
            self.duck = Some(duck);
            return;
        }
        if self.pad.duck_just_down
            || self.pad.sprint
            || self.pad.jump_just_down
            || self.pad.enter_exit_just_down
            || !self.can_ped_duck()
        {
            // ClearTaskDuckSecondary (0x601390) and the walk-off blend.
            duck.make_abortable(self, c.clump, m, false);
            self.pd.mbr = 0.0;
            if let Some(mut g) = self.gun.take() {
                g.clear_anim_for_duck(self, c.clump);
                self.gun = Some(g);
            }
            let two_handed = self.gun.as_ref().and_then(|g| g.info.as_ref()).is_some_and(|i| !i.has(wf::AIMWITHARM));
            if two_handed {
                if mag > 0.5 {
                    if let Some(i) = c.clump.blend_animation(m, group::DEFAULT, anim_id::GUNMOVE_FWD, 4.0) {
                        c.clump.assocs[i].flags |= af::PLAYING;
                    }
                    self.pd.mbr = 1.0;
                    if let Some(g) = &mut self.gun {
                        g.control_gun_move(Vec2::new(1.0, 0.0), ctx.ts);
                    }
                }
            } else if mag > 0.5 {
                let (id, state) = if self.pad.sprint { (anim_id::RUN, 6) } else { (anim_id::WALK, 4) };
                if let Some(i) = c.clump.blend_animation(m, self.anim_group, id, 4.0) {
                    c.clump.assocs[i].flags |= af::PLAYING;
                }
                self.pd.mbr = 1.5;
                self.move_state = state;
            }
            self.duck = Some(duck);
            return;
        }
        let melee = self.info_of(self.active_weapon().ty).is_none_or(|i| i.fire_type == fire::MELEE);
        if self.pad.aim && !melee {
            duck.control_duck_move(x, y, ctx.ts);
            self.pd.mbr = 0.0;
            self.duck = Some(duck);
            return;
        }
        if mag > 0.0 {
            let h = crate::pedtask::radian_angle_between_points(0.0, 0.0, -x, y) - ctx.cam.orientation;
            *c.aim_rot = crate::ped::limit_radian_angle(h);
        }
        self.pd.mbr = mag;
        duck.control_duck_move(0.0, -mag, ctx.ts);
        self.duck = Some(duck);
    }
}
