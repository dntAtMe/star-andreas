//! `CTaskSimpleUseGun` (aim / fire / reload loop driven by the fire anim's time),
//! `CTaskSimplePlayerOnFoot::ProcessPlayerWeapon` (the PC mouse path: RMB free aim, LMB
//! fire), the aiming IK (`CPedIK::PointGunInDirection`, `RotateTorsoForArm`, an analytic
//! stand-in for the arm IK chain) and `FireGun` (hand bone → `CWeapon::Fire`).
//!
//! NPCs (CTaskSimpleGunControl, see `armed.rs`) aim at a target entity: the arm IK points at
//! its spine, the shot goes to it with the AI spread.
//!
//! Not ported: pistol whipping, crouching, blocked-arm sensors (Ped2 col model),
//! the player's lock-on targets, first-person / projectile / area-effect weapons.

use glam::{Quat, Vec2, Vec3};

use crate::{
    Ctx,
    anim::{AnimManager, Clump, af, anim_id, group},
    bullet::InstantHit,
    effects::WorldRequest,
    pedtask::{PedCore, PedTasks},
    weapon::{WeaponInfo, fire, wf, ws, wt},
    world::EntityId,
};

/// `eGunCommand`; ControlGun keeps the highest command of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum GunCmd {
    #[default]
    Null = 0,
    Aim = 1,
    Fire = 2,
    FireBurst = 3,
    Reload = 4,
    PistolWhip = 5,
    EndLeisure = 6,
    EndNow = 7,
}

/// Bone tags used here.
pub mod bone {
    pub const SPINE: i32 = 2;
    pub const SPINE1: i32 = 3;
    pub const NECK: i32 = 4;
    pub const R_UPPERARM: i32 = 22;
    pub const R_FOREARM: i32 = 23;
    pub const R_HAND: i32 = 24;
    pub const L_HAND: i32 = 34;
}

/// `CTaskSimpleUseGun` (0x3C bytes).
#[derive(Debug, Clone, Default)]
pub struct UseGun {
    pub finished: bool,
    in_control: bool,
    move_control: bool,
    pub(crate) has_fired: bool,
    /// Fire-this-frame bits: 1 right gun, 2 left gun.
    pub fire_bits: u8,
    pub next_cmd: Option<GunCmd>,
    pub last_cmd: GunCmd,
    move_cmd: Vec2,
    /// m_pAnim: uid of the fire / reload assoc.
    pub anim: Option<u32>,
    /// m_pWeaponInfo (type, skill) and a copy of the row.
    pub info: Option<WeaponInfo>,
    info_key: Option<(u32, u8)>,
    /// +0x34 burst length / +0x36 shots left in the burst (also the reload counter).
    pub(crate) burst_length: i16,
    pub(crate) burst_shots: i16,
    pub(crate) count_down: u8,
    /// m_pTarget and its aim point (the spine, from the world) for NPCs.
    pub target: Option<(EntityId, Vec3)>,
    arm_ik: bool,
    look_ik: bool,
}

impl UseGun {
    /// `new CTaskSimpleUseGun(target, pos, cmd, burst, aimImmediate)`.
    pub fn new(cmd: GunCmd) -> Self {
        Self { next_cmd: Some(cmd), last_cmd: GunCmd::Null, count_down: 0xFF, ..Default::default() }
    }

    /// `ControlGun` (0x61E040).
    pub fn control_gun(&mut self, cmd: GunCmd) {
        self.in_control = true;
        if self.next_cmd.is_none_or(|n| n < cmd) {
            self.next_cmd = Some(cmd);
        }
    }

    /// `StopAim` (0x61E0A0).
    pub fn stop_aim(&mut self) {
        self.in_control = true;
        let next = self.next_cmd.unwrap_or(GunCmd::Null);
        if self.last_cmd < GunCmd::Fire && next < GunCmd::Fire {
            self.next_cmd = Some(GunCmd::EndLeisure);
        }
    }

    /// `ControlGunMove` (0x61E0C0).
    pub fn control_gun_move(&mut self, v: Vec2, ts: f32) {
        let step = ts * 0.07;
        let approach = |c: f32, t: f32| if t - c > step { c + step } else if t - c < -step { c - step } else { t };
        self.move_cmd = Vec2::new(approach(self.move_cmd.x, v.x), approach(self.move_cmd.y, v.y));
        self.move_control = true;
    }

    /// `ClearAnimForDuck` (0x61E190): drop the fire / reload anim on a duck / stand change.
    pub fn clear_anim_for_duck(&mut self, t: &mut PedTasks, clump: &mut Clump) {
        if let Some(u) = self.anim.take() {
            if let Some(a) = clump.by_uid_mut(u) {
                if a.blend > 0.0 && a.blend_delta >= 0.0 {
                    a.blend_delta = -8.0;
                }
                a.finish_cb = false;
            }
        }
        if self.last_cmd < GunCmd::EndLeisure {
            self.last_cmd = GunCmd::Null;
        }
        self.abort_ik(t, t.now_ms);
    }

