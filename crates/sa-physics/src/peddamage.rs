//! Ped health and damage: `CPedDamageResponseCalculator` (0x4AD3F0: stats multiplier, armour,
//! kill test), the reactions `CEventDamage::ComputeDamageAnim` (0x4B3FC0) and
//! `ComputeDeathAnim` (0x4B3A60) for the player, `CTaskComplexFallAndGetUp`, `CTaskSimpleDie`
//! and the wasted state of `CGameLogic::Update` (0x442AD0).
//!
//! Not ported: NPC peds (nothing to hit yet), CTaskSimpleBeHit's own anim handling beyond
//! blending the hit anim, choking, drowning, burning peds, body-part removal, the cheats.

use glam::Vec3;

use crate::{
    anim::{AnimManager, Clump, af, group},
    pedtask::{AirTask, PedTasks},
    world::EntityId,
};

/// One `GenerateDamageEvent` / calculator input.
#[derive(Debug, Clone, Copy)]
pub struct DamageIn {
    pub src: Option<EntityId>,
    /// Source position (for the push direction of knock-downs / deaths).
    pub src_pos: Option<Vec3>,
    /// eWeaponType: 22.. bullets, 49 rammed by car, 50 run over, 51 explosion, 53 drowning, 54 fall.
    pub ty: u32,
    pub damage: f32,
    pub piece: u8,
    /// 0 front, 1 left, 2 back, 3 right.
    pub dir: u8,
    /// The attacker's CTaskSimpleFight at the hit (FightHitPed).
    pub fight: Option<FightHit>,
}

/// What ComputeDamageAnim reads from the attacker's fight task.
#[derive(Debug, Clone, Copy)]
pub struct FightHit {
    /// +0x24 comboSet, +0x25 currentMove, the combo's anim group.
    pub combo_set: i8,
    pub mv: i8,
    pub group: usize,
    /// `IsComboFall` (FALL_n flag of the move) / `IsComboNoFall`.
    pub fall: bool,
    pub no_fall: bool,
}

/// ped+0x530 life state as far as the port needs it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Life {
    #[default]
    Alive,
    /// CTaskSimpleDie playing its anim (state 0x36).
    Dying { anim: Option<u32> },
    /// The die anim has finished: wasted (PlayerInfo+0xDC = 1) since `since_ms`.
    Wasted { since_ms: u32 },
}

/// `CTaskComplexFallAndGetUp`: knocked down, lie for `down_ms`, then `Getup`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FallAndGetUp {
    Fall { anim: Option<u32>, down_ms: u32, landed_at: Option<u32> },
    GetUp { anim: Option<u32> },
}

#[derive(Debug, Clone, Copy)]
pub struct Health {
    /// ped+0x540 / +0x544 / +0x548.
    pub health: f32,
    pub max_health: f32,
    pub armour: f32,
    pub life: Life,
    /// PlayerData+0x28: the hit-anim cooldown (ms).
    pub hit_anim_until: u32,
    pub fall: Option<FallAndGetUp>,
    /// PlayerInfo+0xE4 / +0xE8: last health / armour hit (HUD flash).
    pub last_health_hit: u32,
    pub last_armour_hit: u32,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            health: 100.0,
            max_health: 100.0,
            armour: 0.0,
            life: Life::Alive,
            hit_anim_until: 0,
            fall: None,
            last_health_hit: 0,
            last_armour_hit: 0,
        }
    }
}

impl Health {
    pub fn alive(&self) -> bool {
        self.life == Life::Alive
    }
}

/// Death anim ids (group 0).
mod da {
    pub const KO_SHOT_FRONT: i16 = 15;
    pub const KO_SHOT_STOM: i16 = 17;
    pub const KD_LEFT: i16 = 22;
    pub const KD_RIGHT: i16 = 23;
    pub const KO_SKID_FRONT: i16 = 24;
    pub const KO_SPIN_R: i16 = 25;
    pub const KO_SKID_BACK: i16 = 26;
    pub const KO_SPIN_L: i16 = 27;
    pub const SHOT_PARTIAL: i16 = 28;
    pub const HIT_FRONT: i16 = 32;
    pub const GETUP: i16 = 112;
}

