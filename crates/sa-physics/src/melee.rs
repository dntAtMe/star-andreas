//! Melee combat (melee.md): melee.dat, `CTaskSimpleFight`, `PlayerControlFighter`, the
//! ProcessPlayerWeapon melee branch and `FightStrike` with its ped / car / object hits.
//!
//! Not ported: lock-on and the mouse target (the fight target is always null), the knife
//! stealth kill, the pistol whip, NPC fighting (AI ChooseAttackMove, step anims), audio,
//! CGlass, ObjectDamage, the victims' hit anims (CEventDamage), crimes and sound events,
//! the anim-block streaming fallback (all blocks are resident).

use std::sync::Arc;

use glam::{Vec2, Vec3};

use crate::{
    Ctx,
    anim::{AnimManager, Clump, af, group},
    collision::{ColModel, ColSphere, MAX_COLPOINTS, Surf, process_col_models},
    colpoint::ColPoint,
    effects::WorldRequest,
    pedtask::{PedCore, PedTasks, radian_angle_between_points},
    physical::{EntityType, Matrix, Status},
    weapon::{fire, wf},
    world::{EntityId, World},
};

/// `CTaskSimpleFight::GetComboType(const char*)` (0x61DB30).
pub fn combo_type_of(name: &str) -> u8 {
    match name {
        "BBALLBAT" => 8,
        "KNIFE" => 9,
        "GOLFCLUB" => 10,
        "SWORD" => 11,
        "CHAINSAW" => 12,
        "DILDO" => 13,
        "FLOWERS" => 14,
        _ => 4,
    }
}

/// melee.dat combo flags (+0x84).
pub mod mf {
    pub const ATTACK_2: u16 = 0x1;
    pub const ATTACK_3: u16 = 0x2;
    pub const GROUND: u16 = 0x4;
    pub const RUNNING: u16 = 0x8;
    pub const BLOCK: u16 = 0x200;
    pub const OWN_IDLE: u16 = 0x400;
}

/// Fight commands (`ControlFight`).
pub mod cmd {
    pub const NONE: i8 = 0;
    pub const END: i8 = 1;
    pub const BLOCK: i8 = 2;
    pub const SHUFFLE_F: i8 = 3;
    pub const SHUFFLE_L: i8 = 4;
    pub const SHUFFLE_B: i8 = 5;
    pub const SHUFFLE_R: i8 = 6;
    pub const ATTACK: i8 = 11;
    pub const STYLE: i8 = 12;
    pub const END_IDLE: i8 = 15;
    pub const END_WALK: i8 = 16;
    pub const END_RUN: i8 = 17;
    pub const END_CROUCH: i8 = 18;
}

/// Anim ids in a combo's group: FightX_1.. = 214 + move, block 222, idle 223.
const ANIM_ATTACK: i16 = 214;
const ANIM_BLOCK: i16 = 0xDE;
const ANIM_IDLE: i16 = 0xDF;
const FIGHT2IDLE: i16 = 0x2F;
/// FightSH_FWD / Left / BWD / Right (default group).
const SH_FWD: i16 = 41;
const SH_LEFT: i16 = 42;
const SH_BWD: i16 = 43;
const SH_RIGHT: i16 = 44;
const WEAPON_CROUCH: i16 = 0x37;

/// One `m_aComboData` entry.
#[derive(Debug, Clone)]
pub struct ComboData {
    pub group: usize,
    pub range: f32,
    pub hit_time: [f32; 5],
    pub chain_time: [f32; 5],
    pub hit_radius: [f32; 5],
    pub hit_level: [u8; 5],
    pub damage: [u8; 5],
    pub ground_loop_time: f32,
    pub block_hold_time: f32,
    pub block_alt_hold_time: f32,
    pub flags: u16,
}

#[derive(Debug, Clone)]
pub struct MeleeData {
    /// Index = combo type − 4.
    pub combos: Vec<ComboData>,
    pub hit_offsets: [Vec3; 7],
}

impl MeleeData {
    pub fn load(d: &sa_formats::meleedat::MeleeDat, group_of: impl Fn(&str) -> Option<usize>) -> Self {
        let melee_1 = group_of("melee_1").unwrap_or(33);
        let combos = d
            .combos
            .iter()
            .map(|c| ComboData {
                // The loader keeps 33 (melee_1) when no group matches.
                group: group_of(&c.anim_group).unwrap_or(melee_1),
                range: c.range,
                hit_time: c.hit_time,
                chain_time: c.chain_time,
                hit_radius: c.hit_radius,
                hit_level: c.hit_level,
                damage: c.damage,
                ground_loop_time: c.ground_loop_time,
                block_hold_time: c.block_hold_time,
                block_alt_hold_time: c.block_alt_hold_time,
                flags: c.flags,
            })
            .collect();
        Self { combos, hit_offsets: d.hit_offsets.map(Vec3::from) }
    }

