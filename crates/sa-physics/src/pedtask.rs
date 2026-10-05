//! The player's on-foot tasks: `CTaskSimplePlayerOnFoot` (PlayerControlZelda /
//! PlayerControlZeldaWeapon / ProcessPlayerWeapon), `CPlayerPed::SetRealMoveAnim`, the jump /
//! in-air / land task tree, `ProcessAnimGroups` / `ReApplyMoveAnims`, weapon switching and
//! `CPed::GiveWeapon` / `SetCurrentWeapon`. The gun task itself is in `gun.rs`.
//!
//! Not ported: ducking, melee and fighting, thrown / first-person / camera weapons, lock-on
//! targeting, turning on the spot, the adrenaline pill, fat / muscle groups, climbing, the
//! head-room / direction checks of the jump launch, and fall damage tasks (CTaskSimpleFall
//! is approximated by its knock-down anim).

use std::sync::Arc;

use glam::{Vec2, Vec3};

use crate::{
    Ctx,
    effects::WorldRequest,
    world::EntityId,
    anim::{AnimManager, Clump, af, anim_id, group},
    gun::{GunCmd, UseGun},
    physical::Physical,
    weapon::{Weapon, WeaponInfo, WeaponInfos, fire, wf, ws, wt},
};

/// The pad as the on-foot code reads it (`CPad`). Sticks are ±128; `walk_ud > 0` is
/// backwards. `*_just_down` flags are cleared after each physics step.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pad {
    pub walk_lr: f32,
    pub walk_ud: f32,
    /// The walk key (`pad+0x2A`): clamps the move blend ratio to 1.
    pub walk_key: bool,
    pub sprint: bool,
    pub sprint_just_down: bool,
    pub jump_just_down: bool,
    /// Jump held (`pad+0x1C` Square: the melee block).
    pub jump: bool,
    /// `GetDuck` (held).
    pub duck: bool,
    /// `GetTarget` (aim, RMB on PC).
    pub aim: bool,
    /// `GetWeapon` (fire, LMB on PC).
    pub fire: bool,
    pub fire_just_down: bool,
    pub next_weapon_just_down: bool,
    pub prev_weapon_just_down: bool,
    /// `DuckJustDown` (C on PC).
    pub duck_just_down: bool,
    /// `ExitVehicleJustDown` (enter / exit key).
    pub enter_exit_just_down: bool,
}

impl Pad {
    pub fn clear_just_down(&mut self) {
        self.sprint_just_down = false;
        self.jump_just_down = false;
        self.fire_just_down = false;
        self.next_weapon_just_down = false;
        self.prev_weapon_just_down = false;
        self.duck_just_down = false;
        self.enter_exit_just_down = false;
    }
}

/// `CPlayerData` fields the tasks use.
#[derive(Debug, Clone, Copy)]
pub struct PlayerData {
    /// +0x14 move blend ratio.
    pub mbr: f32,
    /// +0x18 m_fTimeCanRun (sprint stamina).
    pub time_can_run: f32,
    /// +0x1C sprint tap counter.
    pub sprint_counter: f32,
    /// +0x20 chosen weapon slot.
    pub chosen_slot: usize,
    /// +0x2C attack button counter (spread / crosshair size).
    pub attack_counter: f32,
    /// +0x54 look pitch for the torso IK (rad).
    pub look_pitch: f32,
    /// +0x44 breath.
    pub breath: f32,
    /// +0x34 & 8: free aiming.
    pub free_aim: bool,
    /// +0x0C / +0x10: the fight shuffle stick (x right, y back).
    pub fight: Vec2,
}

impl Default for PlayerData {
    fn default() -> Self {
        Self {
            mbr: 0.0,
            // [I] CPlayerData's initial stamina is stat driven; a full bar here.
            time_can_run: 50.0,
            sprint_counter: 0.0,
            chosen_slot: 0,
            attack_counter: 0.0,
            look_pitch: 0.0,
            breath: crate::ped::BREATH_MAX,
            free_aim: false,
            fight: Vec2::ZERO,
        }
    }
}

/// The jump / in-air / land task tree (`CTaskComplexJump`, `CTaskComplexInAirAndLand`).
#[derive(Debug, Clone, Copy, Default)]
pub enum AirTask {
    #[default]
    None,
    /// `CTaskSimpleJump`: waiting for the launch anim.
    Jump { launch: Option<u32>, launch_done: bool },
    /// `CTaskSimpleInAir`.
    InAir { jump_glide: bool, fall_glide: bool, anim: Option<u32>, anim_id: i16, min_vz: f32, assist_ms: f32 },
    /// `CTaskSimpleLand` (anim -1 = none).
    Land { anim: Option<u32>, anim_id: i16, first: bool, finished: bool },
}

/// `CTaskSimpleThrowProjectile` (0x61F660), player version (no target).
#[derive(Debug, Clone, Default)]
pub struct ThrowTask {
    pub finished: bool,
    start_done: bool,
    released: bool,
    anim: Option<u32>,
    /// Start time; after the release or the throw, the hold duration in ms.
    time: u32,
}

impl ThrowTask {
    pub fn new(now: u32) -> Self {
        Self { time: now, ..Default::default() }
    }

    /// `ControlThrow(release, ...)` (0x61F810).
    pub fn control_throw(&mut self, release: bool, now: u32) {
        if self.finished {
            return;
        }
        if release && !self.released {
            self.time = now.wrapping_sub(self.time);
            self.released = true;
        }
    }