/// `CPed::GetLocalDirection` (0x5DEF60): 0 front, 1 left, 2 back, 3 right for the direction
/// `d` towards the attacker, relative to `heading`.
pub fn local_direction(heading: f32, d: glam::Vec2) -> u8 {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, TAU};
    let mut a = (-d.x).atan2(d.y) - heading;
    while a < 0.0 {
        a += TAU;
    }
    a += FRAC_PI_4;
    while a > TAU {
        a -= TAU;
    }
    ((a / FRAC_PI_2) as u8).min(3)
}

impl PedTasks {
    /// `CPedDamageResponseCalculator::ComputeDamageResponse` (0x4B5AC0) for the player:
    /// returns (health lost, armour lost, killed).
    fn compute_damage_response(&mut self, d: &DamageIn, now: u32) -> (f32, f32, bool) {
        let h = &mut self.health;
        // AccountForPedDamageStats: the player always takes a third.
        let mut dmg = d.damage * 0.33;
        // AccountForPedArmour: drowning and falls ignore it.
        let mut armour_lost = 0.0;
        if h.armour != 0.0 && d.ty != 53 && d.ty != 54 {
            h.last_armour_hit = now;
            if dmg <= h.armour {
                armour_lost = dmg;
                h.armour -= dmg;
                dmg = 0.0;
            } else {
                armour_lost = h.armour;
                dmg -= h.armour;
                h.armour = 0.0;
            }
        }
        // ComputeWillKillPed (the player is never force-killed).
        let health_lost;
        let killed;
        if h.health - dmg < 1.0 {
            health_lost = h.health;
            h.health = 0.0;
            killed = true;
        } else {
            health_lost = dmg;
            h.health -= dmg;
            killed = false;
        }
        if health_lost + armour_lost > 0.0 {
            h.last_health_hit = now;
        }
        (health_lost, armour_lost, killed)
    }

    /// `GenerateDamageEvent` + the event handler's response, for the player. Returns the
    /// death push (source position, force) when the hit kills.
    pub fn take_damage(&mut self, d: DamageIn, clump: &mut Clump, m: &AnimManager, now: u32) -> Option<(Vec3, f32)> {
        if !self.health.alive() {
            return None;
        }
        let (lost, armour_lost, killed) = self.compute_damage_response(&d, now);
        if killed {
            return self.die(&d, clump, m);
        }
        let _ = (lost, armour_lost);
        // Reactions (ComputeDamageAnim / ComputeDamageResponse for the player).
        match d.ty {
            22..=33 => {
                let shotgun_like = matches!(d.ty, 24..=27 | 33);
                let partial = !shotgun_like || self.move_state > 1 || self.ducking;
                if now > self.health.hit_anim_until {
                    self.health.hit_anim_until = now + if d.ty == 31 { 2500 } else if partial { 1500 } else { 2500 };
                    let id = if partial { da::SHOT_PARTIAL + d.dir as i16 } else { da::HIT_FRONT + d.dir as i16 };
                    if partial {
                        // Added at blend 0 and faded in at 8 (no task).
                        let i = clump.index_of(id).or_else(|| clump.add_animation(m, group::DEFAULT, id));
                        if let Some(i) = i {
                            let a = &mut clump.assocs[i];
                            a.blend = 0.0;
                            a.blend_delta = 8.0;
                            a.start(0.0);
                        }
                    } else {
                        clump.blend_animation(m, group::DEFAULT, id, 8.0);
                    }
                }
                None
            }
            0..=15 => self.melee_damage_anim(&d, clump, m),
            49 | 50 => {
                // CTaskComplexFallAndGetUp with the KillPedWithCar anim, 500 ms down for the player.
                let id = match d.dir {
                    1 => da::KD_LEFT,
                    3 => da::KD_RIGHT,
                    2 => da::KO_SKID_BACK,
                    _ => da::KO_SKID_FRONT,
                };
                self.knock_down(id, group::DEFAULT, 500, clump, m);
                None
            }
            _ => None,
        }
    }