    /// `AbortIK` (0x61DFA0).
    fn abort_ik(&mut self, t: &mut PedTasks, now: u32) {
        let now = now as i64;
        if self.arm_ik {
            t.ikm.abort_point_arm(0, 250, now);
            t.ikm.abort_point_arm(1, 250, now);
        }
        if self.look_ik {
            t.ikm.abort_look_at(250, now);
        }
        self.arm_ik = false;
        self.look_ik = false;
    }

    fn next(&self) -> GunCmd {
        self.next_cmd.unwrap_or(GunCmd::Null)
    }

    /// `ClearAnim` (0x61E8E0).
    fn clear_anim(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager) {
        if let (Some(u), Some(info)) = (self.anim, &self.info) {
            if let Some(a) = c.clump.by_uid_mut(u) {
                if !a.has(af::PLAYING) && a.time < a.hier.total_length {
                    if (info.anim_loop_start - a.time).abs() < 1e-6 {
                        a.set_current_time(info.anim_loop_end);
                    }
                    a.flags |= af::PLAYING;
                }
            }
        }
        let mut any = false;
        for id in anim_id::GUN_STAND..=anim_id::GUNMOVE_R {
            if let Some(a) = c.clump.get_mut(id) {
                a.flags |= af::DELETE_BLENDED_OUT;
                any = true;
            }
        }
        t.move_state = 1;
        if any {
            if t.pad.walk_ud.abs() <= 50.0 && t.pad.walk_lr.abs() <= 50.0 {
                if let Some(i) = c.clump.blend_animation(m, t.anim_group, anim_id::IDLE, 8.0) {
                    c.clump.assocs[i].flags |= af::PLAYING;
                }
                if self.next() == GunCmd::EndLeisure && !(57..=59).contains(&t.anim_group) {
                    c.clump.blend_animation(m, group::DEFAULT, anim_id::GUN_2_IDLE, 8.0);
                }
            } else {
                if let Some(i) = c.clump.blend_animation(m, t.anim_group, anim_id::WALK, 8.0) {
                    c.clump.assocs[i].flags |= af::PLAYING;
                }
                t.move_state = 4;
                t.pd.mbr = 1.0;
            }
        }
    }

    /// `StartAnim` (0x624F30).
    fn start_anim(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager) {
        let info = self.info.clone().unwrap();
        if let Some(u) = self.anim.take() {
            if let Some(a) = c.clump.by_uid_mut(u) {
                if self.next() == GunCmd::EndNow && a.blend_delta > -8.0 && a.blend > 0.0 && self.last_cmd < GunCmd::Reload {
                    a.blend_delta = -8.0;
                }
                a.finish_cb = false;
            }
        }
        match self.next() {
            GunCmd::Null | GunCmd::EndLeisure | GunCmd::EndNow => {
                self.clear_anim(t, c, m);
                self.finished = true;
            }
            GunCmd::Aim | GunCmd::Fire | GunCmd::FireBurst => {
                let crouch = t.duck.as_ref().filter(|_| t.ducking);
                if self.next() == GunCmd::Aim {
                    let ok = (self.move_cmd == Vec2::ZERO || info.has(wf::MOVEFIRE)) && crouch.is_none_or(|d| !d.busy_aim(c.clump));
                    if !ok {
                        if matches!(self.last_cmd, GunCmd::Fire | GunCmd::FireBurst) {
                            self.last_cmd = GunCmd::Aim;
                        }
                        return;
                    }
                } else if crouch.is_some_and(|d| d.busy_fire(c.clump)) {
                    return;
                }
                if self.next() == GunCmd::FireBurst {
                    self.burst_shots = self.burst_length;
                }
                let id = if t.ducking && info.has(wf::CROUCHFIRE) { anim_id::WEAPON_CROUCHFIRE } else { anim_id::WEAPON_FIRE };
                let i = c.clump.blend_animation(m, info.anim_group, id, 8.0);
                if let Some(i) = i {
                    if self.last_cmd == GunCmd::Reload && info.has(wf::RELOAD2START) {
                        let a = &mut c.clump.assocs[i];
                        a.set_current_time(info.anim_loop_start);
                        a.flags &= !af::PLAYING;
                    }
                    c.clump.assocs[i].finish_cb = true;
                    self.anim = Some(c.clump.assocs[i].uid);
                }
            }
            GunCmd::Reload => {
                if t.ducking && t.duck.as_ref().is_some_and(|d| d.busy_fire(c.clump)) {
                    return;
                }
                if self.last_cmd != GunCmd::Reload {
                    self.burst_shots = if info.has(wf::TWIN_PISTOL) { 2 } else { 1 };
                }
                if self.burst_shots < 1 {
                    self.last_cmd = GunCmd::Null;
                    self.next_cmd = Some(GunCmd::Null);
                    return;
                }
                let id = if t.ducking && info.has(wf::CROUCHFIRE) { anim_id::WEAPON_CROUCHRELOAD } else { anim_id::WEAPON_RELOAD };
                if let Some(i) = c.clump.blend_animation(m, info.anim_group, id, 8.0) {
                    let a = &mut c.clump.assocs[i];
                    a.start(0.0);
                    a.finish_cb = true;
                    self.anim = Some(a.uid);
                }
                self.burst_shots -= 1;
            }
            GunCmd::PistolWhip => {}
        }
        self.last_cmd = self.next();
        self.next_cmd = Some(GunCmd::Null);
    }