    /// `ProcessPed` (0x62AF50) with StartAnim (0x6259E0). Returns true when done.
    fn process(&mut self, t: &mut PedTasks, c: &mut PedCore, m: &AnimManager, now: u32) -> bool {
        // FinishAnimThrowCB (0x61F890).
        if let Some(u) = self.anim {
            if c.clump.finished.contains(&u) || c.clump.by_uid(u).is_none() {
                self.anim = None;
                if !self.start_done {
                    self.start_done = true;
                } else {
                    self.finished = true;
                }
            }
        }
        let w = *t.active_weapon();
        let Some(info) = t.info_of(w.ty).cloned() else { return true };
        if self.finished || !info.has(wf::THROW) {
            return true;
        }
        let Some(u) = self.anim else {
            let (id, delta) = if !self.start_done {
                (228, 16.0)
            } else {
                let over_arm = !self.released && w.ty != wt::SATCHEL_CHARGE;
                (if over_arm { 230 } else { 229 }, 1000.0)
            };
            self.anim = c.clump.blend_animation(m, info.anim_group, id, delta).map(|i| {
                c.clump.assocs[i].finish_cb = true;
                c.clump.assocs[i].uid
            });
            if self.anim.is_none() {
                return true;
            }
            return false;
        };
        let Some(a) = c.clump.by_uid(u) else { return false };
        if a.id != 229 && a.id != 230 {
            return false;
        }
        let f = if a.id == 230 { info.anim2_loop_fire } else { info.anim_loop_fire };
        let (tt, dt) = (a.time, a.time_step());
        if !(f < tt) || !(tt - dt <= f) || !a.has(af::PLAYING) {
            return false;
        }
        if !self.released {
            self.time = now.wrapping_sub(self.time);
        }
        let hold = self.time.min(533);
        t.pd.attack_counter = hold as f32 * 0.05;
        // The hand: right hand bone · fireOffset ((0,0,0) for the thrown weapons).
        let Some(hand) = c.clump.frame_of_tag(crate::gun::bone::R_HAND) else { return false };
        let pos = c.p.matrix.transform(c.clump.ltm(hand).transform_point3(info.fire_offset));
        let slot = t.active_slot;
        if t.weapons[slot].can_fire(info.ammo_clip) {
            let force = t.pd.attack_counter * 0.0375;
            t.requests.push(WorldRequest::FireProjectile { owner: EntityId::Body(u32::MAX), ty: w.ty, effect: pos, force, cam: None });
            let reload = t.infos.as_deref().map_or(1000, |i| i.reload_time(&info));
            t.weapons[slot].after_shot(now, &info, reload, true, true);
            if w.ty == wt::SATCHEL_CHARGE {
                // Fire(39): give the detonator; switch to it when no satchels are left.
                let left = t.weapons[slot].total_ammo;
                let ds = t.give_weapon(wt::DETONATOR, 1);
                if left <= 1 {
                    t.weapons[ds].state = crate::weapon::ws::READY;
                    t.set_current_weapon(ds);
                }
            }
        }
        false
    }
}

/// `CPedIK` (ped+0x50C): the torso angles.
#[derive(Debug, Clone, Copy, Default)]
pub struct PedIk {
    pub torso_yaw: f32,
    pub torso_pitch: f32,
}

/// Player ped task state (the CPlayerPed / CPed fields beyond the physics).
#[derive(Debug, Clone)]
pub struct PedTasks {
    pub pad: Pad,
    pub pd: PlayerData,
    /// ped+0x534.
    pub move_state: u8,
    /// ped+0x4D4: walk / run / sprint / idle group.
    pub anim_group: usize,
    pub weapons: [Weapon; 13],
    /// ped+0x718.
    pub active_slot: usize,
    /// ped+0x71A (100 for the player).
    pub accuracy: u8,
    pub gun: Option<UseGun>,
    /// `CTaskSimpleThrowProjectile` (also secondary slot 0).
    pub throw: Option<ThrowTask>,
    /// World requests of this step (shots from the throw task, the detonator); the ped's
    /// owner id is filled in when they are sent.
    pub requests: Vec<crate::effects::WorldRequest>,
    /// TheCamera as of this step.
    pub cam: crate::CamInfo,
    /// Health, armour, knock-downs and death.
    pub health: crate::peddamage::Health,
    /// `CTaskSimpleDuck` in secondary slot 1.
    pub duck: Option<crate::duck::DuckTask>,
    /// ped+0x46C & 0x4000000 bIsDucking.
    pub ducking: bool,
    pub air: AirTask,
    pub ik: PedIk,
    /// `CTaskSimpleIKManager` (look-at and arm chains).
    pub ikm: crate::ik::IkManager,
    /// CPedIK flag 0x10 (torso-aim mode) vs 0x4 (arm mode); None before the first AimGun.
    pub torso_ik_mode: Option<bool>,
    /// `CTimer::m_snTimeInMilliseconds` of the current step.
    pub now_ms: u32,
    /// Weapon model in hand (ped+0x740), -1 none.
    pub weapon_model: i32,
    /// Gun flash counters (ped+0x504.. right / left: alpha, rate).
    pub gun_flash: [(i16, i16); 2],
    /// Accumulated random roll of the gunflash frame (degrees).
    pub gun_flash_roll: f32,
    /// Camera mode requested this step (SetNewPlayerWeaponMode), 0 = none.
    pub cam_request: u8,
    /// Weapon skill stats 69..79 (pistol .. m4; tec9 shares the micro uzi stat).
    pub skill_stats: [f32; 10],
    /// ped+0x46C & 0x200 / 0x400.
    pub in_the_air: bool,
    pub landing: bool,
    /// Heading rate saved while a run-stop plays.
    saved_turn_rate: Option<f32>,
    pub infos: Option<Arc<WeaponInfos>>,
    pub anims: Option<Arc<AnimManager>>,
    pub is_player: bool,
    /// melee.dat.
    pub melee: Option<Arc<crate::melee::MeleeData>>,
    /// `CTaskSimpleFight` (secondary slot 0, like the gun and throw tasks).
    pub fight: Option<crate::melee::FightTask>,
    /// ped+0x72D fighting style (combo type) and +0x72E learned-move mask.
    pub fight_style: i8,
    pub fight_moves: u8,
    /// `CTaskSimplePlayerOnFoot+0x10`: ms in the fight stance without attacking.
    pub fighter_counter: u32,
    /// Stats FAT and MUSCLE (GetFatAndMuscleModifier).
    pub stat_fat: f32,
    pub stat_muscle: f32,
}