    /// `CEventDamage::ComputeDamageAnim` (0x4B3FC0) for a melee hit (torso) and the damage
    /// response: a knock-down (CTaskComplexFallAndGetUp, 1000 / (rate·0.025) ms) or the hit
    /// anim (CTaskSimpleBeHit). Returns the knock force push (source, force) for the caller.
    fn melee_damage_anim(&mut self, d: &DamageIn, clump: &mut Clump, m: &AnimManager) -> Option<(Vec3, f32)> {
        if matches!(self.health.fall, Some(FallAndGetUp::Fall { .. })) {
            // Lying on the floor: FLOOR_hit.
            clump.blend_animation(m, group::DEFAULT, 36, 8.0);
            return None;
        }
        let f = d.fight;
        let mut flag = false;
        let mut force = 0.0;
        if d.ty < 9 && self.health.health < 15.0 {
            flag = true;
            force = 1.0;
        } else if f.is_some_and(|f| f.mv == 4) && !self.is_player && self.move_state > 4 {
            flag = true;
        }
        let knocked = flag && (d.dir != 0 || f.is_none_or(|f| !f.fall && !f.no_fall));
        if !knocked {
            flag = false;
        }
        let (mut grp, mut id, delta) = match f {
            Some(f) if d.dir == 0 && f.combo_set >= 4 && f.mv <= 2 => {
                if f.fall {
                    flag = true;
                }
                (f.group, 219 + f.mv as i16, 16.0)
            }
            _ => {
                let id = if d.dir == 2 && d.ty <= 15 { 40 } else { 32 + d.dir as i16 };
                (group::DEFAULT, id, 8.0)
            }
        };
        if flag && knocked {
            grp = group::DEFAULT;
            id = da::KO_SKID_FRONT + d.dir as i16;
        }
        if flag {
            let down = (1000.0 / (self.shooting_rate as f32 * 0.025)) as u32;
            self.knock_down(id, grp, down, clump, m);
            return (force > 0.0).then_some((d.src_pos?, force));
        }
        // CTaskSimpleBeHit.
        clump.blend_animation(m, grp, id, delta);
        None
    }

    /// Start `CTaskComplexFallAndGetUp`.
    fn knock_down(&mut self, anim_id: i16, grp: usize, down_ms: u32, clump: &mut Clump, m: &AnimManager) {
        self.abort_fight(clump, m);
        self.gun = None;
        self.throw = None;
        self.air = AirTask::None;
        let anim = clump.blend_animation(m, grp, anim_id, 8.0).map(|i| {
            clump.assocs[i].finish_cb = true;
            clump.assocs[i].uid
        });
        self.health.fall = Some(FallAndGetUp::Fall { anim, down_ms, landed_at: None });
    }

