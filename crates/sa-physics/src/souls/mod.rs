//! "Souls" combat mode (not part of GTA SA): Elden Ring-style movement and melee for the
//! player, and Elden Ring hit reactions with poise for the peds it fights.
//!
//! The action timings and animations are read at run time from data exported locally
//! from the user's own copy of Elden Ring (`SoulsData`); nothing from that game is in
//! this repository. The state machine is our own, modelled on the behaviour of
//! github.com/Funny-Bones/ELDEN-RING-Combat-Rewrite.
//!
//! * `player` — the state machine: locomotion, sprint, crouch, rolls / backsteps with
//!   i-frames, jumps and falls, light / heavy / charged / running / rolling / backstep /
//!   crouch / jump attacks, the left hand (shield guard, off-hand chains, power stance),
//!   grip and weapon swaps, guard, guard break and guard counters, hurt reactions, hit-stop.
//! * `anim` — clip selection, cross-fades, the upper-body layers (carry, guard, swap) and
//!   the retargeting of the ER skeleton onto the GTA ped skeleton.
//! * `world` — lock-on targeting, blade hits, enemy poise and reactions.

use std::collections::HashMap;

use glam::{Quat, Vec3};

pub mod anim;
pub mod player;
pub mod world;

pub use player::{Button, Grip, Input, Load, Souls};
pub use world::Reaction;

/// One damaging hit of an attack.
#[derive(Clone, Debug)]
pub struct Hit {
    /// Active window in animation frames (30 fps), `from <= f < to`.
    pub from: f32,
    pub to: f32,
    /// Motion value: multiplier on the weapon's attack.
    pub mv: f32,
    /// Multiplier on stamina / poise damage.
    pub guard_damage: f32,
    pub stamina: f32,
    /// Seconds of hit-stop when it lands.
    pub stop: f32,
    pub radius: f32,
    /// Blade ends per frame from `from`: [left, up, forward] × 2, metres, character space.
    pub blade: Vec<[f32; 6]>,
}

/// One action (an animation with its timing data).
#[derive(Clone, Debug)]
pub struct ActionDef {
    pub source: String,
    pub total: f32,
    pub input_from: f32,
    pub input_dodge_from: f32,
    pub cancel_light: f32,
    pub cancel_heavy: f32,
    pub cancel_dodge: f32,
    pub cancel_jump: f32,
    pub cancel_guard: f32,
    pub cancel_move: f32,
    pub cancel_left: f32,
    pub iframes: (f32, f32),
    /// Low sweeps pass underneath while this is airborne.
    pub jump_frames: bool,
    pub hits: Vec<Hit>,
    pub charge: Option<(f32, f32)>,
    pub no_turn: Vec<(f32, f32)>,
    pub turn: Vec<(f32, f32, f32)>,
    /// Cumulative root motion per frame: [left, up, forward] metres.
    pub motion: Vec<[f32; 3]>,
}

impl ActionDef {
    pub fn motion_at(&self, f: f32) -> [f32; 3] {
        let Some(last) = self.motion.len().checked_sub(1) else { return [0.0; 3] };
        let f = f.clamp(0.0, last as f32);
        let i = (f.floor() as usize).min(last);
        let j = (i + 1).min(last);
        let t = f - i as f32;
        let (a, b) = (self.motion[i], self.motion[j]);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
    }

    pub fn can_turn(&self, f: f32) -> bool {
        !self.no_turn.iter().any(|&(a, b)| f >= a && f < b)
    }

    pub fn turn_rate(&self, f: f32) -> f32 {
        self.turn.iter().find(|&&(a, b, _)| f >= a && f < b).map_or(player::TURN_ACTION_DEFAULT, |t| t.2)
    }
}

/// A jump attack's swing while still airborne.
#[derive(Clone, Debug)]
pub struct AirAttackDef {
    pub from: f32,
    pub to: f32,
    pub stamina: f32,
    pub source: String,
    pub radius: f32,
    pub blade: Vec<[f32; 6]>,
}

/// A grip or weapon change: a reach (`start`) during which it takes effect, then a settle.
#[derive(Clone, Debug)]
pub struct SwapDef {
    pub start: String,
    pub end: String,
    pub start_len: f32,
    pub end_len: f32,
    pub apply: f32,
    pub free_from: f32,
}

impl SwapDef {
    pub fn total(&self) -> f32 {
        self.start_len + self.end_len
    }
}

#[derive(Clone, Debug)]
pub struct WeaponInfo {
    pub name: String,
    pub attack: f32,
    pub weight: f32,
    /// Animation category of the idle / guard stance: [one-handed, two-handed].
    pub stance: [u8; 2],
}

/// A baked clip: per frame the pelvis position and the model rotation of every bone.
#[derive(Clone, Debug)]
pub struct Clip {
    pub blend: f32,
    pub frames: usize,
    pub data: Vec<f32>,
}