    /// `FinishGunAnimCB` (0x61F3A0) for the anims whose finish callback fired.
    fn finish_callbacks(&mut self, clump: &Clump) {
        let Some(u) = self.anim else { return };
        if clump.finished.contains(&u) || clump.deleted.contains(&u) || clump.by_uid(u).is_none() {
            let id = clump.by_uid(u).map(|a| a.id);
            if self.burst_shots > 0
                && matches!(id, Some(anim_id::WEAPON_RELOAD | anim_id::WEAPON_CROUCHRELOAD))
                && self.last_cmd == GunCmd::Reload
                && self.next() < GunCmd::EndLeisure
            {
                self.next_cmd = Some(GunCmd::Reload);
            }
            self.anim = None;
        }
    }

    /// `CTaskSimpleUseGun::ProcessPed` (0x62A380). Returns true when finished.
    pub fn process_ped(&mut self, t: &mut PedTasks, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) -> bool {
        if self.look_ik && !t.ikm.is_looking() {
            self.look_ik = false;
        }
        if self.arm_ik && !t.ikm.is_arm_pointing(0) {
            self.arm_ik = false;
        }
        self.fire_bits = 0;
        self.finish_callbacks(c.clump);
        let w = *t.active_weapon();
        let skill = t.weapon_skill(w.ty);
        if self.info.is_none() {
            if t.pd.chosen_slot != t.active_slot {
                return false;
            }
            let Some(info) = t.info_of(w.ty).cloned() else { return true };
            if info.fire_type == fire::MELEE || info.has(wf::THROW) {
                self.finished = true;
            } else {
                self.info = Some(info);
                self.info_key = Some((w.ty, skill));
                self.move_cmd = Vec2::ZERO;
            }
        } else if self.info_key != Some((w.ty, skill)) {
            // MakeAbortable(URGENT): the weapon or its skill changed.
            self.clear_anim(t, c, m);
            self.anim = None;
            self.finished = true;
        }
        if self.finished {
            return self.finish(t, c, m);
        }
        let info = self.info.clone().unwrap();
        if !self.in_control {
            self.has_fired = false;
            self.move_cmd = Vec2::ZERO;
            if self.count_down != 0 {
                self.count_down = self.count_down.wrapping_sub(1);
                return false;
            }
            self.finished = true;
            return self.finish(t, c, m);
        }
        self.count_down = 0xFF;

        if self.anim.is_none() {
            self.start_anim(t, c, m);
        } else {
            match self.last_cmd {
                GunCmd::Reload => {
                    let a = c.clump.by_uid_mut(self.anim.unwrap()).unwrap();
                    if !matches!(a.id, anim_id::WEAPON_RELOAD | anim_id::WEAPON_CROUCHRELOAD) && a.blend_delta >= 0.0 {
                        a.blend_delta = -4.0;
                    }
                }
                GunCmd::Aim | GunCmd::Fire | GunCmd::FireBurst => {
                    let fading = c.clump.by_uid(self.anim.unwrap()).is_none_or(|a| a.blend_delta < 0.0);
                    if !fading {
                        self.firing_loop(t, c, &info, w.state);
                    }
                }
                GunCmd::Null => {
                    if let Some(a) = c.clump.by_uid_mut(self.anim.unwrap()) {
                        if a.blend > 0.0 && a.blend_delta >= 0.0 {
                            a.blend_delta = -4.0;
                        }
                    }
                }
                _ => {}
            }
        }
        if self.finished {
            return self.finish(t, c, m);
        }
        // Aim / IK.
        let mut skip_aim = false;
        if info.has(wf::AIMWITHARM) && !t.ducking {
            let tgt = match self.target.filter(|_| !t.is_player) {
                Some((_, p)) => Some(p),
                None => arm_target(t, c, ctx),
            };
            if let Some(tgt) = tgt {
                let d = tgt - c.p.matrix.pos;
                let rel = crate::ped::limit_radian_angle((-d.x).atan2(d.y) - *c.cur_rot);
                if !(-2.268_928..=2.007_128_7).contains(&rel) {
                    skip_aim = true;
                }
            }
        }
        if matches!(self.last_cmd, GunCmd::Aim | GunCmd::Fire | GunCmd::FireBurst) && !skip_aim {
            self.aim_gun(t, c, ctx, &info);
        } else {
            self.abort_ik(t, ctx.now_ms);
        }
        if !info.has(wf::AIMWITHARM) || t.ducking || self.move_control {
            self.set_move_anim(t, c, m, &info, ctx.ts);
        }
        let next = self.next();
        if next < GunCmd::EndLeisure && (next != GunCmd::Reload || !matches!(self.last_cmd, GunCmd::Fire | GunCmd::FireBurst)) {
            self.next_cmd = Some(GunCmd::Null);
        }
        self.in_control = false;
        false
    }

    fn finish(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager) -> bool {
        self.clear_anim(t, c, m);
        self.abort_ik(t, t.now_ms);
        t.pd.attack_counter = 0.0;
        true
    }