impl Default for PedTasks {
    fn default() -> Self {
        Self {
            pad: Pad::default(),
            pd: PlayerData::default(),
            move_state: 1,
            anim_group: group::PLAYER,
            weapons: [Weapon::default(); 13],
            active_slot: 0,
            accuracy: 100,
            gun: None,
            throw: None,
            requests: Vec::new(),
            cam: crate::CamInfo::default(),
            health: Default::default(),
            duck: None,
            ducking: false,
            air: AirTask::None,
            ik: PedIk::default(),
            ikm: Default::default(),
            torso_ik_mode: None,
            now_ms: 0,
            weapon_model: -1,
            gun_flash: [(0, 0); 2],
            gun_flash_roll: 0.0,
            cam_request: 0,
            skill_stats: [0.0; 10],
            in_the_air: false,
            landing: false,
            saved_turn_rate: None,
            infos: None,
            anims: None,
            is_player: false,
            melee: None,
            fight: None,
            fight_style: 4,
            fight_moves: 0,
            fighter_counter: 0,
            stat_fat: 200.0,
            stat_muscle: 50.0,
        }
    }
}

/// What the tasks may touch on the ped besides their own state.
pub struct PedCore<'a> {
    pub p: &'a mut Physical,
    pub clump: &'a mut Clump,
    pub cur_rot: &'a mut f32,
    pub aim_rot: &'a mut f32,
    pub turn_rate: &'a mut f32,
    pub standing: bool,
    pub ground_below: Option<f32>,
    pub ground_entity: bool,
    /// Standing on a car (CanStrikeTargetOnGround).
    pub ground_car: bool,
}

/// `CGeneral::GetRadianAngleBetweenPoints` (0x53CBE0).
pub fn radian_angle_between_points(x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let dx = x2 - x1;
    let mut dy = y2 - y1;
    if dy == 0.0 {
        dy = 1e-4;
    }
    let a = (dx / dy).atan();
    if dy > 0.0 {
        if dx > 0.0 { std::f32::consts::PI - a } else { -std::f32::consts::PI - a }
    } else {
        -a
    }
}

impl PedTasks {
    pub fn info_of(&self, ty: u32) -> Option<&WeaponInfo> {
        let infos = self.infos.as_deref()?;
        Some(infos.get(ty, self.weapon_skill(ty)))
    }

    /// `CPed::GetWeaponSkill(type)` for the player.
    pub fn weapon_skill(&self, ty: u32) -> u8 {
        let Some(infos) = self.infos.as_deref() else { return 1 };
        if !(22..=32).contains(&ty) {
            return 1;
        }
        let stat = self.skill_stats[if ty == wt::TEC9 { 6 } else { (ty - 22) as usize }];
        crate::weapon::player_weapon_skill(infos, ty, stat)
    }

    pub fn active_weapon(&self) -> &Weapon {
        &self.weapons[self.active_slot]
    }

    pub fn active_info(&self) -> Option<WeaponInfo> {
        self.info_of(self.active_weapon().ty).cloned()
    }

    /// `CPed::GiveWeapon` (0x5E6080); returns the slot.
    pub fn give_weapon(&mut self, ty: u32, ammo: u32) -> usize {
        let Some(infos) = self.infos.clone() else { return 0 };
        let slot = infos.get(ty, 1).slot.max(0) as usize;
        let clip = infos.get(ty, self.weapon_skill(ty)).ammo_clip;
        let w = &mut self.weapons[slot];
        if w.ty == ty && ty != 0 {
            if slot == 10 {
                return slot;
            }
            w.total_ammo = (w.total_ammo + ammo).min(99_999);
            w.reload(clip);
            if w.state == ws::OUT_OF_AMMO && w.total_ammo > 0 {
                w.state = ws::READY;
            }
        } else {
            let mut ammo = ammo;
            if w.ty != 0 && matches!(slot, 3..=5) {
                ammo += w.total_ammo;
            }
            *w = Weapon::initialise(ty, ammo, clip);
            if slot == self.active_slot {
                self.weapon_model = infos.get(ty, 1).model1;
            }
        }
        if self.weapons[slot].state != ws::OUT_OF_AMMO {
            self.weapons[slot].state = ws::READY;
        }
        slot
    }

    /// `CPed::SetCurrentWeapon(slot)` (0x5E61F0).
    pub fn set_current_weapon(&mut self, slot: usize) {
        self.active_slot = slot;
        self.pd.chosen_slot = slot;
        let ty = self.weapons[slot].ty;
        self.weapon_model = if ty != 0 { self.infos.as_deref().map_or(-1, |i| i.get(ty, 1).model1) } else { -1 };
    }

    /// `CPlayerPed::MakeChangesForNewWeapon` (0x60B460) and `RemoveWeaponAnims` (0x5F0250).
    fn change_weapon(&mut self, clump: &mut Clump, slot: usize) {
        // RemoveWeaponAnims(active, -1000): the original only ever looks up anim 0xE0.
        let mut need_idle = false;
        if let Some(a) = clump.get_mut(anim_id::WEAPON_FIRE) {
            a.flags |= af::DELETE_BLENDED_OUT;
            if a.has(af::PARTIAL) {
                a.blend_delta = -1000.0;
            } else {
                need_idle = true;
            }
        }
        if need_idle {
            if let Some(m) = self.anims.clone() {
                clump.blend_animation(&m, self.anim_group, anim_id::IDLE, 1000.0);
            }
        }
        self.set_current_weapon(slot);
        self.pd.attack_counter = 0.0;
        let w = self.weapons[slot];
        if let Some(info) = self.info_of(w.ty).cloned() {
            self.weapons[slot].ammo_in_clip = w.total_ammo.min(info.ammo_clip as u32);
            if !info.has(wf::ONLYFREEAIM) {
                self.pd.free_aim = false;
            }
        }
        if let Some(a) = clump.get_mut(anim_id::WEAPON_FIRE) {
            a.flags |= af::PLAYING | af::FADE_OUT_FINISHED;
        }
    }