    /// `ComputeDeathAnim` (0x4B3A60) + `CTaskComplexDie` / `CTaskSimpleDie`. Returns the push.
    fn die(&mut self, d: &DamageIn, clump: &mut Clump, m: &AnimManager) -> Option<(Vec3, f32)> {
        let (anim_id, force) = match d.ty {
            0 | 1 | 3 | 9 | 46 => (da::KO_SKID_FRONT + d.dir as i16, 0.5),
            2 | 5..=8 | 10 => (da::KO_SKID_FRONT + d.dir as i16, 1.5),
            4 | 11..=15 => (da::KO_SKID_FRONT + d.dir as i16, 0.0),
            24 => (da::KO_SKID_FRONT + d.dir as i16, 1.0),
            25..=27 | 38 => (da::KO_SKID_FRONT + d.dir as i16, 2.0),
            30 | 31 => (da::KO_SKID_FRONT + d.dir as i16, 0.5),
            16 | 35 | 36 | 39 | 51 => (da::KO_SKID_FRONT + d.dir as i16, 0.0),
            49 | 50 => (
                match d.dir {
                    0 => match d.piece {
                        5 => da::KD_LEFT,
                        6 => da::KD_RIGHT,
                        _ => da::KO_SKID_BACK,
                    },
                    1 => da::KO_SPIN_R,
                    2 => match d.piece {
                        5 => da::KD_LEFT,
                        6 => da::KD_RIGHT,
                        _ => da::KO_SKID_FRONT,
                    },
                    _ => da::KO_SPIN_L,
                },
                0.0,
            ),
            53 => (140, 0.0),
            54 => (da::KO_SHOT_STOM, 0.0),
            _ => (da::KO_SHOT_FRONT, 0.0),
        };
        self.abort_fight(clump, m);
        self.gun = None;
        self.throw = None;
        self.duck = None;
        self.ducking = false;
        self.air = AirTask::None;
        self.health.fall = None;
        let anim = clump.blend_animation(m, group::DEFAULT, anim_id, 4.0).map(|i| {
            let a = &mut clump.assocs[i];
            a.finish_cb = true;
            // CTaskSimpleDie holds the last frame.
            a.flags &= !(af::FADE_OUT_FINISHED | af::DELETE_BLENDED_OUT);
            a.uid
        });
        self.health.life = Life::Dying { anim };
        // Push away from the source: d·force·-5 horizontally, +5 up (ApplyMoveForce).
        match (force > 0.0, d.src_pos) {
            (true, Some(sp)) => Some((sp, force)),
            _ => None,
        }
    }

    /// Per step: the knock-down / get-up and die tasks. Returns true while they take over
    /// the ped (no player control).
    pub fn process_health(&mut self, clump: &mut Clump, m: &AnimManager, standing: bool, now: u32) -> bool {
        match self.health.life {
            Life::Alive => {}
            Life::Dying { anim } => {
                let done = anim.is_none_or(|u| clump.finished.contains(&u) || clump.by_uid(u).is_none_or(|a| a.is_finished()));
                if done && standing {
                    // The die anim finished (ped+0x478 & 0x20): wasted.
                    self.health.life = Life::Wasted { since_ms: now };
                }
                return true;
            }
            Life::Wasted { .. } => return true,
        }
        let Some(f) = self.health.fall else { return false };
        match f {
            FallAndGetUp::Fall { anim, down_ms, landed_at } => {
                let finished = anim.is_none_or(|u| clump.finished.contains(&u) || clump.by_uid(u).is_none_or(|a| a.is_finished()));
                let landed_at = match landed_at {
                    None if finished && standing => Some(now),
                    x => x,
                };
                if let Some(t) = landed_at {
                    if now >= t + down_ms {
                        let anim = clump.blend_animation(m, group::DEFAULT, da::GETUP, 4.0).map(|i| {
                            clump.assocs[i].finish_cb = true;
                            clump.assocs[i].uid
                        });
                        self.health.fall = Some(FallAndGetUp::GetUp { anim });
                        return true;
                    }
                }
                self.health.fall = Some(FallAndGetUp::Fall { anim, down_ms, landed_at });
                true
            }
            FallAndGetUp::GetUp { anim } => {
                let done = anim.is_none_or(|u| clump.finished.contains(&u) || clump.by_uid(u).is_none());
                if done {
                    self.health.fall = None;
                    if let Some(u) = anim {
                        if let Some(a) = clump.by_uid_mut(u) {
                            a.flags |= af::DELETE_BLENDED_OUT;
                            a.blend_delta = -4.0;
                        }
                    }
                    clump.blend_animation(m, self.anim_group, crate::anim::anim_id::IDLE, 4.0);
                    return false;
                }
                true
            }
        }
    }

    /// `RestorePlayerStuffDuringResurrection` (0x442060): full max health, no armour, no
    /// weapons.
    pub fn resurrect(&mut self, clump: &mut Clump, m: &AnimManager) {
        self.health.health = self.health.max_health;
        self.health.armour = 0.0;
        self.health.life = Life::Alive;
        self.health.fall = None;
        self.weapons = Default::default();
        self.set_current_weapon(0);
        self.pd = Default::default();
        clump.assocs.clear();
        clump.blend_animation(m, self.anim_group, crate::anim::anim_id::IDLE, 1000.0);
    }
}