    /// The firing loop (§3.5 of the notes): anim time crossing the fire frame fires.
    fn firing_loop(&mut self, t: &mut PedTasks, c: &mut PedCore, info: &WeaponInfo, weapon_state: u8) {
        let (s, e, f) = info.anim_loop(t.ducking);
        let f2 = (e - s) * 0.5 + f;
        let uid = self.anim.unwrap();
        let Some(a) = c.clump.by_uid_mut(uid) else { return };
        let len = a.hier.total_length;
        let reloading = weapon_state == ws::RELOADING;
        let mut tt = a.time;
        let dt = a.time_step();
        let crossed = |x: f32, tt: f32| tt > x && tt - dt <= x;
        let firing = matches!(self.last_cmd, GunCmd::Fire | GunCmd::FireBurst);
        // a. hold at the loop start while blending in or reloading.
        if firing {
            if a.blend < 0.99 || reloading {
                if a.has(af::PLAYING) && tt >= s && tt - dt < s {
                    a.flags &= !af::PLAYING;
                    a.set_current_time(s);
                    tt = s;
                }
            } else if !a.has(af::PLAYING) && tt == s {
                a.flags |= af::PLAYING;
            }
        }
        let playing = a.has(af::PLAYING);
        // b. fire triggers.
        if !info.has(wf::CONTINUOUSFIRE) {
            if playing && crossed(f, tt) && firing {
                self.fire_bits |= 1;
                self.has_fired = true;
                if self.burst_shots > 0 {
                    self.burst_shots -= 1;
                }
            }
            if info.has(wf::TWIN_PISTOL) && playing && crossed(f2, tt) && firing {
                self.fire_bits |= 2;
                self.has_fired = true;
                if self.burst_shots > 0 {
                    self.burst_shots -= 1;
                }
            }
        } else if firing && s < tt && tt < e && playing {
            let next = self.next();
            if !self.has_fired || (self.last_cmd == GunCmd::FireBurst && self.burst_shots > 0) || matches!(next, GunCmd::Fire | GunCmd::FireBurst) {
                self.fire_bits |= 1;
                self.has_fired = true;
                if next > self.last_cmd {
                    self.last_cmd = next;
                }
                self.next_cmd = Some(GunCmd::Null);
                self.burst_shots = if self.last_cmd == GunCmd::FireBurst && self.burst_shots > 0 { self.burst_shots - 1 } else { 0 };
            } else {
                a.flags &= !af::PLAYING;
                a.blend_delta = -4.0;
            }
        }
        // c. restart a stopped anim / aiming freezes at the loop start.
        let next = self.next();
        let a = c.clump.by_uid_mut(uid).unwrap();
        if !a.has(af::PLAYING) {
            if !(self.last_cmd == GunCmd::Aim && next <= GunCmd::Aim) && (a.time < len || a.blend_delta >= 0.0) {
                if self.last_cmd <= GunCmd::Reload && next <= GunCmd::Reload {
                    if a.blend > 0.0 && a.blend_delta >= 0.0 && !reloading {
                        a.flags |= af::PLAYING;
                    }
                } else if !info.has(wf::EXPANDS) {
                    a.blend_delta = -4.0;
                } else {
                    a.flags |= af::PLAYING;
                    if a.time <= s {
                        a.set_current_time(e);
                    }
                }
                if matches!(next, GunCmd::Fire | GunCmd::FireBurst) {
                    self.last_cmd = next;
                    self.next_cmd = Some(GunCmd::Null);
                    if next == GunCmd::FireBurst {
                        self.burst_shots = self.burst_length;
                    }
                } else if self.last_cmd == GunCmd::Aim && next != GunCmd::Aim {
                    self.last_cmd = GunCmd::Null;
                }
            }
        } else if self.last_cmd == GunCmd::Aim && ((a.time >= s && a.time - dt < s) || a.time + dt >= s) {
            a.flags &= !af::PLAYING;
            a.set_current_time(s);
        }
        // d. loop wrap.
        let next = self.next();
        let tt = a.time;
        if tt > e && tt - dt <= e {
            let bursting = self.last_cmd == GunCmd::FireBurst && self.burst_shots > 0 && next != GunCmd::Reload;
            if matches!(next, GunCmd::Fire | GunCmd::FireBurst) || bursting {
                a.set_current_time(s);
                a.set_playing(!reloading);
                if matches!(next, GunCmd::Fire | GunCmd::FireBurst) {
                    if next > self.last_cmd {
                        self.last_cmd = next;
                    }
                    if next == GunCmd::FireBurst && self.burst_shots == 0 {
                        self.burst_shots = self.burst_length;
                    }
                }
                self.next_cmd = Some(GunCmd::Null);
            } else if next == GunCmd::Aim {
                a.set_current_time(s);
                a.flags &= !af::PLAYING;
                self.last_cmd = GunCmd::Aim;
                self.next_cmd = Some(GunCmd::Null);
            }
        }
        // e. breakout.
        if a.time > info.breakout_time && self.next() == GunCmd::EndNow {
            self.finished = true;
            a.blend_delta = if a.has(af::NO_ROOT_PARTIAL_SUM) { -1.0 } else { -4.0 };
        }
    }