    /// `CPlayerPed::ProcessWeaponSwitch` (0x60D850).
    fn process_weapon_switch(&mut self, clump: &mut Clump) {
        let active = self.active_slot;
        let mut chosen = self.pd.chosen_slot;
        if !self.pd.free_aim {
            if self.pad.next_weapon_just_down {
                chosen = active + 1;
                loop {
                    if chosen >= 13 {
                        chosen = 0;
                        break;
                    }
                    let w = &self.weapons[chosen];
                    if w.ty != 0 && w.has_ammo_to_be_used() {
                        break;
                    }
                    chosen += 1;
                }
            } else if self.pad.prev_weapon_just_down {
                let mut c = active as i32 - 1;
                loop {
                    if c < 0 {
                        c = 12;
                    }
                    let w = &self.weapons[c as usize];
                    if c == 0 || (w.ty != 0 && w.has_ammo_to_be_used()) {
                        break;
                    }
                    c -= 1;
                }
                chosen = c as usize;
            }
        }
        // Auto switch away from an empty gun.
        let w = self.weapons[active];
        let melee = self.infos.as_deref().is_none_or(|i| i.get(w.ty, 1).fire_type == fire::MELEE);
        if !melee && (!self.pad.fire || w.ty != wt::MINIGUN) && w.total_ammo < 1 {
            let mut c = active as i32 - 1;
            while c >= 0 {
                if self.weapons[c as usize].total_ammo > 0 {
                    break;
                }
                c -= 1;
            }
            chosen = c.max(0) as usize;
        }
        self.pd.chosen_slot = chosen;
        if chosen != active {
            if let Some(g) = &self.gun {
                let busy = matches!(g.last_cmd, GunCmd::Fire | GunCmd::FireBurst)
                    || (g.last_cmd == GunCmd::Reload && g.anim.is_some());
                if busy {
                    return;
                }
            }
            self.change_weapon(clump, chosen);
        }
    }

    /// `CPlayerPed::ProcessAnimGroups` (0x6098F0) + `ReApplyMoveAnims` (0x609650).
    fn process_anim_groups(&mut self, clump: &mut Clump) {
        let w = if self.weapon_model >= 0 { self.active_weapon().ty } else { 0 };
        let new = match w {
            wt::RLAUNCHER | wt::RLAUNCHER_HS => group::PLAYER_ROCKET,
            wt::BASEBALLBAT | wt::SHOVEL | wt::POOLCUE => group::PLAYER_BBBAT,
            wt::CHAINSAW | wt::FTHROWER | wt::MINIGUN => group::PLAYER_CSAW,
            wt::SHOTGUN | wt::SPAS12 | wt::AK47 | wt::M4 | wt::COUNTRYRIFLE | wt::SNIPERRIFLE => group::PLAYER_2ARMED,
            _ => group::PLAYER,
        };
        if new == self.anim_group {
            return;
        }
        self.anim_group = new;
        let Some(m) = self.anims.clone() else { return };
        for id in [0i16, 1, 2, 3, 5] {
            let Some(i) = clump.index_of(id) else { continue };
            let Some((h, _)) = m.get(new, id) else { continue };
            if Arc::ptr_eq(&clump.assocs[i].hier, h) || clump.assocs[i].hier.name.eq_ignore_ascii_case(&h.name) {
                continue;
            }
            let (blend, delta) = (clump.assocs[i].blend, clump.assocs[i].blend_delta);
            let uid = clump.assocs[i].uid;
            if let Some(n) = clump.add_animation(&m, new, id) {
                clump.assocs[n].blend = blend;
                clump.assocs[n].blend_delta = delta;
            }
            if let Some(a) = clump.by_uid_mut(uid) {
                a.flags |= af::DELETE_BLENDED_OUT;
                a.blend_delta = -1000.0;
            }
        }
    }

    // -------------------------------------------------------------- the step

    /// CPedIntelligence::Process for the player (step 11 of CPed::ProcessControl): the jump
    /// tree or PlayerOnFoot, then the gun task.
    pub fn process(&mut self, c: &mut PedCore, ctx: &Ctx) {
        let Some(m) = self.anims.clone() else { return };
        self.cam_request = 0;
        self.now_ms = ctx.now_ms;
        self.cam = ctx.cam;
        if !matches!(self.air, AirTask::None) {
            self.process_air(c, ctx, &m);
        } else {
            // CEventInAir: walking off a ledge.
            let in_air = !c.standing && c.ground_below.is_none_or(|z| c.p.matrix.pos.z - z > 1.5);
            if in_air && !c.ground_entity {
                self.air = AirTask::InAir {
                    jump_glide: false,
                    fall_glide: false,
                    anim: None,
                    anim_id: -1,
                    min_vz: 0.0,
                    assist_ms: 0.0,
                };
                self.process_air(c, ctx, &m);
            } else {
                self.player_on_foot(c, ctx, &m);
            }
        }
        // Secondary tasks: slot 0 the use-gun or throw task, slot 1 the duck.
        if let Some(mut th) = self.throw.take() {
            let done = th.process(self, c, &m, ctx.now_ms);
            if !done {
                self.throw = Some(th);
            }
        }
        if let Some(mut g) = self.gun.take() {
            let done = g.process_ped(self, c, ctx, &m);
            if !done {
                self.gun = Some(g);
            }
        }
        if let Some(mut f) = self.fight.take() {
            let done = f.process_ped(self, c, ctx, &m);
            if !done {
                self.fight = Some(f);
            }
        }
        if let Some(mut d) = self.duck.take() {
            let done = d.process_ped(self, c.clump, &m);
            if !done {
                self.duck = Some(d);
            }
        }
        // Slot 5: the IK manager's tasks (blend ramps, chain creation).
        self.ikm.process(c.clump, ctx.now_ms as i64, ctx.ts);
    }

