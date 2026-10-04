//! `CWeaponInfo` (weapon.dat) and `CWeapon` (a ped's weapon slot): the table, skill rows,
//! reload times, ammo/state handling and `CWeapon::Fire`'s bookkeeping. The bullets
//! themselves (`FireInstantHit`, `DoBulletImpact`) are world code, see `bullet.rs`.

use glam::Vec3;
use sa_formats::weapondat::WeaponDat;

/// `eWeaponType`.
pub mod wt {
    pub const UNARMED: u32 = 0;
    pub const BRASSKNUCKLE: u32 = 1;
    pub const BASEBALLBAT: u32 = 5;
    pub const SHOVEL: u32 = 6;
    pub const POOLCUE: u32 = 7;
    pub const CHAINSAW: u32 = 9;
    pub const GRENADE: u32 = 16;
    pub const TEARGAS: u32 = 17;
    pub const MOLOTOV: u32 = 18;
    pub const PISTOL: u32 = 22;
    pub const PISTOL_SILENCED: u32 = 23;
    pub const DESERT_EAGLE: u32 = 24;
    pub const SHOTGUN: u32 = 25;
    pub const SAWNOFF: u32 = 26;
    pub const SPAS12: u32 = 27;
    pub const MICRO_UZI: u32 = 28;
    pub const MP5: u32 = 29;
    pub const AK47: u32 = 30;
    pub const M4: u32 = 31;
    pub const TEC9: u32 = 32;
    pub const COUNTRYRIFLE: u32 = 33;
    pub const SNIPERRIFLE: u32 = 34;
    pub const RLAUNCHER: u32 = 35;
    pub const RLAUNCHER_HS: u32 = 36;
    pub const FTHROWER: u32 = 37;
    pub const MINIGUN: u32 = 38;
    pub const SATCHEL_CHARGE: u32 = 39;
    pub const DETONATOR: u32 = 40;
    pub const SPRAYCAN: u32 = 41;
    pub const EXTINGUISHER: u32 = 42;
    pub const CAMERA: u32 = 43;
    pub const PARACHUTE: u32 = 46;
}

/// Name table at 0x8D6150 (`FindWeaponType`).
pub const WEAPON_NAMES: [&str; 49] = [
    "UNARMED", "BRASSKNUCKLE", "GOLFCLUB", "NIGHTSTICK", "KNIFE", "BASEBALLBAT", "SHOVEL", "POOLCUE", "KATANA",
    "CHAINSAW", "DILDO1", "DILDO2", "VIBE1", "VIBE2", "FLOWERS", "CANE", "GRENADE", "TEARGAS", "MOLOTOV", "ROCKET",
    "ROCKET_HS", "FREEFALL_BOMB", "PISTOL", "PISTOL_SILENCED", "DESERT_EAGLE", "SHOTGUN", "SAWNOFF", "SPAS12",
    "MICRO_UZI", "MP5", "AK47", "M4", "TEC9", "COUNTRYRIFLE", "SNIPERRIFLE", "RLAUNCHER", "RLAUNCHER_HS", "FTHROWER",
    "MINIGUN", "SATCHEL_CHARGE", "DETONATOR", "SPRAYCAN", "EXTINGUISHER", "CAMERA", "NIGHTVISION", "INFRARED",
    "PARACHUTE", "", "ARMOUR",
];

/// `CWeaponInfo::FindWeaponType`: 0 (UNARMED) for unknown names.
pub fn find_weapon_type(name: &str) -> u32 {
    WEAPON_NAMES.iter().position(|n| !n.is_empty() && n.eq_ignore_ascii_case(name)).unwrap_or(0) as u32
}

/// `eWeaponFire` (FindWeaponFireType 0x5BCF30; unknown strings are INSTANT_HIT).
pub mod fire {
    pub const MELEE: u8 = 0;
    pub const INSTANT_HIT: u8 = 1;
    pub const PROJECTILE: u8 = 2;
    pub const AREA_EFFECT: u8 = 3;
    pub const CAMERA: u8 = 4;
    pub const USE: u8 = 5;