    pub fn combo(&self, ty: i8) -> &ComboData {
        &self.combos[(ty as i32 - 4).clamp(0, self.combos.len() as i32 - 1) as usize]
    }
}

/// `FightStrike` input (sent to the world: it needs the other entities).
#[derive(Debug, Clone, Copy)]
pub struct Strike {
    pub owner: EntityId,
    pub is_player: bool,
    /// The attacker's matrix with the hit position as translation.
    pub mat: Matrix,
    pub radius: f32,
    pub weapon: u32,
    /// `GetStrikeDamage` (before the ftol for peds).
    pub damage: f32,
    pub combo: i8,
    pub mv: i8,
}

/// `CTaskSimpleFight` (0x28 bytes).
#[derive(Debug, Clone)]
pub struct FightTask {
    pub finished: bool,
    in_control: bool,
    idle_period: u32,
    idle_counter: u32,
    chain_counter: i8,
    pub target: Option<EntityId>,
    /// m_pAnim (attack / block), m_pIdleAnim: assoc uids.
    anim: Option<u32>,
    idle_anim: Option<u32>,
    pub combo_set: i8,
    pub current_move: i8,
    next_cmd: i8,
    pub last_cmd: i8,
}

impl FightTask {
    /// ctor 0x61C470.
    pub fn new(target: Option<EntityId>, command: i8, idle_period: u32) -> Self {
        Self {
            finished: false,
            in_control: true,
            idle_period: idle_period.min(60000),
            idle_counter: 0,
            chain_counter: 0,
            target,
            anim: None,
            idle_anim: None,
            combo_set: -1,
            current_move: -1,
            next_cmd: command,
            last_cmd: 0,
        }
    }

    /// `ControlFight` (0x61C5E0): the maximum command of the frame wins.
    pub fn control_fight(&mut self, target: Option<EntityId>, command: i8) {
        self.in_control = true;
        self.target = target;
        if self.next_cmd < command {
            self.next_cmd = command;
        }
    }

    /// `MakeAbortable` (0x6239F0), URGENT / IMMEDIATE without an event.
    pub fn make_abortable(&mut self, t: &mut PedTasks, clump: &mut Clump, m: &AnimManager, immediate: bool) {
        if let Some(u) = self.anim.take() {
            if immediate {
                if let Some(a) = clump.by_uid_mut(u) {
                    a.blend_delta = -1000.0;
                }
            }
        }
        if let Some(u) = self.idle_anim.take() {
            if clump.by_uid(u).is_some_and(|a| a.blend > 0.0 && a.blend_delta >= 0.0) {
                clump.blend_animation(m, t.anim_group, crate::anim::anim_id::IDLE, if immediate { 1000.0 } else { 16.0 });
            }
        }
        t.pd.fight = Vec2::ZERO;
        self.shuffle(t, clump, m);
        self.finished = true;
    }

    /// `FinishMeleeAnimCB` (0x61DAE0) for the anims of the last update: the attack/block anim
    /// on finish, the idle anim on delete.
    fn callbacks(&mut self, clump: &Clump) {
        let mut fired = Vec::new();
        if let Some(u) = self.anim {
            if clump.finished.contains(&u) {
                fired.push((u, clump.by_uid(u).map(|a| a.id)));
            } else if clump.by_uid(u).is_none() {
                self.anim = None; // deleted without finishing: nothing left to point at
            }
        }
        if let Some(u) = self.idle_anim {
            if clump.deleted.contains(&u) || clump.by_uid(u).is_none() {
                fired.push((u, None));
            }
        }
        for (u, id) in fired {
            if self.anim == Some(u) {
                self.anim = None;
            } else if self.idle_anim == Some(u) {
                self.idle_anim = None;
            }
            if id == Some(FIGHT2IDLE) {
                self.finished = true;
            }
            if self.idle_anim.is_none() && matches!(self.last_cmd, 1 | 15 | 16 | 17) {
                self.finished = true;
            }
        }
    }

    /// `GetComboType(ped, cmd)` (0x61C7F0); every anim block is resident here.
    fn get_combo_type(&self, t: &PedTasks, command: i8) -> i8 {
        let Some(md) = t.melee.as_deref() else { return 4 };
        if !matches!(command, 0 | 2 | 11..=14) {
            return 0;
        }
        let base = t.active_info().map_or(4, |i| i.base_combo as i8);
        if command == cmd::STYLE {
            t.fight_style
        } else if base == 4 && !matches!(command, 0 | 2) {
            4
        } else {
            let c = if base == 4 { t.fight_style } else { base };
            if command == 0 && md.combo(c).flags & mf::OWN_IDLE == 0 {
                return 4;
            }
            c
        }
    }