    /// `SetMoveAnim` (0x61E3F0): Gun_stand and the four-way GunMove strafes.
    fn set_move_anim(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager, info: &WeaponInfo, ts: f32) {
        if t.ducking {
            if matches!(self.last_cmd, GunCmd::Fire | GunCmd::FireBurst | GunCmd::Reload) {
                if let Some(d) = &mut t.duck {
                    d.force_stop_move();
                }
            }
            self.move_cmd = Vec2::ZERO;
            self.move_control = false;
            return;
        }
        let (mag, sum) = if !self.move_control || (self.last_cmd == GunCmd::Fire && !info.has(wf::MOVEFIRE)) {
            (0.0, 0.0)
        } else {
            (self.move_cmd.length(), self.move_cmd.x.abs() + self.move_cmd.y.abs())
        };
        let clump = &mut *c.clump;
        if sum >= 0.1 {
            if !info.has(wf::AIMWITHARM) {
                let mut f = 1.0;
                if let Some(gs) = clump.get_mut(anim_id::GUN_STAND) {
                    if gs.blend_delta >= 0.0 {
                        gs.blend_delta = 0.0;
                        gs.blend = (gs.blend - ts * 0.16).max(0.0);
                    }
                    f = 1.0 - (gs.blend + ts * gs.blend_delta * 0.02).clamp(0.0, 1.0);
                }
                let x = self.move_cmd.x / sum * f;
                let y = self.move_cmd.y / sum * f;
                let blends = [
                    (anim_id::GUNMOVE_R, x.max(0.0)),
                    (anim_id::GUNMOVE_L, (-x).max(0.0)),
                    (anim_id::GUNMOVE_BWD, y.max(0.0)),
                    (anim_id::GUNMOVE_FWD, (-y).max(0.0)),
                ];
                for (id, b) in blends {
                    let exists = clump.index_of(id).is_some();
                    if b > 0.0 || exists {
                        let i = match clump.index_of(id) {
                            Some(i) => i,
                            None => match clump.add_animation(m, group::DEFAULT, id) {
                                Some(i) => i,
                                None => continue,
                            },
                        };
                        let a = &mut clump.assocs[i];
                        a.blend = b;
                        if b > 0.0 {
                            a.blend_delta = 0.0;
                            a.flags |= af::PLAYING;
                            a.speed = info.move_speed * mag;
                        }
                    }
                }
                if self.last_cmd == GunCmd::Aim && !info.has(wf::MOVEAIM) {
                    if let Some(a) = self.anim.and_then(|u| clump.by_uid_mut(u)) {
                        if a.blend > 0.0 && a.blend_delta >= 0.0 {
                            a.blend_delta = -4.0;
                        }
                    }
                }
                self.move_control = false;
                return;
            }
            let id = if self.move_cmd.x > 0.75 {
                anim_id::GUNMOVE_R
            } else if self.move_cmd.x < -0.75 {
                anim_id::GUNMOVE_L
            } else if self.move_cmd.y > 0.0 {
                anim_id::GUNMOVE_BWD
            } else {
                anim_id::GUNMOVE_FWD
            };
            if let Some(i) = clump.blend_animation(m, group::DEFAULT, id, 8.0) {
                clump.assocs[i].flags |= af::PLAYING;
            }
            t.move_state = 0;
            return;
        }
        let a = if !info.has(wf::AIMWITHARM) {
            clump.blend_animation(m, group::DEFAULT, anim_id::GUN_STAND, 8.0)
        } else {
            t.move_state = 1;
            clump.blend_animation(m, t.anim_group, anim_id::IDLE, 8.0)
        };
        let full = a.is_some_and(|i| clump.assocs[i].blend > 0.95);
        for id in anim_id::GUNMOVE_FWD..=anim_id::GUNMOVE_R {
            if let Some(s) = clump.get_mut(id) {
                s.flags &= !af::PLAYING;
                if full {
                    s.set_current_time(0.0);
                }
            }
        }
        self.move_cmd = Vec2::ZERO;
        self.move_control = false;
    }