    pub fn from_name(s: &str) -> u8 {
        match s {
            "MELEE" => MELEE,
            "PROJECTILE" => PROJECTILE,
            "AREA_EFFECT" => AREA_EFFECT,
            "CAMERA" => CAMERA,
            "USE" => USE,
            _ => INSTANT_HIT,
        }
    }
}

/// weapon.dat flag bits (column `a`).
pub mod wf {
    pub const CANAIM: u32 = 0x1;
    pub const AIMWITHARM: u32 = 0x2;
    pub const FIRSTPERSON: u32 = 0x4;
    pub const ONLYFREEAIM: u32 = 0x8;
    pub const MOVEAIM: u32 = 0x10;
    pub const MOVEFIRE: u32 = 0x20;
    pub const THROW: u32 = 0x100;
    pub const HEAVY: u32 = 0x200;
    pub const CONTINUOUSFIRE: u32 = 0x400;
    pub const TWIN_PISTOL: u32 = 0x800;
    pub const RELOAD: u32 = 0x1000;
    pub const CROUCHFIRE: u32 = 0x2000;
    pub const RELOAD2START: u32 = 0x4000;
    pub const LONG_RELOAD: u32 = 0x8000;
    pub const SLOWSDOWN: u32 = 0x10000;
    pub const RANDSPEED: u32 = 0x20000;
    pub const EXPANDS: u32 = 0x40000;
}

/// `CWeaponInfo` (0x70 bytes; table of 80 at 0xC8AAB8). Times in seconds.
#[derive(Debug, Clone)]
pub struct WeaponInfo {
    pub fire_type: u8,
    pub target_range: f32,
    pub weapon_range: f32,
    pub model1: i32,
    pub model2: i32,
    pub slot: i32,
    pub flags: u32,
    /// Index into the anim assoc group table (`sa_physics::anim::GROUPS` ids).
    pub anim_group: usize,
    pub ammo_clip: i16,
    pub damage: i16,
    pub fire_offset: Vec3,
    pub skill: i32,
    pub req_stat: i32,
    pub accuracy: f32,
    pub move_speed: f32,
    pub anim_loop_start: f32,
    pub anim_loop_end: f32,
    pub anim_loop_fire: f32,
    pub anim2_loop_start: f32,
    pub anim2_loop_end: f32,
    pub anim2_loop_fire: f32,
    pub breakout_time: f32,
    pub speed: f32,
    pub radius: f32,
    pub lifespan: f32,
    pub spread: f32,
    pub aim_offset_index: usize,
    pub base_combo: u8,
    pub num_combos: u8,
}

impl Default for WeaponInfo {
    /// `CWeaponInfo::Initialise` (0x5BF750) defaults.
    fn default() -> Self {
        Self {
            fire_type: 0,
            target_range: 0.0,
            weapon_range: 0.0,
            model1: -1,
            model2: -1,
            slot: -1,
            flags: 0,
            anim_group: 0,
            ammo_clip: 0,
            damage: 0,
            fire_offset: Vec3::ZERO,
            skill: 1,
            req_stat: 0,
            accuracy: 1.0,
            move_speed: 1.0,
            anim_loop_start: 0.0,
            anim_loop_end: 0.0,
            anim_loop_fire: 0.0,
            anim2_loop_start: 0.0,
            anim2_loop_end: 0.0,
            anim2_loop_fire: 0.0,
            breakout_time: 0.0,
            speed: 0.0,
            radius: 0.0,
            lifespan: 0.0,
            spread: 0.0,
            aim_offset_index: 0,
            base_combo: 4,
            num_combos: 1,
        }
    }
}

impl WeaponInfo {
    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    /// Loop start / end / fire time, the second set when ducking (0x608F00 / 0x608F20 / 0x61C150).
    pub fn anim_loop(&self, ducking: bool) -> (f32, f32, f32) {
        if ducking {
            (self.anim2_loop_start, self.anim2_loop_end, self.anim2_loop_fire)
        } else {
            (self.anim_loop_start, self.anim_loop_end, self.anim_loop_fire)
        }
    }
}