    /// The CPlayerPed::ProcessControl tail: CWeapon::Update, ProcessWeaponSwitch,
    /// ProcessAnimGroups.
    pub fn post_process(&mut self, clump: &mut Clump, ctx: &Ctx) {
        let w = self.weapons[self.active_slot];
        if let Some(info) = self.info_of(w.ty).cloned() {
            let reload = clump
                .get(anim_id::WEAPON_RELOAD)
                .map(|a| (a.time, a.hier.total_length.max(1e-6)));
            let gun = self.gun.is_some();
            self.weapons[self.active_slot].update(ctx.now_ms, &info, info.ammo_clip, reload, gun);
        }
        if matches!(self.air, AirTask::None) {
            self.process_weapon_switch(clump);
        }
        self.process_anim_groups(clump);
        // Spread counter decay (pd+0x2C).
        self.pd.attack_counter *= 0.96f32.powf(ctx.ts);
    }

    /// `CTaskSimplePlayerOnFoot::ProcessPed` (0x688810).
    fn player_on_foot(&mut self, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) {
        // Last frame's move state: once walking, the normal control stays in charge and the
        // fight task plays the moving attack on top.
        let mut moving_attack = self.move_state >= 4;
        if self.active_weapon().ty == 9 && self.fight.as_ref().is_some_and(|f| f.current_move == 4) {
            moving_attack = true;
        }
        self.move_state = 1;
        let zelda_weapon = self.gun.as_ref().and_then(|g| g.info.clone()).is_some_and(|i| !i.has(wf::AIMWITHARM));
        if self.ducking {
            self.player_control_ducked(c, ctx, m);
        } else if self.fight.is_some() && !moving_attack {
            self.player_control_fighter(c, ctx, m);
        } else if zelda_weapon {
            self.player_control_zelda_weapon(c, ctx.ts);
            if self.pad.duck_just_down && self.can_ped_duck() {
                self.set_task_duck(c.clump, m);
            }
        } else {
            self.player_control_zelda(c, ctx, m);
        }
        crate::gun::process_player_weapon(self, c, ctx);
    }

    /// `PlayerControlZeldaWeapon` (0x687C20): the strafe command for two-handed aiming.
    fn player_control_zelda_weapon(&mut self, c: &mut PedCore, ts: f32) {
        let mut v = Vec2::new(self.pad.walk_lr, self.pad.walk_ud) * (1.0 / 128.0);
        if v.length() > 1.0 {
            v = v.normalize();
        }
        if let Some(g) = &mut self.gun {
            g.control_gun_move(v, ts);
        }
        if self.pad.jump_just_down && c.standing {
            self.air = AirTask::Jump { launch: None, launch_done: false };
        }
    }

    /// `PlayerControlZelda` (0x6883D0), the movement part.
    fn player_control_zelda(&mut self, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) {
        let (lr, ud) = (self.pad.walk_lr, self.pad.walk_ud);
        let mut mag = (ud * ud + lr * lr).sqrt() * (1.0 / 60.0);
        if self.pad.walk_key && mag > 1.0 {
            mag = 1.0;
        }
        if mag > 0.0 {
            let a = radian_angle_between_points(0.0, 0.0, -lr, ud) - ctx.cam.orientation;
            *c.aim_rot = crate::ped::limit_radian_angle(a);
            let step = ctx.ts * 0.07;
            let mbr = &mut self.pd.mbr;
            if mag - *mbr > step {
                *mbr += step;
            } else if mag - *mbr < -step {
                *mbr -= step;
            } else {
                *mbr = mag;
            }
        } else {
            self.pd.mbr = 0.0;
        }
        // Sprint / run.
        let heavy = self.active_info().is_some_and(|i| i.has(wf::HEAVY));
        if !heavy {
            let own_sprint = match (m.get(self.anim_group, 1), m.get(self.anim_group, 2)) {
                (Some(a), Some(b)) => !Arc::ptr_eq(&a.0, &b.0),
                _ => false,
            };
            if own_sprint {
                if self.control_button_sprint(ctx.ts) >= 1.0 {
                    self.move_state = 7;
                }
            } else if self.pad.sprint {
                self.move_state = 6;
            }
        }
        self.set_real_move_anim(c, ctx, m);
        if self.pad.duck_just_down && self.can_ped_duck() {
            self.set_task_duck(c.clump, m);
        }
        // Jump (0x6886C5): not while aiming, not with heavy weapons.
        if !self.in_the_air && !heavy && self.pad.jump_just_down && !self.pad.aim && c.standing {
            self.air = AirTask::Jump { launch: None, launch_done: false };
        }
    }

    /// `ControlButtonSprint(0)` (0x60A610) with HandleSprintEnergy.
    fn control_button_sprint(&mut self, ts: f32) -> f32 {
        let pd = &mut self.pd;
        let can = pd.sprint_counter > 0.0 || pd.time_can_run > 0.0;
        if self.pad.sprint_just_down && can {
            pd.sprint_counter = (pd.sprint_counter + 4.0).min(10.0);
        } else if self.pad.sprint && can {
            pd.sprint_counter = (pd.sprint_counter - ts * 0.7).max(1.0);
        } else if pd.sprint_counter > 0.0 {
            pd.sprint_counter = (pd.sprint_counter - ts * 0.2).max(0.0);
        }
        let (r, rate) = if pd.sprint_counter > 5.0 {
            (pd.sprint_counter / 5.0, 0.5)
        } else if pd.sprint_counter > 0.0 && can {
            (1.0, 1.0)
        } else {
            return 0.0;
        };
        if pd.time_can_run <= -150.0 {
            pd.sprint_counter = 0.0;
            return 0.0;
        }
        pd.time_can_run = (pd.time_can_run - ts * rate).max(-150.0);
        (r - 1.0f32).max(0.0) * 0.3 + 1.0
    }