    /// The idle combo of §3.5 / StartAnim case 0.
    fn idle_combo(&self, t: &PedTasks) -> i8 {
        let Some(md) = t.melee.as_deref() else { return 4 };
        let mut c = t.active_info().map_or(4, |i| i.base_combo as i8);
        if c == 4 {
            c = t.fight_style;
        }
        if md.combo(c).flags & mf::OWN_IDLE == 0 { 4 } else { c }
    }

    fn blend_idle(&mut self, t: &PedTasks, clump: &mut Clump, m: &AnimManager, delta: f32) {
        let Some(md) = t.melee.as_deref() else { return };
        let g = md.combo(self.idle_combo(t)).group;
        self.idle_anim = clump.blend_animation(m, g, ANIM_IDLE, delta).map(|i| clump.assocs[i].uid);
    }

    /// `CanStrikeTargetOnGround` (0x61D6F0) for the player without a target and without
    /// other peds: standing on a car roof.
    fn can_strike_target_on_ground(&self, t: &PedTasks, c: &PedCore) -> bool {
        self.target.is_none() && t.is_player && c.ground_car
    }

    /// Player `ChooseAttackMove` (0x624710).
    fn choose_attack_move(&mut self, t: &mut PedTasks, c: &mut PedCore) -> i32 {
        let Some(md) = t.melee.clone() else { return -1 };
        let mut res = if (11..=14).contains(&self.next_cmd) && self.combo_set >= 4 { -1 } else { 1 };
        let mut f = md.combo(self.combo_set.max(4)).flags & 0xFF;
        if (5..=7).contains(&self.combo_set) {
            f &= t.fight_moves as u16;
        }
        if res < 0 {
            if self.anim.is_some() && !matches!(self.current_move, 3 | 4) {
                t.move_state = 1;
                let max = if f & mf::ATTACK_3 != 0 {
                    2
                } else if f & mf::ATTACK_2 != 0 {
                    1
                } else {
                    0
                };
                if self.can_strike_target_on_ground(t, c) {
                    res = -1;
                } else if self.next_cmd == self.last_cmd || self.chain_counter > 2 {
                    res = self.current_move as i32 + 1;
                    if res > max {
                        res = -1;
                    }
                } else {
                    self.chain_counter += 1;
                    let k = self.current_move as i32 - self.last_cmd as i32 + self.next_cmd as i32;
                    res = if k & 1 != 0 {
                        0
                    } else if k & 2 != 0 {
                        1
                    } else {
                        2
                    };
                    if res > max {
                        res = 0;
                    }
                }
            } else if t.move_state > 4 {
                if f & mf::RUNNING == 0 {
                    self.combo_set = 4;
                }
                res = 4;
            } else if self.can_strike_target_on_ground(t, c) {
                if self.anim.is_some() && self.next_cmd != self.last_cmd {
                    return -1;
                }
                if f & mf::GROUND == 0 {
                    self.combo_set = 4;
                }
                return 3;
            } else {
                self.chain_counter = 0;
                res = 0;
            }
        }
        // Auto-turn to the nearest ped within 2 m: there are no other peds yet.
        res
    }

    /// The player shuffle (0x61C9B0): FightSH_* blends set directly from pd+0x0C/+0x10.
    fn shuffle(&mut self, t: &mut PedTasks, clump: &mut Clump, m: &AnimManager) {
        let v = t.pd.fight;
        if (self.next_cmd != 0 || self.combo_set != 0) && v.length() >= 0.1 {
            let s = 1.0 / (v.x.abs() + v.y.abs());
            let (fx, fy) = (s * v.x, s * v.y);
            for (id, b) in [(SH_RIGHT, fx.max(0.0)), (SH_LEFT, (-fx).max(0.0)), (SH_BWD, fy.max(0.0)), (SH_FWD, (-fy).max(0.0))] {
                let i = clump.index_of(id).or_else(|| clump.add_animation(m, group::DEFAULT, id));
                if let Some(i) = i {
                    let a = &mut clump.assocs[i];
                    a.blend = b;
                    a.blend_delta = 0.0;
                }
            }
            self.combo_set = 1;
            self.last_cmd = self.next_cmd;
            self.next_cmd = 0;
        } else {
            for id in [SH_FWD, SH_LEFT, SH_BWD, SH_RIGHT] {
                if let Some(a) = clump.get_mut(id) {
                    a.blend_delta = -8.0;
                    a.flags |= af::DELETE_BLENDED_OUT;
                }
            }
            self.combo_set = 0;
            self.last_cmd = 0;
            self.next_cmd = 0;
        }
    }