/// weapon.dat `%` rows (0xC8A8A8, 21 entries).
#[derive(Debug, Clone, Copy, Default)]
pub struct AimOffset {
    pub aim_x: f32,
    pub aim_z: f32,
    pub duck_x: f32,
    pub duck_z: f32,
    pub rload: [u16; 2],
    pub crouch_rload: [u16; 2],
}

/// `CWeaponInfo::ms_aWeaponInfo` plus the aim-offset table.
#[derive(Debug, Clone)]
pub struct WeaponInfos {
    pub info: Vec<WeaponInfo>,
    pub aim: [AimOffset; 21],
}

impl Default for WeaponInfos {
    fn default() -> Self {
        Self { info: vec![WeaponInfo::default(); 80], aim: [AimOffset::default(); 21] }
    }
}

/// Round a loop end to the 50 Hz anim step (LoadWeaponData).
fn round_loop_end(start: f32, end: f32) -> f32 {
    ((end - start) * 50.0 + 0.1).floor() * 0.02 - 0.006 + start
}

impl WeaponInfos {
    /// `CWeaponInfo::LoadWeaponData` (0x5BE670). `group_of` maps an anim group name to its
    /// index in the assoc group table.
    pub fn load(dat: &WeaponDat, group_of: impl Fn(&str) -> Option<usize>) -> Self {
        let mut t = Self::default();
        for m in &dat.melee {
            let ty = find_weapon_type(&m.name) as usize;
            let e = &mut t.info[ty];
            e.fire_type = fire::from_name(&m.fire_type);
            e.target_range = m.target_range;
            e.weapon_range = m.weapon_range;
            e.model1 = m.model1;
            e.model2 = m.model2;
            e.slot = m.slot;
            e.num_combos = m.num_combos as u8;
            e.flags = m.flags;
            if !m.stealth_anim_group.starts_with("null") {
                if let Some(g) = group_of(&m.stealth_anim_group) {
                    e.anim_group = g;
                }
            }
        }
        let mut idx = 0usize;
        for g in &dat.guns {
            let ty = find_weapon_type(&g.name) as usize;
            let mut skill = g.skill;
            if !(22..=32).contains(&ty) {
                skill = 1;
                idx = ty;
            } else {
                idx = match skill {
                    0 => ty + 25,
                    1 => ty,
                    2 => ty + 36,
                    3 => ty + 47,
                    _ => idx, // stale local, as in the exe
                };
            }
            let e = &mut t.info[idx];
            e.fire_type = fire::from_name(&g.fire_type);
            e.target_range = g.target_range;
            e.weapon_range = g.weapon_range;
            e.model1 = g.model1;
            e.model2 = g.model2;
            e.slot = g.slot;
            e.ammo_clip = g.ammo_clip as i16;
            e.damage = g.damage as i16;
            e.fire_offset = Vec3::from(g.fire_offset);
            e.skill = skill;
            e.req_stat = g.req_stat;
            e.accuracy = g.accuracy;
            e.move_speed = g.move_speed;
            e.flags = g.flags;
            e.speed = g.speed.unwrap_or(0.0);
            e.radius = g.radius.unwrap_or(0.0);
            e.lifespan = g.lifespan.unwrap_or(0.0);
            e.spread = g.spread.unwrap_or(0.0);
            let s = |f: i32| f as f32 * 0.033_333_3;
            e.anim_loop_start = s(g.anim_loop[0]);
            e.anim_loop_end = s(g.anim_loop[1]);
            e.anim_loop_fire = s(g.anim_loop[2]);
            e.anim2_loop_start = s(g.anim_loop2[0]);
            e.anim2_loop_end = s(g.anim_loop2[1]);
            e.anim2_loop_fire = s(g.anim_loop2[2]);
            e.breakout_time = s(g.breakout_time);
            if !g.anim_group.starts_with("null") {
                if let Some(gi) = group_of(&g.anim_group) {
                    e.anim_group = gi;
                }
            }
            if 10 < e.anim_group && e.anim_group < 32 {
                e.aim_offset_index = e.anim_group - 11;
            }
            e.anim_loop_end = round_loop_end(e.anim_loop_start, e.anim_loop_end);
            e.anim2_loop_end = round_loop_end(e.anim2_loop_start, e.anim2_loop_end);
        }
        for a in &dat.aim {
            let Some(g) = group_of(&a.anim_group) else { continue };
            if !(11..32).contains(&g) {
                continue;
            }
            t.aim[g - 11] = AimOffset {
                aim_x: a.aim_x,
                aim_z: a.aim_z,
                duck_x: a.duck_x,
                duck_z: a.duck_z,
                rload: [a.reload[0] as u16, a.reload[1] as u16],
                crouch_rload: [a.crouch_reload[0] as u16, a.crouch_reload[1] as u16],
            };
        }
        t
    }