    /// `GetButtonSprintResults` (0x60A820).
    fn button_sprint_results(&self) -> f32 {
        let c = self.pd.sprint_counter;
        if c > 5.0 {
            (c / 5.0 - 1.0).max(0.0) * 0.3 + 1.0
        } else if c > 0.0 {
            1.0
        } else {
            0.0
        }
    }

    /// `CPlayerPed::SetRealMoveAnim` (0x60A9C0).
    fn set_real_move_anim(&mut self, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) {
        let grp = self.anim_group;
        let clump = &mut *c.clump;
        let uid = |cl: &Clump, id: i16| cl.get(id).map(|a| a.uid);
        let mut walk = uid(clump, 0);
        let mut run = uid(clump, 1);
        let mut sprint = uid(clump, 2);
        let mut wstart = uid(clump, 5);
        let idle = uid(clump, 3);
        let stop = uid(clump, 6);
        let stop_r = uid(clump, 7);
        let tired = uid(clump, 10);
        let mbr = self.pd.mbr;

        // A. a run-stop is playing.
        let playing = |cl: &Clump, u: Option<u32>| u.and_then(|u| cl.by_uid(u)).is_some_and(|a| a.has(af::PLAYING));
        if playing(clump, stop) || playing(clump, stop_r) {
            self.move_state = 6;
            return self.move_anim_tail(clump, sprint, ctx);
        }
        // B. a run-stop has finished and is not fading yet.
        let not_fading = |cl: &Clump, u: Option<u32>| u.and_then(|u| cl.by_uid(u)).is_some_and(|a| a.blend_delta >= 0.0);
        if not_fading(clump, stop) || not_fading(clump, stop_r) {
            let s = stop.or(stop_r).unwrap();
            if let Some(a) = clump.by_uid_mut(s) {
                a.flags |= af::DELETE_BLENDED_OUT;
                a.blend = 1.0;
                a.blend_delta = -8.0;
            }
            self.restore_heading_rate(c.turn_rate);
            let i = match clump.index_of(anim_id::IDLE) {
                Some(i) => Some(i),
                None => clump.blend_animation(m, grp, anim_id::IDLE, 8.0),
            };
            if let Some(i) = i {
                clump.assocs[i].blend = 0.0;
                clump.assocs[i].blend_delta = 8.0;
            }
            return self.move_anim_tail(clump, sprint, ctx);
        }
        // C. standing.
        if mbr == 0.0 && sprint.is_none() {
            if idle.is_none() {
                clump.blend_animation(m, grp, anim_id::IDLE, 4.0);
            }
            if self.pd.time_can_run < 0.0 && self.gun.is_none() {
                if tired.is_none() {
                    if let Some(i) = clump.blend_animation(m, group::DEFAULT, 10, 4.0) {
                        clump.assocs[i].flags |= af::PLAYING;
                    }
                }
            } else if let Some(a) = tired.and_then(|u| clump.by_uid_mut(u)) {
                if a.blend > 0.0 && a.blend_delta >= 0.0 {
                    a.flags &= !af::PLAYING;
                    a.blend_delta = -2.0;
                }
            }
            self.move_state = 1;
            return self.move_anim_tail(clump, sprint, ctx);
        }
        // D. moving.
        if let Some(idle) = idle {
            match wstart {
                None => wstart = clump.add_animation(m, grp, anim_id::WALK_START).map(|i| clump.assocs[i].uid),
                Some(u) => {
                    if let Some(a) = clump.by_uid_mut(u) {
                        a.blend = 1.0;
                        a.blend_delta = 0.0;
                    }
                }
            }
            for u in [walk, run].into_iter().flatten() {
                if let Some(a) = clump.by_uid_mut(u) {
                    a.set_current_time(0.0);
                }
            }
            clump.delete(idle);
            if let Some(a) = tired.and_then(|u| clump.by_uid_mut(u)) {
                a.blend_delta = -4.0;
            }
            if let Some(s) = sprint.take() {
                clump.delete(s);
            }
            self.move_state = 4;
        }
        for (u, restore) in [(stop, true), (stop_r, true)] {
            if let Some(u) = u {
                clump.delete(u);
                if restore {
                    self.restore_heading_rate(c.turn_rate);
                }
            }
        }
        if walk.is_none() {
            walk = clump.add_animation(m, grp, anim_id::WALK).map(|i| {
                clump.assocs[i].blend = 0.0;
                clump.assocs[i].uid
            });
        }
        if run.is_none() {
            run = clump.add_animation(m, grp, anim_id::RUN).map(|i| {
                clump.assocs[i].blend = 0.0;
                clump.assocs[i].uid
            });
        }
        let (Some(walk), Some(run)) = (walk, run) else { return };
        if let Some(ws_uid) = wstart {
            let done = clump.by_uid(ws_uid).is_none_or(|a| !a.has(af::PLAYING) || a.time + a.time_step() >= a.hier.total_length);
            if done {
                clump.delete(ws_uid);
                wstart = None;
                for u in [walk, run] {
                    if let Some(a) = clump.by_uid_mut(u) {
                        a.flags |= af::PLAYING;
                    }
                }
            }
        }
        if self.move_state == 7 && wstart.is_some() {
            self.move_state = 1;
        }
        let sprint_blend = sprint.and_then(|u| clump.by_uid(u)).map(|a| (a.blend, a.blend_delta, a.time / a.hier.total_length.max(1e-6)));
        if sprint.is_none() || (self.move_state == 7 && mbr >= 0.4) {
            if wstart.is_some() {
                for u in [walk, run] {
                    if let Some(a) = clump.by_uid_mut(u) {
                        a.flags &= !af::PLAYING;
                        a.blend = 0.0;
                    }
                }
                return self.move_anim_tail(clump, sprint, ctx);
            }
            if self.move_state == 7 {
                if sprint.is_none() {
                    let (wb, rb, rd) = {
                        let w = clump.by_uid(walk).unwrap();
                        let r = clump.by_uid(run).unwrap();
                        (w.blend, r.blend, r.blend_delta)
                    };
                    if rb < 1.0 {
                        if wb == 0.0 && rb == 0.0 {
                            clump.by_uid_mut(walk).unwrap().blend = 1.0;
                        }
                        let mut delta = rd;
                        if rd <= 0.0 {
                            if let Some(i) = clump.blend_animation(m, grp, anim_id::RUN, 4.0) {
                                delta = clump.assocs[i].blend_delta;
                            }
                        }
                        self.pd.mbr = delta + 1.0; // QUIRK: blendDelta, not blend
                        return self.move_anim_tail(clump, sprint, ctx);
                    }
                    sprint = clump.blend_animation(m, grp, anim_id::SPRINT, 2.0).map(|i| clump.assocs[i].uid);
                } else if sprint_blend.is_some_and(|s| s.1 < 0.0) {
                    if let Some(a) = clump.by_uid_mut(sprint.unwrap()) {
                        a.blend_delta = 2.0;
                    }
                    if let Some(a) = clump.by_uid_mut(run) {
                        a.blend_delta = -2.0;
                    }
                }
                return self.move_anim_tail(clump, sprint, ctx);
            }
            let (wb, rb) = if mbr < 1.0 {
                self.move_state = 4;
                (1.0, 0.0)
            } else if mbr < 2.0 {
                self.move_state = 6;
                (2.0 - mbr, mbr - 1.0)
            } else {
                self.move_state = 6;
                (0.0, 1.0)
            };
            for (u, b) in [(walk, wb), (run, rb)] {
                if let Some(a) = clump.by_uid_mut(u) {
                    a.blend = b;
                    a.blend_delta = 0.0;
                }
            }
            return self.move_anim_tail(clump, sprint, ctx);
        }
        // E. a sprint assoc exists but we are no longer sprinting.
        let (sb, sd, phase) = sprint_blend.unwrap();
        let su = sprint.unwrap();
        if sb == 0.0 {
            let a = clump.by_uid_mut(su).unwrap();
            a.flags |= af::DELETE_BLENDED_OUT;
            a.blend_delta = -1000.0;
        } else if sd < 0.0 && sb < 0.8 {
            if mbr < 1.0 {
                clump.by_uid_mut(su).unwrap().blend_delta = -8.0;
                clump.by_uid_mut(run).unwrap().blend_delta = 8.0;
            }
        } else if mbr < 0.4 {
            let id = if phase < 0.5 { 6 } else { 7 };
            if let Some(i) = clump.add_animation(m, group::DEFAULT, id) {
                clump.assocs[i].blend = 1.0;
            }
            if self.saved_turn_rate.is_none() {
                self.saved_turn_rate = Some(*c.turn_rate);
            }
            *c.turn_rate = 0.0;
            let a = clump.by_uid_mut(su).unwrap();
            a.flags |= af::DELETE_BLENDED_OUT;
            a.blend_delta = -1000.0;
            for u in [walk, run] {
                if let Some(a) = clump.by_uid_mut(u) {
                    a.flags &= !af::PLAYING;
                    a.blend = 0.0;
                    a.blend_delta = 0.0;
                }
            }
        } else if sd >= 0.0 {
            let a = clump.by_uid_mut(su).unwrap();
            a.flags |= af::DELETE_BLENDED_OUT;
            a.blend_delta = -1.0;
            clump.by_uid_mut(run).unwrap().blend_delta = 1.0;
        }
        self.move_state = if mbr > 1.0 { 6 } else { 4 };
        self.move_anim_tail(clump, sprint, ctx)
    }