    /// `StartAnim(ped, move)` (0x623B10).
    fn start_anim(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager, mv: i32) {
        let Some(md) = t.melee.clone() else { return };
        if mv < 0 {
            self.next_cmd = 0;
            return;
        }
        self.anim = None; // SetDeleteCallback(DefaultAnimCB): detach
        match self.next_cmd {
            0 => {
                self.combo_set = 0;
                self.current_move = 0;
                if t.move_state > 3 && t.is_player {
                    self.finished = true;
                    self.last_cmd = 0;
                    self.next_cmd = 0;
                    return;
                }
                match self.idle_anim.and_then(|u| c.clump.by_uid(u)) {
                    None => self.blend_idle(t, c.clump, m, 8.0),
                    Some(a) if a.blend < 1.0 && a.blend_delta <= 0.0 => self.blend_idle(t, c.clump, m, 8.0),
                    _ => {}
                }
                if t.is_player {
                    t.pd.fight = Vec2::ZERO;
                    self.shuffle(t, c.clump, m);
                }
                self.combo_set = 0;
            }
            1 | 15..=18 => {
                let n = self.next_cmd;
                if t.is_player {
                    t.pd.fight = Vec2::ZERO;
                    self.shuffle(t, c.clump, m);
                    if n == cmd::END_RUN {
                        t.pd.mbr = 2.0;
                    } else if n == cmd::END_WALK {
                        t.pd.mbr = 1.0;
                    }
                } else {
                    t.move_state = match n {
                        17 => 6,
                        16 => 4,
                        _ => 1,
                    };
                }
                use crate::anim::anim_id;
                let g = t.anim_group;
                match n {
                    17 => _ = c.clump.blend_animation(m, g, anim_id::RUN, 8.0),
                    16 => _ = c.clump.blend_animation(m, g, anim_id::WALK, 8.0),
                    15 => _ = c.clump.blend_animation(m, g, anim_id::IDLE, 4.0),
                    18 if t.ducking && t.duck.is_some() => {
                        _ = c.clump.blend_animation(m, group::DEFAULT, WEAPON_CROUCH, 4.0);
                    }
                    _ => _ = c.clump.blend_animation(m, g, anim_id::IDLE, 2.0),
                }
                match self.idle_anim.and_then(|u| c.clump.by_uid_mut(u)) {
                    Some(a) => a.flags &= !af::PLAYING,
                    None => self.finished = true,
                }
                self.next_cmd = cmd::END_WALK;
            }
            2 => {
                self.current_move = 0;
                if md.combo(self.combo_set.max(4)).flags & mf::BLOCK == 0 {
                    self.combo_set = 4;
                }
                let g = md.combo(self.combo_set).group;
                self.anim = c.clump.blend_animation(m, g, ANIM_BLOCK, 8.0).map(|i| {
                    c.clump.assocs[i].finish_cb = true;
                    c.clump.assocs[i].uid
                });
            }
            3..=10 => {
                if t.is_player {
                    // The player shuffles through 0x61C9B0.
                } else {
                    // NPC step anims are not ported.
                }
            }
            11..=14 => {
                self.current_move = mv as i8;
                let combo = md.combo(self.combo_set.max(4)).clone();
                self.anim = c.clump.blend_animation(m, combo.group, ANIM_ATTACK + mv as i16, 8.0).map(|i| {
                    let a = &mut c.clump.assocs[i];
                    a.finish_cb = true;
                    if mv == 3 && a.time != 0.0 {
                        a.set_current_time(combo.ground_loop_time);
                    }
                    if t.is_player {
                        // CStats::GetFatAndMuscleModifier(3) at the default stats.
                        a.speed = fat_muscle_modifier_speed(t);
                    }
                    a.uid
                });
                if t.is_player && mv < 4 {
                    t.move_state = 1;
                }
            }
            _ => {}
        }
        self.last_cmd = self.next_cmd;
        self.next_cmd = 0;
    }