    /// `AimGun` (0x61ED10) for the player without a target entity.
    fn aim_gun(&mut self, t: &mut PedTasks, c: &mut PedCore, ctx: &Ctx, info: &WeaponInfo) {
        let blend = self.anim.and_then(|u| c.clump.by_uid(u)).map_or(0.0, |a| a.blend);
        let now = ctx.now_ms as i64;
        if !info.has(wf::AIMWITHARM) || t.ducking {
            // Torso IK mode (flag 0x10): the arm / look chains go.
            if t.torso_ik_mode.is_none_or(|m| !m) {
                self.abort_ik(t, ctx.now_ms);
            }
            t.torso_ik_mode = Some(true);
            match self.target.filter(|_| !t.is_player) {
                // PointGunAtPosition: the yaw / pitch to the target.
                Some((_, tgt)) => {
                    let d = tgt - c.p.matrix.pos;
                    let yaw = (-d.x).atan2(d.y);
                    let pitch = d.z.atan2(d.truncate().length());
                    point_gun_in_direction(t, c, yaw, pitch, blend);
                }
                None => point_gun_in_direction(t, c, *c.cur_rot, t.pd.look_pitch, blend),
            }
            return;
        }
        t.torso_ik_mode = Some(false);
        let twin = info.has(wf::TWIN_PISTOL);
        if let Some((_, tgt)) = self.target.filter(|_| !t.is_player) {
            // m_pTarget: look at / point the arm(s) at the target's spine, twist the torso.
            if !self.look_ik && blend > 0.98 {
                t.ikm.look_at(tgt, 9_999_999, 0.25, 250, now);
                self.look_ik = true;
            } else if self.look_ik {
                t.ikm.set_target(0, tgt);
            }
            if !self.arm_ik {
                t.ikm.point_arm(0, tgt, 0.5, 250, now);
                if twin {
                    t.ikm.point_arm(1, tgt, 0.5, 250, now);
                }
                self.arm_ik = true;
            } else {
                t.ikm.set_target(1, tgt);
                if twin {
                    t.ikm.set_target(2, tgt);
                }
            }
            rotate_torso_for_arm(c, tgt);
        } else if t.pd.free_aim && matches!(ctx.cam.mode, 53 | 65) {
            let src = c.p.matrix.pos + Vec3::new(0.0, 0.0, 0.7);
            let tgt = ctx.cam.target_vector(20.0, src).1;
            if blend > 0.98 {
                t.ikm.look_at(tgt, 9_999_999, 0.25, 250, now);
                self.look_ik = true;
            }
            t.ikm.point_arm(0, tgt, 0.5, 250, now);
            if twin {
                t.ikm.point_arm(1, tgt, 0.5, 250, now);
            }
            self.arm_ik = true;
            rotate_torso_for_arm(c, tgt);
        } else if !(self.arm_ik && self.look_ik) {
            // Straight ahead: (0, 2, 0) in the right upper arm's space.
            if let Some(tgt) = arm_target(t, c, ctx) {
                t.ikm.look_at(tgt, 9_999_999, 0.25, 250, now);
                t.ikm.point_arm(0, tgt, 0.5, 250, now);
                if twin {
                    t.ikm.point_arm(1, tgt, 0.5, 250, now);
                }
                self.arm_ik = true;
            }
        }
    }
}

/// Where a one-handed gun points: 20 units along the crosshair ray in free aim, otherwise
/// straight ahead of the right upper arm.
fn arm_target(t: &PedTasks, c: &PedCore, ctx: &Ctx) -> Option<Vec3> {
    if t.pd.free_aim && matches!(ctx.cam.mode, 53 | 65) {
        let src = c.p.matrix.pos + Vec3::new(0.0, 0.0, 0.7);
        Some(ctx.cam.target_vector(20.0, src).1)
    } else {
        let f = c.clump.frame_of_tag(bone::R_UPPERARM)?;
        let ltm = c.clump.ltm(f);
        let local = ltm.transform_point3(Vec3::new(0.0, 2.0, 0.0));
        Some(c.p.matrix.transform(local))
    }
}

/// `CPedIK::PointGunInDirection` (0x5FDC00) with `MoveLimb(blend)`: rotate the Spine1
/// keyframe by the torso pitch (about a heading-compensated axis) and yaw (local X).
fn point_gun_in_direction(t: &mut PedTasks, c: &mut PedCore, yaw: f32, pitch: f32, blend: f32) {
    let yaw_rel = crate::ped::limit_radian_angle(yaw - *c.cur_rot);
    // ms_torsoInfo: yaw ±50°, pitch +60° / -55°.
    t.ik.torso_yaw = (yaw_rel * blend).clamp(-0.872_664_6, 0.872_664_6);
    t.ik.torso_pitch = (pitch * blend).clamp(-0.959_931_1, 1.047_197_6);
    let (Some(spine), Some(spine1)) = (c.clump.frame_of_tag(bone::SPINE), c.clump.frame_of_tag(bone::SPINE1)) else {
        return;
    };
    // M = Spine's world matrix; a = -LimitRadianAngle(atan2(-M.at.y, -M.at.x) - cur).
    let world_spine = c.p.matrix.rotate(c.clump.ltm(spine).z_axis.truncate());
    let a = -crate::ped::limit_radian_angle((-world_spine.y).atan2(-world_spine.x) - *c.cur_rot);
    let axis = Vec3::new(0.0, -a.sin(), a.cos());
    let q = &mut c.clump.pose[spine1].0;
    *q = *q * Quat::from_axis_angle(axis, t.ik.torso_pitch) * Quat::from_axis_angle(Vec3::X, t.ik.torso_yaw);
}

/// `CPedIK::RotateTorsoForArm` (0x5FDF90): beyond +45° / -60° the spine1 and neck twist
/// up to +45° / -20° more (half to the neck).
fn rotate_torso_for_arm(c: &mut PedCore, target: Vec3) {
    let p = c.p.matrix.pos;
    let mut d = (-(target.x - p.x)).atan2(target.y - p.y);
    let cur = *c.cur_rot;
    while d - cur > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d - cur < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    d -= cur;
    let twist = if d > std::f32::consts::FRAC_PI_4 {
        (d - std::f32::consts::FRAC_PI_4).min(std::f32::consts::FRAC_PI_4)
    } else if d < -std::f32::consts::FRAC_PI_3 {
        (d + std::f32::consts::FRAC_PI_3).max(-0.349_065_9)
    } else {
        return;
    };
    if twist == 0.0 {
        return;
    }
    let mut tw = twist;
    if let Some(neck) = c.clump.frame_of_tag(bone::NECK) {
        let q = &mut c.clump.pose[neck].0;
        *q = *q * Quat::from_axis_angle(Vec3::X, tw * 0.5);
        tw *= 0.5;
    }
    if let Some(s1) = c.clump.frame_of_tag(bone::SPINE1) {
        let q = &mut c.clump.pose[s1].0;
        *q = *q * Quat::from_axis_angle(Vec3::X, tw);
    }
}