    /// `CWeaponInfo::GetWeaponInfo(type, skill)` (0x743C60).
    pub fn get(&self, ty: u32, skill: u8) -> &WeaponInfo {
        let i = match skill {
            0 => ty as usize + 25,
            1 => ty as usize,
            2 => ty as usize + 36,
            3 => ty as usize + 47,
            _ => 47,
        };
        &self.info[i.min(79)]
    }

    /// `GetWeaponReloadTime` (0x743D70), ms.
    pub fn reload_time(&self, info: &WeaponInfo) -> u32 {
        if info.has(wf::RELOAD) {
            return if info.has(wf::TWIN_PISTOL) { 2000 } else { 1000 };
        }
        if info.has(wf::LONG_RELOAD) {
            return 1000;
        }
        let a = &self.aim[info.aim_offset_index];
        for v in [a.rload[0], a.crouch_rload[0], a.rload[1]] {
            if v as u32 + 100 > 400 {
                return v as u32 + 100;
            }
        }
        (a.crouch_rload[1] as u32 + 100).max(400)
    }
}

/// `CWeapon` states.
pub mod ws {
    pub const READY: u8 = 0;
    pub const FIRING: u8 = 1;
    pub const RELOADING: u8 = 2;
    pub const OUT_OF_AMMO: u8 = 3;
    pub const MELEE_MADECONTACT: u8 = 4;
}

/// `CWeapon` (0x1C bytes; a ped holds 13 at +0x5A0).
#[derive(Debug, Clone, Copy, Default)]
pub struct Weapon {
    pub ty: u32,
    pub state: u8,
    pub ammo_in_clip: u32,
    pub total_ammo: u32,
    /// ms, compared with `CTimer::m_snTimeInMilliseconds`.
    pub time_for_next_shot: u32,
}

impl Weapon {
    /// `CWeapon::Initialise` (0x73B4A0); `clip` is the (skill) clip size.
    pub fn initialise(ty: u32, ammo: u32, clip: i16) -> Self {
        let total = ammo.min(99_999);
        let ammo_in_clip = if total != 0 { total.min(clip as u32) } else { 0 };
        Self { ty, state: ws::READY, ammo_in_clip, total_ammo: total, time_for_next_shot: 0 }
    }

    /// `HasWeaponAmmoToBeUsed` (0x73B2A0).
    pub fn has_ammo_to_be_used(&self) -> bool {
        matches!(self.ty, 0..=5 | 8..=14 | 46) || self.total_ammo != 0
    }

    /// `CWeapon::Reload` (0x73AEB0).
    pub fn reload(&mut self, clip: i16) {
        if self.total_ammo == 0 {
            return;
        }
        self.ammo_in_clip = self.total_ammo.min(clip as u32);
    }