/// Which jump attack: light, heavy, or with a weapon in each hand.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AirKind {
    Light,
    Heavy,
    Paired,
}

/// Everything read from the exported Elden Ring data.
#[derive(Clone, Debug, Default)]
pub struct SoulsData {
    pub walk_speed: f32,
    pub run_speed: f32,
    pub run_back_speed: f32,
    pub run_side_speed: f32,
    pub sprint_speed: f32,
    pub crouch_walk_speed: f32,
    pub crouch_run_speed: f32,
    pub max_stamina: f32,
    pub weapons: Vec<WeaponInfo>,
    /// Weapon-independent actions by the exporter's variant name, e.g. `Backstep`,
    /// `Roll(Load::Medium, Dir::Front)`, `Hurt(HurtLevel::Small, Dir::Left)`.
    pub base: HashMap<String, ActionDef>,
    /// Attacks by (weapon index, two-handed, kind name).
    pub attacks: HashMap<(usize, bool, String), ActionDef>,
    /// Jump attacks by (weapon index, two-handed, kind).
    pub air: HashMap<(usize, bool, AirKind), AirAttackDef>,
    /// Grip / weapon swaps by kind name (`NextWeapon`, `ToTwoHandRight`, …).
    pub swaps: HashMap<String, SwapDef>,
    /// The exported bones and their rest pose (Havok model space: position, rotation).
    pub bones: Vec<String>,
    pub rest: Vec<(Vec3, Quat)>,
    pub clips: HashMap<String, Clip>,
}

impl SoulsData {
    /// Parses `anims.bin` (written by the local exporter) into `bones`, `rest` and `clips`.
    pub fn load_anims(&mut self, b: &[u8]) -> Result<(), String> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8], String> {
            let s = b.get(at..at + n).ok_or("anims.bin truncated")?;
            at += n;
            Ok(s)
        };
        if take(4)? != b"ERRT" {
            return Err("not an ERRT file".into());
        }
        let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap());
        let f32s = |s: &[u8]| s.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect::<Vec<f32>>();
        let nb = u32_(take(4)?) as usize;
        let nc = u32_(take(4)?) as usize;
        for _ in 0..nb {
            let len = take(1)?[0] as usize;
            self.bones.push(String::from_utf8_lossy(take(len)?).into_owned());
            let v = f32s(take(28)?);
            self.rest.push((Vec3::new(v[0], v[1], v[2]), Quat::from_xyzw(v[3], v[4], v[5], v[6])));
        }
        let stride = 3 + 4 * nb;
        for _ in 0..nc {
            let len = take(1)?[0] as usize;
            let name = String::from_utf8_lossy(take(len)?).into_owned();
            let h = take(8)?;
            let blend = f32::from_le_bytes(h[0..4].try_into().unwrap());
            let frames = u32_(&h[4..8]) as usize;
            let data = f32s(take(frames * stride * 4)?);
            self.clips.insert(name, Clip { blend, frames, data });
        }
        Ok(())
    }

    pub fn weapon_index(&self, name: &str) -> Option<usize> {
        self.weapons.iter().position(|w| w.name == name)
    }

    /// The left-hand items: (shield, fist, torch).
    pub fn specials(&self) -> (usize, usize, usize) {
        let n = self.weapons.len().saturating_sub(1);
        (self.weapon_index("Shield").unwrap_or(n), self.weapon_index("Fist").unwrap_or(0), self.weapon_index("Torch").unwrap_or(0))
    }
}

/// A GTA stand-in for an ER weapon class: (GTA weapon type for damage, model id to hold).
/// The shield and the torch have no GTA model (-1; the app draws its own).
pub fn gta_weapon_for(name: &str) -> (u32, i32) {
    match name {
        "Dagger" => (4, 335),
        "Longsword" | "Claymore" | "Greatsword" | "Rapier" | "Uchigatana" | "Heavy Thrusting Sword" | "Curved Sword"
        | "Curved Greatsword" | "Twinblade" | "Colossal Weapon" | "Straight Sword" => (8, 339),
        "Club" | "Flail" | "Whip" => (5, 336),
        "Battle Axe" | "Greataxe" | "Great Hammer" | "Axe" | "Hammer" => (6, 337),
        "Short Spear" | "Halberd" | "Great Spear" | "Reaper" | "Spear" => (7, 338),
        "Claw" => (1, 331),
        _ => (0, -1),
    }
}

/// The ER weapon class standing in for a GTA melee weapon type.
pub fn er_weapon_for(ty: u32) -> &'static str {
    match ty {
        4 => "Dagger",
        8 => "Uchigatana",
        9 => "Greatsword",
        6 => "Great Hammer",
        7 => "Short Spear",
        15 => "Rapier",
        1 => "Claw",
        2 | 3 | 5 | 10..=14 => "Club",
        _ => "Fist",
    }
}