    /// `ProcessPed` (0x629920). Returns true when finished.
    pub fn process_ped(&mut self, t: &mut PedTasks, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) -> bool {
        self.callbacks(c.clump);
        if self.finished {
            if let Some(u) = self.idle_anim.take() {
                if c.clump.by_uid(u).is_some_and(|a| a.blend > 0.0 && a.blend_delta >= 0.0) {
                    c.clump.blend_animation(m, t.anim_group, crate::anim::anim_id::IDLE, 8.0);
                }
            }
            return true;
        }
        let Some(md) = t.melee.clone() else { return true };
        let dt_ms = (ctx.ts * 0.02 * 1000.0) as i32 as u32;
        if self.combo_set != 0 && self.in_control {
            self.idle_counter = 0;
        } else {
            self.idle_counter = (self.idle_counter + dt_ms) & 0xFFFF;
        }
        let anim_id = self.anim.and_then(|u| c.clump.by_uid(u)).map(|a| a.id);
        if !self.in_control && (self.next_cmd < 1 || anim_id == Some(FIGHT2IDLE)) {
            return false;
        }
        if self.idle_anim.is_none() {
            if matches!(self.last_cmd, 1 | 15 | 16 | 17) {
                self.finished = true;
            } else if self.anim.is_none() && (t.move_state <= 4 || !t.is_player) {
                self.blend_idle(t, c.clump, m, 4.0);
                t.move_state = 1;
                self.combo_set = 0;
                self.last_cmd = 0;
            }
        }
        if self.anim.is_none() {
            if t.is_player && self.idle_counter > self.idle_period && self.next_cmd == 0 && self.combo_set == 0 {
                self.next_cmd = cmd::END;
            }
            if (self.next_cmd != 0 || self.combo_set != 0) && self.last_cmd != cmd::END_WALK {
                self.combo_set = self.get_combo_type(t, self.next_cmd);
                if (3..=6).contains(&self.next_cmd) && t.is_player {
                    self.shuffle(t, c.clump, m);
                } else {
                    let mv = self.choose_attack_move(t, c);
                    self.start_anim(t, c, m, mv);
                }
            }
        } else if let Some(u) = self.anim {
            let cs = self.combo_set;
            if cs >= 4 && self.last_cmd == cmd::BLOCK {
                let combo = md.combo(cs);
                let (hold, alt) = (combo.block_hold_time, combo.block_alt_hold_time);
                if self.next_cmd == cmd::BLOCK {
                    if let Some(a) = c.clump.by_uid_mut(u) {
                        if a.has(af::PLAYING) {
                            let (tt, dt) = (a.time, a.time_step());
                            if (tt < hold && tt + dt >= hold) || (tt >= hold && tt < alt && tt + dt >= alt) {
                                a.flags &= !af::PLAYING;
                                a.set_current_time(hold);
                            }
                        }
                    }
                } else {
                    if let Some(a) = c.clump.by_uid_mut(u) {
                        if !a.has(af::PLAYING) && a.blend > 0.0 && a.blend_delta >= 0.0 {
                            a.blend_delta = -4.0;
                        }
                    }
                    if self.next_cmd >= 11 {
                        self.anim = None;
                    }
                }
                if self.next_cmd == cmd::BLOCK {
                    self.next_cmd = 0;
                }
            } else if cs >= 4 {
                let Some(a) = c.clump.by_uid(u) else { return false };
                if a.blend > 0.9 && a.blend_delta >= 0.0 {
                    let combo = md.combo(cs).clone();
                    let mv = self.current_move.clamp(0, 4) as usize;
                    let (tt, dt) = (a.time, a.time_step());
                    let hit = combo.hit_time[mv];
                    // `t > hit && t - dt < hit` in SA; with the fixed 1/30 s step the anim clock
                    // lands exactly on melee.dat's whole frames, so the tie counts as a cross.
                    if tt >= hit && tt - dt < hit {
                        // Hit frame: presses before it are dropped.
                        if (11..=14).contains(&self.next_cmd) {
                            self.next_cmd = 0;
                        }
                        let lvl = combo.hit_level[mv];
                        if lvl != 7 {
                            let mut mat = c.p.matrix;
                            let mut pos = mat.transform(md.hit_offsets[lvl as usize]);
                            if mv == 4 {
                                pos += c.p.move_speed * ctx.ts;
                            }
                            mat.pos = pos;
                            let w = t.active_weapon().ty;
                            t.requests.push(WorldRequest::MeleeStrike(Strike {
                                owner: EntityId::Body(u32::MAX),
                                is_player: t.is_player,
                                mat,
                                radius: combo.hit_radius[mv],
                                weapon: w,
                                damage: strike_damage(t, combo.damage[mv]),
                                combo: cs,
                                mv: mv as i8,
                            }));
                        }
                    } else if tt >= combo.chain_time[mv] && (11..=14).contains(&self.next_cmd) {
                        match mv {
                            0 | 1 => self.chain(t, c, m),
                            3 => {
                                if t.is_player && self.choose_attack_move(t, c) == 3 {
                                    self.start_anim(t, c, m, 3);
                                }
                            }
                            4 => {
                                if cs == 12 && t.is_player {
                                    if let Some(a) = c.clump.by_uid_mut(u) {
                                        a.set_current_time(combo.hit_time[4] - 0.01);
                                    }
                                } else {
                                    self.chain(t, c, m);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        // Facing: the target, else the camera while the player aims with the mouse.
        if self.target.is_none() && t.is_player && t.pad.aim {
            *c.aim_rot = (-t.cam.front.x).atan2(t.cam.front.y);
        }
        self.in_control = false;
        false
    }

    fn chain(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager) {
        self.combo_set = self.get_combo_type(t, self.next_cmd);
        let mv = if t.is_player { self.choose_attack_move(t, c) } else { self.current_move as i32 + 1 };
        self.start_anim(t, c, m, mv);
    }
}

/// `CStats::GetFatAndMuscleModifier(3)` (attack anim speed), floored at 0.8.
fn fat_muscle_modifier_speed(t: &PedTasks) -> f32 {
    (1.0 + (t.stat_fat - 200.0) * -0.2 / 800.0 + (t.stat_muscle - 50.0) * -0.1 / 950.0).max(0.8)
}

/// `GetStrikeDamage` (0x61C740) for the player: × `GetFatAndMuscleModifier(4)` (cap 2.0).
fn strike_damage(t: &PedTasks, d: u8) -> f32 {
    if t.is_player {
        let k = (1.0 + (t.stat_muscle - 50.0) * 1.0 / 950.0 + (t.stat_fat - 200.0) * 0.5 / 800.0).min(2.0);
        d as f32 * k
    } else {
        d as f32
    }
}

impl PedTasks {
    /// `MeleeAttackJustDown(alt)` (0x540390): 1 fire, (alt) 2 sprint tap, 3 jump held,
    /// 4 enter tap.
    fn melee_attack_just_down(&self, alt: bool) -> u8 {
        if self.pad.fire_just_down {
            return 1;
        }
        if alt {
            if self.pad.sprint_just_down {
                return 2;
            }
            if self.pad.jump {
                return 3;
            }
            if self.pad.enter_exit_just_down {
                return 4;
            }
        }
        0
    }

    /// The melee branch of `ProcessPlayerWeapon` (0x685BDD..0x6861A2).
    pub(crate) fn player_melee(&mut self) {
        let attack_task = self.gun.is_some() || self.throw.is_some() || self.fight.is_some();
        let mut command = 0;
        if !self.pad.aim && !attack_task {
            if self.melee_attack_just_down(false) == 1 {
                command = cmd::ATTACK;
            }
        } else {
            let heavy = self.active_info().is_some_and(|i| i.has(wf::HEAVY));
            command = match self.melee_attack_just_down(self.pad.aim) {
                1 => cmd::ATTACK,
                4 => {
                    if heavy {
                        cmd::ATTACK
                    } else {
                        cmd::STYLE
                    }
                }
                3 => cmd::BLOCK,
                _ if self.pad.fire && self.active_weapon().ty == 9 && attack_task => cmd::ATTACK,
                _ => 0,
            };
        }
        if command != 0 {
            if !attack_task {
                self.fight = Some(FightTask::new(None, command, 2000));
            } else if let Some(f) = &mut self.fight {
                f.control_fight(None, command);
            }
            return;
        }
        if let Some(f) = &mut self.fight {
            if self.move_state == 1 && self.pad.sprint {
                f.control_fight(None, cmd::END_IDLE);
            } else if self.pd.chosen_slot != self.active_slot {
                f.control_fight(None, cmd::END);
            } else {
                f.control_fight(None, cmd::NONE);
            }
        }
    }

    /// `CTaskSimplePlayerOnFoot::PlayerControlFighter` (0x687530).
    pub(crate) fn player_control_fighter(&mut self, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) {
        let Some(mut f) = self.fight.take() else { return };
        let non_melee = self.active_info().is_some_and(|i| i.fire_type != fire::MELEE);
        let (lr, ud) = (self.pad.walk_lr, self.pad.walk_ud);
        let (mut x, mut y) = (lr / 128.0, ud / 128.0);
        let max_d = ctx.ts * 0.07;
        let approach = |v: &mut f32, to: f32| {
            if to - *v > max_d {
                *v += max_d;
            } else if to - *v < -max_d {
                *v -= max_d;
            } else {
                *v = to;
            }
        };
        let strafe = self.pad.aim; // mouse + target (no pad lock-on)
        enum Out {
            Shuffle,
            Leave,
            Done,
        }
        let out = if strafe {
            let mag = Vec2::new(x, y).length().min(1.0);
            if mag > 0.0 {
                let h = crate::ped::limit_radian_angle(radian_angle_between_points(0.0, 0.0, -x, y) - ctx.cam.orientation);
                let w = Vec3::new(-h.sin(), h.cos(), 0.0);
                x = w.dot(c.p.matrix.right) * mag;
                y = -w.dot(c.p.matrix.fwd) * mag;
            }
            approach(&mut self.pd.fight.y, y);
            approach(&mut self.pd.fight.x, x);
            if self.pad.sprint || non_melee { Out::Leave } else { Out::Shuffle }
        } else {
            let h = radian_angle_between_points(0.0, 0.0, -lr, ud);
            let mag = Vec2::new(x, y).length().min(1.0);
            if mag == 0.0 {
                self.pd.fight = Vec2::ZERO;
            } else if y <= 0.0 {
                approach(&mut self.pd.fight.y, -mag);
                self.pd.fight.x = 0.0;
                *c.aim_rot = crate::ped::limit_radian_angle(h - ctx.cam.orientation);
            }
            if y <= 0.0 && f.last_cmd < 11 {
                self.fighter_counter = self.fighter_counter.wrapping_add((ctx.ts * 0.02 * 1000.0) as i32 as u32);
            } else {
                self.fighter_counter = 0;
            }
            if !self.pad.sprint && !self.pad.duck && !non_melee && y <= 0.0 && self.fighter_counter < 2000 {
                Out::Shuffle
            } else {
                let command = if mag > 0.5 { cmd::END_WALK } else { cmd::END_IDLE };
                if self.pad.duck_just_down && self.can_ped_duck() {
                    self.set_task_duck(c.clump, m);
                    f.control_fight(None, cmd::END_CROUCH);
                } else {
                    f.control_fight(None, command);
                }
                Out::Done
            }
        };
        match out {
            Out::Leave => {
                let command = if self.pad.sprint {
                    cmd::END_RUN
                } else if y < -0.5 {
                    cmd::END_WALK
                } else {
                    cmd::END_IDLE
                };
                f.control_fight(None, command);
            }
            Out::Shuffle => {
                let v = self.pd.fight;
                if v.y.abs() > 0.0 && v.y.abs() > v.x.abs() {
                    f.control_fight(None, if v.y < 0.0 { cmd::SHUFFLE_F } else { cmd::SHUFFLE_B });
                } else if v.x.abs() > 0.0 {
                    f.control_fight(None, if v.x > 0.0 { cmd::SHUFFLE_R } else { cmd::SHUFFLE_L });
                }
            }
            Out::Done => {}
        }
        self.fight = Some(f);
    }

    /// Abort the fight task (death, knock-down).
    pub(crate) fn abort_fight(&mut self, clump: &mut Clump, m: &AnimManager) {
        if let Some(mut f) = self.fight.take() {
            f.make_abortable(self, clump, m, true);
        }
    }
}

/// `SetStrikeColModelRadius` (0x61D5F0): one sphere of radius r at the origin.
fn strike_col_model(r: f32) -> ColModel {
    ColModel {
        bbox_min: Vec3::splat(-r),
        bbox_max: Vec3::splat(r),
        bound_radius: r,
        spheres: vec![ColSphere { center: Vec3::ZERO, radius: r, surf: Surf { material: 0, piece: 0, lighting: 0 } }],
        ..Default::default()
    }
}

impl World {
    /// `FightStrike` (0x6240B0).
    pub(crate) fn melee_strike(&mut self, s: Strike) {
        let r = s.radius;
        let pos = s.mat.pos;
        let strike = strike_col_model(r);
        let mut hit_ped = None;
        let mut no_hit = true;
        let mut objects = 0;
        for id in self.body_ids() {
            if id == s.owner {
                continue;
            }
            let Some(b) = self.body(id) else { continue };
            let kind = b.phys.kind;
            let bound = b.col.bound_radius + r;
            let bc = b.phys.matrix.transform(b.col.bound_center);
            match kind {
                EntityType::Ped => {
                    let alive = b.logic.as_any().downcast_ref::<crate::ped::PedLogic>().is_some_and(|p| p.tasks.health.alive());
                    if !(b.phys.has_e(crate::physical::ef::USES_COLLISION) || !alive) {
                        continue;
                    }
                    if (bc.truncate() - pos.truncate()).length_squared() >= bound * bound {
                        continue;
                    }
                    // The skinned col model's spheres (the static ped spheres here).
                    let touched = b.col.spheres.iter().any(|sp| {
                        let c = b.phys.matrix.transform(sp.center);
                        (c - pos).length_squared() < (sp.radius + r) * (sp.radius + r)
                    });
                    if touched {
                        no_hit = false;
                        if self.fight_hit_ped(&s, id, pos) {
                            hit_ped = Some(id);
                        }
                    }
                }
                EntityType::Vehicle | EntityType::Object => {
                    let is_car = b.logic.as_any().is::<crate::automobile::Automobile>();
                    if kind == EntityType::Vehicle && !is_car {
                        continue;
                    }
                    if kind == EntityType::Object {
                        // Only the player scans objects: FindObjectsInRange(hitPos, 5.0, 2D, 16).
                        if !s.is_player || objects >= 16 {
                            continue;
                        }
                        if (b.phys.matrix.pos.truncate() - pos.truncate()).length() >= 5.0 {
                            continue;
                        }
                        objects += 1;
                        if !b.phys.has_e(crate::physical::ef::USES_COLLISION) {
                            continue;
                        }
                    }
                    if (bc - pos).length_squared() >= bound * bound {
                        continue;
                    }
                    let mut cps = [ColPoint::default(); MAX_COLPOINTS];
                    let n = process_col_models(&s.mat, &strike, &b.phys.matrix, &b.col, &mut cps, &mut [], &mut [], false);
                    if n > 0 {
                        let cp = cps[0];
                        if kind == EntityType::Vehicle {
                            self.fight_hit_car(&s, id, cp.point, cp.normal, cp.piece_b);
                        } else {
                            self.fight_hit_obj(&s, id, cp.point, cp.normal);
                        }
                    }
                }
                _ => {}
            }
        }
        let _ = no_hit; // CEventSoundQuiet(40.0) for a miss: no AI hearing yet.
        // Street style's 2nd move aborts on a miss.
        if s.combo == 7 && s.mv == 1 && hit_ped.is_none() {
            if let Some(b) = self.body_mut(s.owner) {
                if let Some(p) = b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>() {
                    if let (Some(f), Some(clump)) = (&mut p.tasks.fight, p.clump.as_deref_mut()) {
                        if let Some(a) = f.anim.and_then(|u| clump.by_uid_mut(u)) {
                            a.blend_delta = -4.0;
                            a.flags &= !af::PLAYING;
                            a.flags |= af::DELETE_BLENDED_OUT;
                        }
                    }
                }
            }
        }
    }

    /// `FightHitPed` (0x61CBA0) without blocking victims and hit anims: damage and blood.
    fn fight_hit_ped(&mut self, s: &Strike, victim: EntityId, pos: Vec3) -> bool {
        let attacker_pos = self.body(s.owner).map_or(pos, |b| b.phys.matrix.pos);
        let Some(b) = self.body_mut(victim) else { return false };
        let vpos = b.phys.matrix.pos;
        let Some(p) = b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>() else { return false };
        let to = attacker_pos - vpos;
        let dir = crate::peddamage::local_direction(p.cur_rot, Vec2::new(to.x, to.y));
        let health = p.tasks.health.health;
        let alive = p.tasks.health.alive();
        let lighting = p.lighting;
        p.pending_damage.push(crate::peddamage::DamageIn {
            src: Some(s.owner),
            src_pos: Some(attacker_pos),
            ty: s.weapon,
            damage: s.damage as i32 as f32,
            piece: 3,
            dir,
        });
        let thr = if (8..=12).contains(&s.combo) {
            100
        } else if s.combo == 4 && s.mv == 4 {
            -1
        } else {
            (100.0 - health) as i32
        };
        let roll = ((self.rng.next() & 0xFFFF) as f32 * (1.0 / 32768.0) * 100.0) as i32;
        if roll < thr {
            let mut bdir = (attacker_pos - vpos).normalize_or_zero();
            if !alive {
                bdir = Vec3::new(0.0, 0.0, 2.0);
            }
            let mut n = 8;
            if (8..=12).contains(&s.combo) {
                n = 16;
                if alive {
                    bdir *= 1.5;
                }
            }
            self.weapon_fx(|f| f.add_blood(pos, bdir, n, lighting));
        }
        true
    }

    /// `FightHitCar` (0x61D0B0).
    fn fight_hit_car(&mut self, s: &Strike, id: EntityId, pos: Vec3, normal: Vec3, piece: u8) {
        let ts = self.last_ts;
        let chainsaw = s.weapon == 9;
        if chainsaw {
            let at = s.mat.fwd;
            self.weapon_fx(|f| f.add_sparks(pos, at, 5.0, 32, Vec3::ZERO, true, 0.3, 1.0));
        }
        if let Some(b) = self.body_mut(id) {
            let crate::world::Body { phys, logic, .. } = b;
            if let Some(car) = logic.as_any_mut().downcast_mut::<crate::automobile::Automobile>() {
                // VehicleDamage(mass · dmg · k, piece, ped, pos, normal, weapon).
                let k = if chainsaw { 0.000_75 } else { 0.01 };
                let saved = (phys.damage_intensity, phys.damage_piece, phys.last_collision_pos, phys.last_collision_impact_velocity, phys.damage_entity_kind);
                phys.damage_intensity = phys.mass * s.damage * k;
                phys.damage_piece = piece;
                phys.last_collision_pos = pos;
                phys.last_collision_impact_velocity = normal;
                phys.damage_entity_kind = Some(EntityType::Ped);
                let is_player = phys.status == Status::Player;
                car.damage.vehicle_damage(phys, ts, is_player);
                (phys.damage_intensity, phys.damage_piece, phys.last_collision_pos, phys.last_collision_impact_velocity, phys.damage_entity_kind) = saved;
            }
        }
        self.weapon_fx(|f| f.add_punch_impact(pos, normal));
    }

    /// `FightHitObj` (0x61D400) without ObjectDamage.
    fn fight_hit_obj(&mut self, _s: &Strike, id: EntityId, pos: Vec3, normal: Vec3) {
        let uproot = self.body(id).and_then(|b| b.logic.uproot_limit());
        if let Some(b) = self.body_mut(id) {
            if b.phys.is_static() && uproot.is_some_and(|u| u <= 0.0) {
                b.phys.eflags &= !crate::physical::ef::IS_STATIC;
            }
            if !b.phys.is_static() {
                let k = if b.phys.flags & 0x80 != 0 { -0.1 } else { -0.5 };
                let rel = pos - b.phys.matrix.pos;
                b.phys.apply_force(normal * k, rel, true);
            }
        }
        self.weapon_fx(|f| f.add_punch_impact(pos, normal));
    }
}

/// Shared handle type for the tasks.
pub type MeleeRef = Arc<MeleeData>;