/// `CTaskSimplePlayerOnFoot::ProcessPlayerWeapon` (0x6859A0), PC mouse path, for
/// instant-hit and area-effect weapons.
pub fn process_player_weapon(t: &mut PedTasks, c: &mut PedCore, ctx: &Ctx) {
    let Some(info) = t.active_info() else { return };
    let w = *t.active_weapon();
    // 1. First-person weapons: the aim button switches to their camera (rocket launchers only
    //    here; FireSniper / the camera weapon are not ported).
    if info.has(wf::FIRSTPERSON) && t.pad.aim && matches!(w.ty, wt::RLAUNCHER | wt::RLAUNCHER_HS) {
        t.cam_request = if w.ty == wt::RLAUNCHER { 8 } else { 51 };
    }
    // USE: the detonator on a fresh press.
    if info.fire_type == fire::USE {
        if t.pad.fire_just_down && w.ty == wt::DETONATOR && t.weapons[t.active_slot].can_fire(1) {
            t.requests.push(WorldRequest::Detonate);
            let slot = t.active_slot;
            t.weapons[slot].total_ammo = 1;
            t.weapons[slot].ammo_in_clip = 1;
            t.weapons[slot].after_shot(ctx.now_ms, &info, 0, true, true);
        }
        return;
    }
    // Thrown weapons: CTaskSimpleThrowProjectile on a fresh press, released on button up.
    if info.fire_type == fire::PROJECTILE && !matches!(w.ty, wt::RLAUNCHER | wt::RLAUNCHER_HS) {
        if t.pad.fire && t.move_state != 7 && t.pd.chosen_slot == t.active_slot {
            match &mut t.throw {
                None => {
                    if t.pad.fire_just_down && t.gun.is_none() {
                        t.throw = Some(crate::pedtask::ThrowTask::new(ctx.now_ms));
                    }
                }
                Some(th) => th.control_throw(t.pad.fire_just_down, ctx.now_ms),
            }
        } else if let Some(th) = &mut t.throw {
            th.control_throw(true, ctx.now_ms);
        }
        return;
    }
    if info.fire_type == fire::MELEE {
        t.player_melee();
    }
    // Fire held.
    if t.pad.fire && t.move_state != 7 && t.pd.chosen_slot == t.active_slot {
        let rocket = matches!(w.ty, wt::RLAUNCHER | wt::RLAUNCHER_HS) && t.cam_request != 0;
        if rocket || (matches!(info.fire_type, fire::INSTANT_HIT | fire::AREA_EFFECT) && !info.has(wf::FIRSTPERSON)) {
            let mut cmd = Some(GunCmd::Fire);
            if w.state == ws::RELOADING {
                cmd = (t.pad.aim || t.pd.free_aim).then_some(GunCmd::Aim);
            }
            if let Some(cmd) = cmd {
                match &mut t.gun {
                    None => {
                        t.gun = Some(UseGun::new(cmd));
                        t.pd.attack_counter = 0.0;
                    }
                    Some(g) => g.control_gun(cmd),
                }
            }
            if !t.pad.aim {
                t.pd.look_pitch = if w.ty == wt::EXTINGUISHER { 0.349_065_87 } else { 0.0 };
            }
        }
    }
    // Reload.
    if w.state == ws::RELOADING && info.has(wf::RELOAD) {
        if let Some(g) = &mut t.gun {
            g.control_gun(GunCmd::Reload);
        } else if !t.ducking {
            if c.clump.get(anim_id::WEAPON_RELOAD).is_none() {
                if let Some(m) = t.anims.clone() {
                    c.clump.blend_animation(&m, info.anim_group, anim_id::WEAPON_RELOAD, 4.0);
                }
            }
        } else if info.has(wf::CROUCHFIRE) && t.duck.as_ref().is_some_and(|d| !d.busy_aim(c.clump)) {
            if c.clump.get(anim_id::WEAPON_CROUCHRELOAD).is_none() {
                if let Some(m) = t.anims.clone() {
                    c.clump.blend_animation(&m, info.anim_group, anim_id::WEAPON_CROUCHRELOAD, 4.0);
                }
            }
        }
    }
    // Aim button.
    let aiming = t.pad.aim && t.pd.chosen_slot == t.active_slot && !(t.move_state == 7 && info.fire_type != fire::MELEE);
    if aiming && !info.has(wf::FIRSTPERSON) {
        if info.has(wf::CANAIM) || info.has(wf::ONLYFREEAIM) {
            t.pd.free_aim = true; // the mouse never locks on
        }
        if matches!(info.fire_type, fire::MELEE | fire::PROJECTILE | fire::USE) {
            return;
        }
        t.cam_request = 53;
        match &mut t.gun {
            None => t.gun = Some(UseGun::new(GunCmd::Aim)),
            Some(g) => g.control_gun(GunCmd::Aim),
        }
    } else if t.pad.aim && t.cam_request != 0 {
        // First-person aim (rocket camera): the gun task holds AIM.
        match &mut t.gun {
            None => t.gun = Some(UseGun::new(GunCmd::Aim)),
            Some(g) => g.control_gun(GunCmd::Aim),
        }
    } else {
        if let Some(g) = &mut t.gun {
            if !t.pad.fire && (t.pad.walk_ud.abs() > 50.0 || t.pad.walk_lr.abs() > 50.0) {
                g.control_gun(GunCmd::EndNow);
            } else {
                g.stop_aim();
            }
            if !info.has(wf::AIMWITHARM) && ctx.cam.mode == 4 {
                *c.aim_rot = (-ctx.cam.front.x).atan2(ctx.cam.front.y);
            }
        }
        t.pd.free_aim = false;
    }
}