    /// `CWeapon::Update` (0x73DB40) without the audio events. `reload_anim` is the
    /// (current time, length) of the owner's reload anim (226/227), `gun_task` whether a
    /// use-gun task runs.
    pub fn update(&mut self, now: u32, info: &WeaponInfo, clip: i16, reload_anim: Option<(f32, f32)>, gun_task: bool) {
        match self.state {
            ws::FIRING => {
                if now > self.time_for_next_shot {
                    self.state = if info.fire_type != fire::MELEE && self.total_ammo == 0 {
                        ws::OUT_OF_AMMO
                    } else {
                        ws::READY
                    };
                }
            }
            ws::RELOADING => {
                if self.ty < 47 && info.has(wf::RELOAD) {
                    match reload_anim {
                        Some((t, len)) => {
                            if now > self.time_for_next_shot && t / len < 0.9 {
                                self.time_for_next_shot = now;
                            }
                        }
                        None if gun_task => {
                            if now > self.time_for_next_shot {
                                self.time_for_next_shot = now;
                            }
                        }
                        None => {}
                    }
                }
                if now > self.time_for_next_shot {
                    self.reload(clip);
                    self.state = ws::READY;
                }
            }
            ws::MELEE_MADECONTACT => self.state = ws::READY,
            _ => {}
        }
    }

    /// First half of `CWeapon::Fire` (0x742300): can the weapon fire? `std_clip` is the
    /// skill-1 clip size Fire uses to refill an empty clip.
    pub fn can_fire(&mut self, std_clip: i16) -> bool {
        if self.state != ws::READY && self.state != ws::FIRING {
            return false;
        }
        if self.ammo_in_clip == 0 {
            if self.total_ammo == 0 {
                return false;
            }
            self.ammo_in_clip = self.total_ammo.min(std_clip as u32);
        }
        true
    }

    /// Second half of `CWeapon::Fire`, for a shot that went off: ammo, state and the
    /// next-shot timer (`set_time` false for on-foot guns, whose rate is the anim loop).
    pub fn after_shot(&mut self, now: u32, info: &WeaponInfo, reload_ms: u32, is_player: bool, set_time: bool) {
        if self.ammo_in_clip > 0 {
            self.ammo_in_clip -= 1;
        }
        if self.total_ammo > 0 && (is_player || self.total_ammo < 25_000) {
            self.total_ammo -= 1;
        }
        self.state = ws::FIRING;
        if self.ammo_in_clip == 0 {
            if self.total_ammo == 0 {
                return;
            }
            self.state = ws::RELOADING;
            self.time_for_next_shot = now + reload_ms;
            return;
        }
        self.time_for_next_shot = if !set_time {
            now
        } else if self.ty == wt::CAMERA {
            now + 1100
        } else {
            (now as i64 - ((info.anim_loop_end - info.anim_loop_start) * -900.0) as i64) as u32
        };
    }
}

/// `CPed::GetWeaponSkill(type)` (0x5E3B60) for the player: POOR / STD / PRO from the
/// weapon skill stat.
pub fn player_weapon_skill(infos: &WeaponInfos, ty: u32, stat: f32) -> u8 {
    if !(22..=32).contains(&ty) {
        return 1;
    }
    if stat >= infos.get(ty, 2).req_stat as f32 {
        2
    } else if stat < infos.get(ty, 1).req_stat as f32 {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn infos() -> WeaponInfos {
        let root = std::path::Path::new(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
        let dat = std::fs::read(root.join("data/weapon.dat")).expect("weapon.dat");
        let dat = sa_formats::weapondat::parse(&dat).unwrap();
        WeaponInfos::load(&dat, crate::anim::AnimManager::group_by_name)
    }

    #[test]
    #[ignore = "needs the game files"]
    fn loads_shipped_weapon_dat() {
        let t = infos();
        let pistol = t.get(wt::PISTOL, 1);
        assert_eq!(pistol.ammo_clip, 17);
        assert!((pistol.anim_loop_end - 0.494).abs() < 1e-4, "{}", pistol.anim_loop_end);
        assert_eq!(pistol.anim_group, 13);
        assert_eq!(pistol.flags, 0x3033);
        assert_eq!(t.get(wt::PISTOL, 2).flags & wf::TWIN_PISTOL, wf::TWIN_PISTOL);
        assert_eq!(t.reload_time(t.get(wt::SHOTGUN, 1)), 826);
        assert_eq!(t.reload_time(t.get(wt::PISTOL, 1)), 1000);
        assert_eq!(t.get(wt::M4, 1).anim_group, 25);
        assert_eq!(t.get(wt::M4, 0).anim_group, 26);
    }
}