    fn move_anim_tail(&mut self, clump: &mut Clump, sprint: Option<u32>, ctx: &Ctx) {
        let speed = if ctx.cam.mode == 15 { 0.7 } else { self.button_sprint_results().max(1.0) };
        if let Some(a) = sprint.and_then(|u| clump.by_uid_mut(u)) {
            a.speed = speed;
        }
    }

    fn restore_heading_rate(&mut self, rate: &mut f32) {
        if let Some(r) = self.saved_turn_rate.take() {
            *rate = r;
        }
    }

    // -------------------------------------------------------------- jump / in air / land

    fn process_air(&mut self, c: &mut PedCore, ctx: &Ctx, m: &AnimManager) {
        match self.air {
            AirTask::None => {}
            AirTask::Jump { launch, launch_done } => {
                if !launch_done {
                    match launch {
                        None => {
                            // StartLaunchAnim (0x67D7A0): the foot follows the cycle phase.
                            let cl = &mut *c.clump;
                            let a = [2i16, 1].iter().find_map(|&id| cl.get(id).filter(|a| a.blend >= 0.3)).or_else(|| cl.get(0));
                            let mut phase = 0.0;
                            if let Some(a) = a.filter(|a| a.blend > 0.3) {
                                phase = a.time / a.hier.total_length.max(1e-6) + 0.367;
                                if phase > 1.0 {
                                    phase -= 1.0;
                                }
                            }
                            let id = if phase < 0.5 { anim_id::JUMP_LAUNCH } else { anim_id::JUMP_LAUNCH + 1 };
                            let uid = cl.blend_animation(m, group::DEFAULT, id, 8.0).map(|i| {
                                cl.assocs[i].finish_cb = true;
                                cl.assocs[i].uid
                            });
                            *c.aim_rot = *c.cur_rot;
                            match uid {
                                Some(u) => self.air = AirTask::Jump { launch: Some(u), launch_done: false },
                                None => self.air = AirTask::None,
                            }
                        }
                        Some(u) => {
                            if c.clump.finished.contains(&u) || c.clump.by_uid(u).is_none() {
                                self.air = AirTask::Jump { launch, launch_done: true };
                                self.process_air(c, ctx, m);
                            }
                        }
                    }
                    return;
                }
                // CTaskSimpleJump::Launch (0x679B80).
                let cl = &*c.clump;
                let hs = if let Some(s) = cl.get(2) {
                    0.17 + (0.22 - 0.17) * s.blend
                } else if let Some(r) = cl.get(1) {
                    0.1 + (0.17 - 0.1) * r.blend
                } else {
                    0.1
                };
                c.p.apply_move_force(Vec3::new(0.0, 0.0, 8.5));
                let mv = Vec2::new(c.p.move_speed.x, c.p.move_speed.y);
                if mv.length_squared() < hs * hs || c.ground_entity {
                    c.p.move_speed.x = -c.cur_rot.sin() * hs;
                    c.p.move_speed.y = c.cur_rot.cos() * hs;
                }
                self.in_the_air = true;
                c.clump.blend_animation(m, group::DEFAULT, anim_id::JUMP_GLIDE, 8.0);
                self.air = AirTask::InAir {
                    jump_glide: true,
                    fall_glide: false,
                    anim: None,
                    anim_id: -1,
                    min_vz: 0.0,
                    assist_ms: 0.0,
                };
            }
            AirTask::InAir { jump_glide, fall_glide, mut anim, mut anim_id, mut min_vz, mut assist_ms } => {
                let cl = &mut *c.clump;
                if anim.is_some_and(|u| cl.by_uid(u).is_none()) {
                    anim = None;
                }
                if anim.is_none() {
                    self.in_the_air = true;
                    if jump_glide {
                        let a = cl.get(anim_id::JUMP_GLIDE).map(|a| (a.uid, a.blend, a.blend_delta));
                        if a.is_none_or(|(_, b, d)| b < 1.0 && d <= 0.0) {
                            cl.blend_animation(m, group::DEFAULT, anim_id::JUMP_GLIDE, 4.0);
                        }
                        if let Some((u, _, _)) = a {
                            anim = Some(u);
                            anim_id = anim_id::JUMP_GLIDE;
                        }
                    } else if !fall_glide {
                        if let Some(i) = cl.blend_animation(m, group::DEFAULT, anim_id::FALL_GLIDE, 4.0) {
                            anim = Some(cl.assocs[i].uid);
                            anim_id = anim_id::FALL_GLIDE;
                        }
                    }
                }
                let vz = c.p.move_speed.z;
                let mut finish = false;
                if vz > 0.0 {
                    // Rising: forward speed floor of 0.05 for at most 1 s.
                    if jump_glide {
                        let f = c.p.move_speed.dot(c.p.matrix.fwd);
                        if f < 0.1 * 0.5 && assist_ms < 1000.0 {
                            let fwd = c.p.matrix.fwd;
                            let mass = c.p.mass;
                            c.p.apply_move_force(fwd * ((0.05 - f) * mass));
                            assist_ms += ((ctx.ts * 0.02 * 1000.0) as i32) as f32;
                        }
                    }
                } else {
                    min_vz = min_vz.min(vz);
                    let ground = c.standing || c.ground_below.is_some();
                    if !ground {
                        if vz < -0.1 && anim.is_some() && anim_id != anim_id::FALL_FALL {
                            if let Some(i) = cl.blend_animation(m, group::DEFAULT, anim_id::FALL_FALL, 4.0) {
                                anim = Some(cl.assocs[i].uid);
                                anim_id = anim_id::FALL_FALL;
                            }
                        }
                    } else {
                        let low = c.ground_below.is_some_and(|z| c.p.matrix.pos.z - z < 1.3);
                        finish = low || c.standing;
                    }
                }
                if !finish {
                    self.air = AirTask::InAir { jump_glide, fall_glide, anim, anim_id, min_vz, assist_ms };
                    return;
                }
                self.in_the_air = false;
                // CTaskComplexInAirAndLand::CreateNextSubTask (0x67CCB0).
                let land = if anim_id == anim_id::FALL_FALL {
                    if min_vz >= -0.4 { 123 } else { 26 } // FALL_collapse / KO_skid_back (CTaskSimpleFall)
                } else if self.pd.mbr > 1.5 && (self.pad.walk_lr != 0.0 || self.pad.walk_ud != 0.0) {
                    anim_id::JUMP_LAND
                } else {
                    anim_id::FALL_LAND
                };
                self.air = AirTask::Land { anim: None, anim_id: land, first: true, finished: false };
                self.process_air(c, ctx, m);
            }
            AirTask::Land { mut anim, anim_id: id, mut first, mut finished } => {
                let cl = &mut *c.clump;
                if let Some(u) = anim {
                    if cl.finished.contains(&u) || cl.by_uid(u).is_none() {
                        finished = true;
                        if let Some(a) = cl.by_uid_mut(u) {
                            if a.id == anim_id::JUMP_LAND {
                                a.blend_delta = -100.0;
                            }
                        }
                        anim = None;
                    }
                }
                if finished {
                    self.landing = false;
                    for i in [0i16, 1, 2] {
                        if let Some(a) = cl.get_mut(i) {
                            a.set_current_time(0.0);
                        }
                    }
                    self.air = AirTask::None;
                    return;
                }
                if anim.is_none() {
                    anim = cl.blend_animation(m, group::DEFAULT, id, 100.0).map(|i| {
                        cl.assocs[i].finish_cb = true;
                        cl.assocs[i].uid
                    });
                    if anim.is_none() {
                        finished = true;
                    }
                }
                if first {
                    c.p.turn_speed = Vec3::ZERO;
                    self.landing = true;
                    if self.move_state == 7 && self.pad.sprint {
                        if let Some(a) = anim.and_then(|u| cl.by_uid_mut(u)) {
                            a.speed = 2.0;
                        }
                    }
                    first = false;
                }
                self.air = AirTask::Land { anim, anim_id: id, first, finished };
            }
        }
    }
}