/// `CTaskSimpleUseGun::SetPedPosition` → `FireGun` (0x61EB10) → `CWeapon::Fire` (0x742300):
/// returns the instant-hit shots of this frame and updates ammo / gun flash.
pub fn fire_guns(t: &mut PedTasks, clump: &Clump, p: &crate::physical::Physical, owner: EntityId, now: u32) -> Vec<WorldRequest> {
    let mut out = Vec::new();
    let Some(g) = &mut t.gun else { return out };
    let bits = std::mem::take(&mut g.fire_bits);
    let Some(info) = g.info.clone() else { return out };
    let target = g.target;
    for (left, bit) in [(false, 1u8), (true, 2u8)] {
        if bits & bit == 0 {
            continue;
        }
        let tag = if left { bone::L_HAND } else { bone::R_HAND };
        let Some(f) = clump.frame_of_tag(tag) else { continue };
        let ltm = clump.ltm(f);
        let origin = p.matrix.transform(ltm.w_axis.truncate());
        let effect = p.matrix.transform(ltm.transform_point3(info.fire_offset + Vec3::new(0.15, 0.0, 0.0)));
        let slot = t.active_slot;
        let ty = t.weapons[slot].ty;
        let Some(infos) = t.infos.clone() else { continue };
        let std_clip = infos.get(ty, 1).ammo_clip;
        // FireProjectile refuses rockets outside the 1st-person rocket cameras (no ammo used).
        let rocket = matches!(ty, wt::RLAUNCHER | wt::RLAUNCHER_HS);
        if rocket && !matches!(t.cam.mode, 34 | 7 | 8 | 51 | 42 | 39 | 40 | 52) {
            continue;
        }
        if !t.weapons[slot].can_fire(std_clip) {
            continue;
        }
        let set_time = match ty {
            22..=24 | 28..=33 | 38 => {
                out.push(WorldRequest::FireInstantHit(InstantHit {
                    owner,
                    ty,
                    skill: t.weapon_skill(ty),
                    origin,
                    effect,
                    is_player: t.is_player,
                    target,
                    accuracy: t.accuracy,
                    ducking: t.ducking,
                    attack_counter: t.pd.attack_counter,
                    model_flash: t.weapon_model >= 0,
                }));
                false // the anim sets the rate on foot
            }
            25..=27 => {
                out.push(WorldRequest::FireInstantHit(InstantHit {
                    owner,
                    ty,
                    skill: t.weapon_skill(ty),
                    origin,
                    effect,
                    is_player: t.is_player,
                    target,
                    accuracy: t.accuracy,
                    ducking: t.ducking,
                    attack_counter: t.pd.attack_counter,
                    model_flash: t.weapon_model >= 0,
                }));
                true
            }
            wt::RLAUNCHER | wt::RLAUNCHER_HS => {
                // Muzzle = hand bone · fireOffset (FireGun passes the muzzle as the effect point).
                let muzzle = p.matrix.transform(ltm.transform_point3(info.fire_offset));
                out.push(WorldRequest::FireProjectile { owner, ty, effect: muzzle, force: 0.0, cam: Some((t.cam.front, t.cam.up)) });
                true
            }
            wt::FTHROWER | wt::SPRAYCAN | wt::EXTINGUISHER => {
                out.push(WorldRequest::FireAreaEffect {
                    owner,
                    ty,
                    src: effect,
                    mouse_cam: t.cam.mode == 4,
                    look_pitch: Some(t.pd.look_pitch),
                });
                false
            }
            _ => continue,
        };
        let reload = infos.reload_time(&info);
        t.weapons[slot].after_shot(now, &info, reload, t.is_player, set_time);
        if t.weapons[slot].state == ws::FIRING {
            // CPed::DoGunFlash(250, left): full alpha and a random roll of the flash frame.
            let i = left as usize;
            t.gun_flash[i] = (10000, (10000 / 250) as i16);
            let r = (now.wrapping_mul(1_103_515_245).wrapping_add(12345) >> 16) & 0x7FFF;
            t.gun_flash_roll += r as f32 * (1.0 / 32767.0) * 720.0 - 360.0;
        }
    }
    out
}
