//! "Souls" combat mode (not part of GTA SA): Elden Ring-style melee for the player —
//! stamina, rolls and backsteps with i-frames, lock-on, light / heavy chains with cancel
//! windows, charged heavies, guard and guard break, hit reactions and hit-stop.
//!
//! The action timings and animations are read at run time from data exported locally
//! from the user's own copy of Elden Ring (`SoulsData`); nothing from that game is in
//! this repository. The state machine is our own, modelled on the behaviour of
//! github.com/Funny-Bones/ELDEN-RING-Combat-Rewrite.
//!
//! Animations are retargeted onto the GTA ped skeleton: each mapped ER bone's model-space
//! rotation change from its rest pose is applied to the matching CJ bone, after a fixed
//! correction that lines up the two rest poses (ER rests in an A-pose, CJ in a T-pose).

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
use std::sync::Arc;

use glam::{Mat3, Quat, Vec2, Vec3};

use crate::anim::Clump;

// ------------------------------------------------------------------ data

/// One damaging hit of an attack.
#[derive(Clone, Debug)]
pub struct Hit {
    /// Active window in animation frames (30 fps), `from <= f < to`.
    pub from: f32,
    pub to: f32,
    /// Motion value: multiplier on the weapon's attack.
    pub mv: f32,
    pub guard_damage: f32,
    pub stamina: f32,
    /// Seconds of hit-stop when it lands.
    pub stop: f32,
    pub radius: f32,
    /// Blade ends per frame from `from`: [left, up, forward] × 2, metres, character space.
    pub blade: Vec<[f32; 6]>,
}

#[derive(Clone, Debug)]
pub struct ActionDef {
    pub source: String,
    pub total: f32,
    pub input_from: f32,
    pub input_dodge_from: f32,
    pub cancel_light: f32,
    pub cancel_heavy: f32,
    pub cancel_dodge: f32,
    pub cancel_guard: f32,
    pub cancel_move: f32,
    pub iframes: (f32, f32),
    pub hits: Vec<Hit>,
    pub charge: Option<(f32, f32)>,
    pub no_turn: Vec<(f32, f32)>,
    pub turn: Vec<(f32, f32, f32)>,
    /// Cumulative root motion per frame: [left, up, forward] metres.
    pub motion: Vec<[f32; 3]>,
}

impl ActionDef {
    fn motion_at(&self, f: f32) -> [f32; 3] {
        let Some(last) = self.motion.len().checked_sub(1) else { return [0.0; 3] };
        let f = f.clamp(0.0, last as f32);
        let i = (f.floor() as usize).min(last);
        let j = (i + 1).min(last);
        let t = f - i as f32;
        let (a, b) = (self.motion[i], self.motion[j]);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
    }

    fn can_turn(&self, f: f32) -> bool {
        !self.no_turn.iter().any(|&(a, b)| f >= a && f < b)
    }

    fn turn_rate(&self, f: f32) -> f32 {
        self.turn.iter().find(|&&(a, b, _)| f >= a && f < b).map_or(TURN_ACTION_DEFAULT, |t| t.2)
    }
}

#[derive(Clone, Debug)]
pub struct WeaponInfo {
    pub name: String,
    pub attack: f32,
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

/// Everything read from the exported Elden Ring data.
#[derive(Clone, Debug, Default)]
pub struct SoulsData {
    pub walk_speed: f32,
    pub run_speed: f32,
    pub run_back_speed: f32,
    pub run_side_speed: f32,
    pub sprint_speed: f32,
    pub max_stamina: f32,
    pub weapons: Vec<WeaponInfo>,
    /// Weapon-independent actions by the exporter's variant name, e.g. `Backstep`,
    /// `Roll(Load::Medium, Dir::Front)`, `Hurt(HurtLevel::Small, Dir::Left)`.
    pub base: HashMap<String, ActionDef>,
    /// Attacks by (weapon index, two-handed, kind name).
    pub attacks: HashMap<(usize, bool, String), ActionDef>,
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
            let h = f32s(take(8)?);
            let frames = u32::from_le_bytes(h[1].to_le_bytes()) as usize;
            let data = f32s(take(frames * stride * 4)?);
            self.clips.insert(name, Clip { blend: h[0], frames, data });
        }
        Ok(())
    }

    fn base(&self, name: &str) -> Option<&ActionDef> {
        self.base.get(name)
    }

    fn weapon_index(&self, name: &str) -> usize {
        self.weapons.iter().position(|w| w.name == name).unwrap_or(0)
    }
}

// ------------------------------------------------------------------ tuning

/// Animation frames per second, and physics ts units (1/50 s) per second.
const ANIM_FPS: f32 = 30.0;
const STAMINA_REGEN: f32 = 45.0;
const GUARD_REGEN_MULT: f32 = 0.5;
const SPRINT_DRAIN: f32 = 11.0;
const GUARD_STAMINA_TAKEN: f32 = 0.55;
const ROLL_COST: f32 = 12.0;
const BACKSTEP_COST: f32 = 8.0;
const ACCEL: f32 = 26.0;
const DECEL: f32 = 32.0;
const WALK_TILT: f32 = 0.55;
const TURN_RUN: f32 = 1080.0;
const TURN_SPRINT: f32 = 480.0;
const TURN_LOCKED: f32 = 720.0;
const TURN_ACTION_DEFAULT: f32 = 360.0;
/// The dodge button rolls on release if held for less than this (frames); longer is a sprint.
const SPRINT_HOLD_FRAMES: f32 = 10.0;
const HIT_STOP_SCALE: f32 = 0.5;
const GUARD_COUNTER_WINDOW: f32 = 20.0;
const GUARD_RAISE_FRAMES: f32 = 4.0;
const GUARD_ARC_DEG: f32 = 80.0;
pub const LOCK_ON_RANGE: f32 = 15.0;
pub const LOCK_BREAK_RANGE: f32 = 22.0;
/// ER attack ratings against GTA ped health (100): a light longsword swing takes ~a third.
pub const DAMAGE_SCALE: f32 = 0.3;
/// GTA damage taken → stamina lost on a guarded hit.
const GUARD_STAMINA_PER_DAMAGE: f32 = 2.0;
/// Positions of the blade checked per step.
const SWEEP_STEPS: usize = 5;

// ------------------------------------------------------------------ input

#[derive(Clone, Copy, Default, Debug)]
pub struct Button {
    pub held: bool,
    pub pressed: bool,
    pub released: bool,
}

impl Button {
    /// Folds one frame of key state in; presses and releases persist until consumed.
    pub fn feed(&mut self, held: bool) {
        if held && !self.held {
            self.pressed = true;
        }
        if !held && self.held {
            self.released = true;
        }
        self.held = held;
    }

    fn consume(&mut self) {
        self.pressed = false;
        self.released = false;
    }
}

/// The app's input for one physics step.
#[derive(Clone, Copy, Default, Debug)]
pub struct Input {
    /// Wished world direction (GTA xy) × stick tilt 0..1.
    pub mv: Vec2,
    pub walk: bool,
    pub dodge: Button,
    pub light: Button,
    pub heavy: Button,
    pub guard: Button,
    pub lock_pressed: bool,
}

impl Input {
    fn wish(&self) -> Option<Vec2> {
        (self.mv.length() > 0.1).then(|| self.mv.normalize())
    }

    fn tilt(&self) -> f32 {
        self.mv.length().min(1.0)
    }
}

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Front,
    Back,
    Left,
    Right,
}

impl Dir {
    fn name(self) -> &'static str {
        match self {
            Dir::Front => "Front",
            Dir::Back => "Back",
            Dir::Left => "Left",
            Dir::Right => "Right",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Req {
    Light,
    Heavy,
    Dodge,
}

#[derive(Clone, Debug, PartialEq)]
enum ActionId {
    Base(String),
    Attack(String),
}

#[derive(Clone, Debug)]
struct Act {
    id: ActionId,
    f: f32,
    landed: u32,
    paid: u32,
}

#[derive(Clone, Debug)]
enum State {
    Ground,
    Act(Act),
}

/// A blade swing live this step, in the world.
#[derive(Clone, Debug)]
pub struct ActiveHit {
    pub damage: f32,
    pub stop: f32,
    pub radius: f32,
    pub sweep: Vec<(Vec3, Vec3)>,
}

/// What the HUD shows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hud {
    pub stamina: f32,
    pub max_stamina: f32,
    pub locked: Option<Vec3>,
}

/// The animation layer: the clip playing and the one it cross-fades from.
#[derive(Clone, Debug, Default)]
struct AnimState {
    clip: String,
    frame: f32,
    looped: bool,
    /// Pose being faded out (ER model space: pelvis, rotations) and fade progress in frames.
    from: Option<(Vec3, Vec<Quat>)>,
    fade: f32,
    fade_len: f32,
}

/// The souls state of the player ped.
pub struct Souls {
    pub data: Arc<SoulsData>,
    pub input: Input,
    pub stamina: f32,
    /// Index into `data.weapons`.
    pub weapon: usize,
    pub heading: f32,
    speed: f32,
    move_dir: Vec2,
    sprinting: bool,
    sprint_spent: bool,
    guarding: bool,
    guard_t: f32,
    guard_counter: f32,
    dodge_hold: f32,
    dodge_armed: bool,
    buffer: Option<Req>,
    state: State,
    /// Seconds left frozen after landing a hit.
    pub hit_stop: f32,
    /// Lock-on target (set by the world each step).
    pub target: Option<Vec3>,
    pub target_id: Option<crate::world::EntityId>,
    /// The GTA weapon type in hand (for the damage events).
    pub gta_weapon: u32,
    pub locked: bool,
    anim: AnimState,
    retarget: Option<Retarget>,
    /// Blade ends of the last step (world), for the sweep.
    /// The hit live this step (filled by `step`, consumed by the world).
    pub active_hit: Option<ActiveHit>,
    landed_hit: bool,
    /// Height of the ped origin above the feet.
    pub feet: f32,
}

impl Souls {
    pub fn new(data: Arc<SoulsData>, heading: f32) -> Self {
        let stamina = data.max_stamina;
        Self {
            data,
            input: Input::default(),
            stamina,
            weapon: 0,
            heading,
            speed: 0.0,
            move_dir: Vec2::new(-heading.sin(), heading.cos()),
            sprinting: false,
            sprint_spent: false,
            guarding: false,
            guard_t: 0.0,
            guard_counter: 0.0,
            dodge_hold: 0.0,
            dodge_armed: false,
            buffer: None,
            state: State::Ground,
            hit_stop: 0.0,
            target: None,
            target_id: None,
            gta_weapon: 0,
            locked: false,
            anim: AnimState::default(),
            retarget: None,
            active_hit: None,
            landed_hit: false,
            feet: 1.0,
        }
    }

    /// The ER weapon for a GTA melee weapon type.
    pub fn set_gta_weapon(&mut self, ty: u32) {
        self.gta_weapon = ty;
        let name = match ty {
            4 => "Dagger",
            8 => "Uchigatana",
            9 => "Greatsword",
            6 => "Great Hammer",
            7 => "Short Spear",
            15 => "Rapier",
            2 | 3 | 5 | 10..=14 => "Club",
            _ => "Fist",
        };
        self.weapon = self.data.weapon_index(name);
    }

    pub fn hud(&self) -> Hud {
        Hud { stamina: self.stamina, max_stamina: self.data.max_stamina, locked: if self.locked { self.target } else { None } }
    }

    fn fwd(&self) -> Vec2 {
        Vec2::new(-self.heading.sin(), self.heading.cos())
    }

    fn left(&self) -> Vec2 {
        Vec2::new(-self.heading.cos(), -self.heading.sin())
    }

    fn def(&self, id: &ActionId) -> Option<&ActionDef> {
        match id {
            ActionId::Base(n) => self.data.base(n),
            ActionId::Attack(k) => self.data.attacks.get(&(self.weapon, true, k.clone())),
        }
    }

    fn has_attack(&self, kind: &str) -> bool {
        self.data.attacks.contains_key(&(self.weapon, true, kind.to_string()))
    }

    fn pick(&self, kinds: &[&str]) -> Option<ActionId> {
        kinds.iter().find(|k| self.has_attack(k)).map(|k| ActionId::Attack(k.to_string()))
    }

    pub fn invincible(&self) -> bool {
        match &self.state {
            State::Act(a) => self.def(&a.id).is_some_and(|d| a.f >= d.iframes.0 && a.f < d.iframes.1),
            State::Ground => false,
        }
    }

    fn guard_up(&self) -> bool {
        self.guarding && self.guard_t >= GUARD_RAISE_FRAMES
    }

    fn spend(&mut self, cost: f32) {
        self.stamina = (self.stamina - cost).max(0.0);
    }

    fn start(&mut self, id: ActionId) {
        let cost = match &id {
            ActionId::Base(n) if n.starts_with("Roll") => ROLL_COST,
            ActionId::Base(n) if n == "Backstep" => BACKSTEP_COST,
            _ => 0.0,
        };
        self.spend(cost);
        if !matches!(&id, ActionId::Base(n) if n == "GuardHit") {
            self.guard_t = 0.0;
        }
        self.state = State::Act(Act { id, f: 0.0, landed: 0, paid: 0 });
        self.speed = 0.0;
        self.sprinting = false;
        self.guarding = false;
    }

    /// Turns for a move toward `goal`; locked on, squares up to one of four sides.
    fn square_up(&mut self, goal: f32, locked: bool) -> Dir {
        let off = angle_diff(self.heading, goal);
        let side = if !locked || off.abs() <= FRAC_PI_4 {
            Dir::Front
        } else if off.abs() >= PI - FRAC_PI_4 {
            Dir::Back
        } else if off > 0.0 {
            Dir::Left
        } else {
            Dir::Right
        };
        self.heading = match side {
            Dir::Front => goal,
            Dir::Back => goal + PI,
            Dir::Left => goal - FRAC_PI_2,
            Dir::Right => goal + FRAC_PI_2,
        };
        side
    }

    fn start_dodge(&mut self, wish: Option<Vec2>) {
        let Some(d) = wish else {
            self.start(ActionId::Base("Backstep".into()));
            return;
        };
        let side = self.square_up(heading_of(d), self.target.is_some() && self.locked);
        self.start(ActionId::Base(format!("Roll(Load::Medium, Dir::{})", side.name())));
    }

    fn next_light(&self, from: &ActionId) -> Option<ActionId> {
        match from {
            ActionId::Attack(k) if matches!(k.as_str(), "RunLight" | "RollAttack" | "BackstepAttack" | "CrouchAttack") => {
                self.pick(&["Light2", "Light1"])
            }
            ActionId::Attack(k) => {
                let next = match k.as_str() {
                    "Light1" => "Light2",
                    "Light2" => "Light3",
                    "Light3" => "Light4",
                    "Light4" => "Light5",
                    "Light5" => "Light6",
                    _ => "Light1",
                };
                self.pick(&[next, "Light1"])
            }
            ActionId::Base(n) if n.starts_with("Roll") => self.pick(&["RollAttack", "Light1"]),
            ActionId::Base(n) if n == "Backstep" => self.pick(&["BackstepAttack", "Light1"]),
            ActionId::Base(n) if n == "SprintStop" => self.pick(&["RunLight", "Light1"]),
            _ => self.pick(&["Light1"]),
        }
    }

    fn next_heavy(&self, from: &ActionId) -> Option<ActionId> {
        if self.guard_counter > 0.0 {
            return self.pick(&["GuardCounter", "Heavy1Charge"]);
        }
        match from {
            ActionId::Attack(k) if k == "Heavy1" || k == "Heavy1Charge" => self.pick(&["Heavy2Charge", "Heavy1Charge"]),
            ActionId::Base(n) if n == "SprintStop" => self.pick(&["RunHeavy", "Heavy1Charge"]),
            _ => self.pick(&["Heavy1Charge", "Heavy1"]),
        }
    }

    fn read_buttons(&mut self) {
        let (listening, listening_dodge) = match &self.state {
            State::Act(a) => self.def(&a.id).map_or((true, true), |d| (a.f >= d.input_from, a.f >= d.input_dodge_from)),
            State::Ground => (true, true),
        };
        let inp = self.input;
        if inp.dodge.pressed {
            self.dodge_hold = 0.0;
            self.dodge_armed = true;
        }
        if inp.dodge.released {
            if self.dodge_armed && self.dodge_hold < SPRINT_HOLD_FRAMES && listening_dodge {
                self.buffer = Some(Req::Dodge);
            }
            self.dodge_armed = false;
            self.dodge_hold = 0.0;
            self.sprint_spent = false;
        }
        if listening {
            if inp.heavy.pressed {
                self.buffer = Some(Req::Heavy);
            }
            if inp.light.pressed {
                self.buffer = Some(Req::Light);
            }
        }
    }

    /// One physics step. `pos` is the ped origin (GTA world); returns the wished ground
    /// velocity (m/s, GTA xy).
    pub fn step(&mut self, pos: Vec3, dt: f32) -> Vec2 {
        let df = dt * ANIM_FPS;
        self.active_hit = None;
        // Lock-on toggle.
        if self.input.lock_pressed {
            self.locked = !self.locked && self.target.is_some();
        }
        if self.target.is_none() {
            self.locked = false;
        }
        let target = if self.locked { self.target } else { None };

        self.read_buttons();
        if self.input.dodge.held {
            self.dodge_hold += df;
        }
        self.input.dodge.consume();
        self.input.light.consume();
        self.input.heavy.consume();
        self.input.guard.consume();
        self.input.lock_pressed = false;

        if self.hit_stop > 0.0 {
            self.hit_stop -= dt;
            return Vec2::ZERO;
        }
        self.guard_counter = (self.guard_counter - df).max(0.0);

        let (vel, regen) = match self.state.clone() {
            State::Ground => self.ground(target, df, dt),
            State::Act(a) => self.act(a, pos, target, df, dt),
        };
        if regen {
            let m = if self.guarding { GUARD_REGEN_MULT } else { 1.0 };
            self.stamina = (self.stamina + STAMINA_REGEN * m * dt).min(self.data.max_stamina);
        }
        vel
    }

    fn ground(&mut self, target: Option<Vec3>, df: f32, dt: f32) -> (Vec2, bool) {
        let inp = self.input;
        let wish = inp.wish();
        if let Some(req) = self.buffer.take() {
            if self.stamina > 0.0 {
                let attack = match req {
                    Req::Dodge => {
                        self.start_dodge(wish);
                        return (Vec2::ZERO, false);
                    }
                    Req::Light if self.sprinting => self.pick(&["RunLight", "Light1"]),
                    Req::Light => self.pick(&["Light1"]),
                    Req::Heavy if self.sprinting => self.pick(&["RunHeavy", "Heavy1Charge"]),
                    Req::Heavy if self.guard_counter > 0.0 => self.pick(&["GuardCounter", "Heavy1Charge"]),
                    Req::Heavy => self.pick(&["Heavy1Charge", "Heavy1"]),
                };
                if let Some(a) = attack {
                    self.start(a);
                    return (Vec2::ZERO, false);
                }
            }
        }
        let was_sprinting = self.sprinting;
        self.sprinting = inp.dodge.held && self.dodge_hold >= SPRINT_HOLD_FRAMES && wish.is_some() && !self.sprint_spent && self.stamina > 0.0;
        if self.sprinting {
            self.stamina -= SPRINT_DRAIN * dt;
            if self.stamina <= 0.0 {
                self.stamina = 0.0;
                self.sprinting = false;
                self.sprint_spent = true;
            }
        }
        if was_sprinting && wish.is_none() && self.speed > self.data.run_speed + 0.5 {
            self.start(ActionId::Base("SprintStop".into()));
            return (Vec2::ZERO, true);
        }
        if inp.guard.held && !self.sprinting {
            self.guarding = true;
            self.guard_t += df;
        } else {
            self.guarding = false;
            self.guard_t = 0.0;
        }
        let d = &self.data;
        let strafing = target.is_some() && !self.sprinting;
        let walking = inp.walk || inp.tilt() < WALK_TILT;
        let want = match wish {
            None => 0.0,
            Some(_) if self.sprinting => d.sprint_speed,
            Some(_) if walking => d.walk_speed,
            Some(w) if strafing => {
                let along = w.dot(self.fwd());
                if along > FRAC_PI_4.cos() {
                    d.run_speed
                } else if along < -FRAC_PI_4.cos() {
                    d.run_back_speed
                } else {
                    d.run_side_speed
                }
            }
            Some(_) => d.run_speed,
        };
        let rate = if want > self.speed { ACCEL } else { DECEL };
        self.speed = approach(self.speed, want, rate * dt);
        match target {
            Some(t) if strafing => {
                let to = Vec2::new(t.x, t.y);
                let _ = to;
                if let Some(w) = wish {
                    self.move_dir = w;
                }
            }
            _ => {
                if let Some(w) = wish {
                    let rate = if self.sprinting { TURN_SPRINT } else { TURN_RUN };
                    self.heading = turn_toward(self.heading, heading_of(w), rate.to_radians() * dt);
                    self.move_dir = self.fwd();
                }
            }
        }
        (self.move_dir * self.speed, !self.sprinting)
    }

    /// Turns toward the lock-on target (done in `face_target` with the position).
    pub fn face_target(&mut self, pos: Vec3, dt: f32) {
        let Some(t) = (if self.locked { self.target } else { None }) else { return };
        let to = Vec2::new(t.x - pos.x, t.y - pos.y);
        if to.length_squared() < 1e-4 {
            return;
        }
        let rate = match &self.state {
            State::Ground if !self.sprinting => TURN_LOCKED,
            State::Act(a) => {
                let Some(d) = self.def(&a.id) else { return };
                let fixed = matches!(&a.id, ActionId::Base(n) if n.starts_with("Roll") || n == "Backstep");
                if fixed || !d.can_turn(a.f) {
                    return;
                }
                d.turn_rate(a.f)
            }
            _ => return,
        };
        self.heading = turn_toward(self.heading, heading_of(to), rate.to_radians() * dt);
    }

    fn act(&mut self, mut a: Act, pos: Vec3, target: Option<Vec3>, df: f32, dt: f32) -> (Vec2, bool) {
        let Some(def) = self.def(&a.id).cloned() else {
            self.state = State::Ground;
            return (Vec2::ZERO, true);
        };
        let wish = self.input.wish();
        let prev = a.f;
        // Letting go inside the charge window swaps to the uncharged swing.
        if let (Some((from, to)), ActionId::Attack(k)) = (def.charge, &a.id) {
            if a.f >= from && a.f < to && !self.input.heavy.held {
                let release = if k == "Heavy2Charge" { "Heavy2" } else { "Heavy1" };
                if self.has_attack(release) {
                    self.start(ActionId::Attack(release.into()));
                    return (Vec2::ZERO, false);
                }
            }
        }
        a.f += df;
        for (i, hit) in def.hits.iter().enumerate() {
            if a.paid & (1 << i) == 0 && a.f >= hit.from {
                self.spend(hit.stamina);
                a.paid |= 1 << i;
            }
        }
        let fixed = target.is_some() && matches!(&a.id, ActionId::Base(n) if n.starts_with("Roll") || n == "Backstep");
        if def.can_turn(a.f) && !fixed && target.is_none() {
            if let Some(w) = wish {
                self.heading = turn_toward(self.heading, heading_of(w), def.turn_rate(a.f).to_radians() * dt);
            }
        }
        // Root motion → a velocity for this step.
        let (m0, m1) = (def.motion_at(prev), def.motion_at(a.f));
        let step = self.fwd() * (m1[2] - m0[2]) + self.left() * (m1[0] - m0[0]);
        let vel = step / dt.max(1e-4);

        // The blade.
        if let ActionId::Attack(_) = &a.id {
            if let Some((i, hit)) = def.hits.iter().enumerate().find(|(i, h)| a.f >= h.from && a.f < h.to && a.landed & (1 << i) == 0) {
                let weapon_attack = self.data.weapons.get(self.weapon).map_or(100.0, |w| w.attack);
                let mut sweep = Vec::with_capacity(SWEEP_STEPS);
                let start = (a.f - df).max(hit.from);
                for s in 0..SWEEP_STEPS {
                    let at = start + (a.f - start) * s as f32 / (SWEEP_STEPS - 1) as f32;
                    if let Some((p, q)) = blade_at(&hit.blade, hit.from, at) {
                        sweep.push((self.place(pos, p), self.place(pos, q)));
                    }
                }
                if !sweep.is_empty() {
                    self.active_hit = Some(ActiveHit { damage: weapon_attack * hit.mv * DAMAGE_SCALE, stop: hit.stop, radius: hit.radius, sweep });
                }
                if std::mem::take(&mut self.landed_hit) {
                    a.landed |= 1 << i;
                    self.active_hit = None;
                }
            }
        }

        if let Some(req) = self.buffer {
            let open = a.f
                >= match req {
                    Req::Light => def.cancel_light,
                    Req::Heavy => def.cancel_heavy,
                    Req::Dodge => def.cancel_dodge,
                };
            if open {
                self.buffer = None;
                if self.stamina > 0.0 {
                    let next = match req {
                        Req::Dodge => {
                            self.start_dodge(wish);
                            return (vel, false);
                        }
                        Req::Light => self.next_light(&a.id),
                        Req::Heavy => self.next_heavy(&a.id),
                    };
                    if let Some(n) = next {
                        self.start(n);
                        return (vel, false);
                    }
                }
            }
        }
        let regen = a.f >= def.cancel_move;
        if self.input.guard.held && a.f >= def.cancel_guard {
            self.state = State::Ground;
            self.speed = 0.0;
        } else if let (Some(w), true) = (wish, a.f >= def.cancel_move) {
            self.state = State::Ground;
            self.move_dir = w;
            self.speed = match &a.id {
                ActionId::Base(n) if n.starts_with("Roll") => self.data.run_speed,
                _ => 0.0,
            };
        } else if a.f >= def.total {
            self.state = State::Ground;
            self.speed = 0.0;
        } else {
            self.state = State::Act(a);
        }
        (vel, regen)
    }

    /// Character-space [left, up, forward] → GTA world.
    fn place(&self, pos: Vec3, v: Vec3) -> Vec3 {
        let (f, l) = (self.fwd(), self.left());
        Vec3::new(pos.x + l.x * v.x + f.x * v.z, pos.y + l.y * v.x + f.y * v.z, pos.z - self.feet + v.y)
    }

    /// The live blade connected: hit-stop, and it can't hit again this swing.
    pub fn mark_hit(&mut self, stop: f32) {
        self.landed_hit = true;
        self.hit_stop = stop * HIT_STOP_SCALE;
    }

    /// A hit taken from `from` (GTA world) worth `damage` GTA health. Returns whether the
    /// GTA damage still applies (false: dodged or blocked).
    pub fn receive_hit(&mut self, pos: Vec3, from: Vec3, damage: f32) -> bool {
        if self.invincible() {
            return false;
        }
        let toward = Vec2::new(from.x - pos.x, from.y - pos.y);
        let in_arc = toward.normalize_or_zero().dot(self.fwd()) >= GUARD_ARC_DEG.to_radians().cos();
        if self.guard_up() && in_arc {
            self.stamina -= damage * GUARD_STAMINA_PER_DAMAGE * GUARD_STAMINA_TAKEN;
            if self.stamina <= 0.0 {
                self.stamina = 0.0;
                self.start(ActionId::Base("GuardBreak".into()));
            } else {
                self.start(ActionId::Base("GuardHit".into()));
                self.guard_counter = GUARD_COUNTER_WINDOW;
            }
            return false;
        }
        let level = if damage < 10.0 {
            "Small"
        } else if damage < 25.0 {
            "Middle"
        } else if damage < 50.0 {
            "Large"
        } else {
            "Knockdown"
        };
        let off = angle_diff(self.heading, heading_of(toward));
        let side = if toward.length_squared() < 1e-6 || off.abs() <= FRAC_PI_4 {
            Dir::Front
        } else if off.abs() >= PI - FRAC_PI_4 {
            Dir::Back
        } else if off > 0.0 {
            Dir::Left
        } else {
            Dir::Right
        };
        self.start(ActionId::Base(format!("Hurt(HurtLevel::{level}, Dir::{})", side.name())));
        true
    }

    // -------------------------------------------------------------- animation

    /// The clip the state wants and whether it loops, plus its playback rate.
    fn wanted_clip(&self) -> (String, bool, f32, f32) {
        let stance = self.data.weapons.get(self.weapon).map_or(0, |w| w.stance[1]);
        let cat = |id: u32| -> String {
            let n = format!("a{stance:03}_{id:06}");
            if self.data.clips.contains_key(&n) { n } else { format!("a000_{id:06}") }
        };
        match &self.state {
            State::Act(a) => {
                let src = self.def(&a.id).map(|d| d.source.clone()).unwrap_or_default();
                (src, false, a.f, 1.0)
            }
            State::Ground => {
                if self.speed < 0.2 {
                    let idle = if self.guarding { 100 } else { 0 };
                    return (cat(idle), true, -1.0, 1.0);
                }
                let d = &self.data;
                if self.sprinting {
                    return (cat(20200), true, -1.0, self.speed / d.sprint_speed);
                }
                let walking = self.speed <= d.walk_speed + 0.3;
                let (base, nominal) = if walking { (20000, d.walk_speed) } else { (20100, d.run_speed) };
                // Locked on: the four directional loops.
                let side = if self.locked && self.target.is_some() {
                    let off = angle_diff(self.heading, heading_of(self.move_dir));
                    if off.abs() <= FRAC_PI_4 {
                        Dir::Front
                    } else if off.abs() >= PI - FRAC_PI_4 {
                        Dir::Back
                    } else if off > 0.0 {
                        Dir::Left
                    } else {
                        Dir::Right
                    }
                } else {
                    Dir::Front
                };
                (cat(base + side.index() as u32), true, -1.0, self.speed / nominal.max(0.1))
            }
        }
    }

    /// Advances the animation and writes the retargeted pose onto the clump.
    pub fn animate(&mut self, clump: &mut Clump, dt: f32) {
        let (name, looped, frame, rate) = self.wanted_clip();
        let data = self.data.clone();
        if self.retarget.is_none() {
            self.retarget = Retarget::new(&data, clump);
            if let Some(r) = &self.retarget {
                self.feet = r.feet;
            }
        }
        let Some(rt) = self.retarget.as_ref() else { return };
        let nb = data.bones.len();
        if name != self.anim.clip {
            // Fade from the pose shown so far.
            let cur = sample(&data, &self.anim.clip, self.anim.frame, self.anim.looped, nb);
            let blend = data.clips.get(&name).map_or(4.0, |c| c.blend.max(1.0));
            self.anim = AnimState { clip: name.clone(), frame: 0.0, looped, from: cur, fade: 0.0, fade_len: blend };
        }
        let df = dt * ANIM_FPS;
        self.anim.frame = if frame >= 0.0 { frame } else { self.anim.frame + df * rate };
        self.anim.fade += df;
        let Some((mut pelvis, mut rots)) = sample(&data, &self.anim.clip, self.anim.frame, looped, nb) else { return };
        if let Some((fp, fr)) = &self.anim.from {
            let t = (self.anim.fade / self.anim.fade_len).clamp(0.0, 1.0);
            if t < 1.0 {
                pelvis = fp.lerp(pelvis, t);
                for (r, f) in rots.iter_mut().zip(fr) {
                    *r = f.slerp(*r, t);
                }
            } else {
                self.anim.from = None;
            }
        }
        rt.apply(&data, clump, pelvis, &rots);
    }
}

/// The pose of a clip at a frame: pelvis position and bone rotations (ER model space).
fn sample(data: &SoulsData, name: &str, frame: f32, looped: bool, nb: usize) -> Option<(Vec3, Vec<Quat>)> {
    let c = data.clips.get(name)?;
    let stride = 3 + 4 * nb;
    let last = (c.frames.max(1) - 1) as f32;
    let f = if looped && last > 0.0 { frame.rem_euclid(last) } else { frame.clamp(0.0, last) };
    let i = (f.floor() as usize).min(c.frames - 1);
    let j = (i + 1).min(c.frames - 1);
    let t = f - i as f32;
    let (a, b) = (&c.data[i * stride..][..stride], &c.data[j * stride..][..stride]);
    let pelvis = Vec3::new(a[0], a[1], a[2]).lerp(Vec3::new(b[0], b[1], b[2]), t);
    let rots = (0..nb)
        .map(|k| {
            let o = 3 + 4 * k;
            let qa = Quat::from_xyzw(a[o], a[o + 1], a[o + 2], a[o + 3]);
            let qb = Quat::from_xyzw(b[o], b[o + 1], b[o + 2], b[o + 3]);
            qa.slerp(qb, t)
        })
        .collect();
    Some((pelvis, rots))
}

// ------------------------------------------------------------------ retargeting

/// Havok model space (+X left, +Y up, -Z forward) → GTA ped model space (+X right,
/// +Y forward, +Z up).
fn er_to_gta() -> Mat3 {
    Mat3::from_cols(Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, -1.0, 0.0))
}

/// (ER bone, ER child for its direction, GTA bone tag, GTA child tag).
const MAP: &[(&str, &str, i32, i32)] = &[
    ("Pelvis", "", 1, 0),
    ("Spine1", "Spine2", 2, 3),
    ("Spine2", "Neck", 3, 4),
    ("Neck", "Head", 4, 5),
    ("Head", "", 5, 0),
    ("R_Clavicle", "R_UpperArm", 21, 22),
    ("R_UpperArm", "R_Forearm", 22, 23),
    ("R_Forearm", "R_Hand", 23, 24),
    ("R_Hand", "R_Finger1", 24, 25),
    ("L_Clavicle", "L_UpperArm", 31, 32),
    ("L_UpperArm", "L_Forearm", 32, 33),
    ("L_Forearm", "L_Hand", 33, 34),
    ("L_Hand", "L_Finger1", 34, 35),
    ("L_Thigh", "L_Calf", 41, 42),
    ("L_Calf", "L_Foot", 42, 43),
    ("L_Foot", "L_Toe0", 43, 44),
    ("L_Toe0", "", 44, 0),
    ("R_Thigh", "R_Calf", 51, 52),
    ("R_Calf", "R_Foot", 52, 53),
    ("R_Foot", "R_Toe0", 53, 54),
    ("R_Toe0", "", 54, 0),
];

struct Retarget {
    /// Per clump frame: the ER bone driving it and its rest correction `C · B`.
    driven: Vec<Option<(usize, Quat)>>,
    /// ER rest rotations (inverse), per ER bone.
    rest_inv: Vec<Quat>,
    /// Bind model rotations of the clump frames.
    bind_model: Vec<Quat>,
    /// The reference pose's locals (for the frames the ER skeleton does not drive).
    reference: Vec<(Quat, Vec3)>,
    pelvis_frame: usize,
    pelvis_bind: Vec3,
    er_pelvis_rest: Vec3,
    feet: f32,
}

impl Retarget {
    fn new(data: &SoulsData, clump: &Clump) -> Option<Self> {
        let n = clump.num_frames();
        let a = er_to_gta();
        // Reference model transforms from the clump's current pose: GTA's own upright
        // stance (the DFF bind pose lies on its back; the anims stand it up through the
        // root). Frames are listed parents first.
        let reference = clump.pose.clone();
        let mut bm_rot = vec![Quat::IDENTITY; n];
        let mut bm_pos = vec![Vec3::ZERO; n];
        for k in 0..n {
            let (lr, lp) = reference[k];
            match clump.parent(k) {
                Some(p) => {
                    bm_rot[k] = bm_rot[p] * lr;
                    bm_pos[k] = bm_pos[p] + bm_rot[p] * lp;
                }
                None => {
                    bm_rot[k] = lr;
                    bm_pos[k] = lp;
                }
            }
        }
        let er = |name: &str| data.bones.iter().position(|b| b == name);
        let mut driven = vec![None; n];
        for &(eb, ec, tag, ctag) in MAP {
            let (Some(ei), Some(k)) = (er(eb), clump.frame_of_tag(tag)) else { continue };
            // Rest correction: rotate CJ's bind direction onto ER's rest direction.
            let c = match (er(ec), clump.frame_of_tag(ctag)) {
                (Some(eci), Some(kc)) if !ec.is_empty() && ctag != 0 => {
                    let ed = (a * (data.rest[eci].0 - data.rest[ei].0)).normalize_or_zero();
                    let cd = (bm_pos[kc] - bm_pos[k]).normalize_or_zero();
                    if ed == Vec3::ZERO || cd == Vec3::ZERO { Quat::IDENTITY } else { Quat::from_rotation_arc(cd, ed) }
                }
                _ => Quat::IDENTITY,
            };
            driven[k] = Some((ei, c * bm_rot[k]));
        }
        let pelvis_frame = clump.frame_of_tag(1)?;
        let pi = er("Pelvis")?;
        let er_pelvis_rest = data.rest[pi].0;
        let pelvis_bind = bm_pos[pelvis_frame];
        Some(Self {
            driven,
            rest_inv: data.rest.iter().map(|r| r.1.inverse()).collect(),
            bind_model: bm_rot,
            reference,
            pelvis_frame,
            pelvis_bind,
            er_pelvis_rest,
            feet: er_pelvis_rest.y - pelvis_bind.z,
        })
    }

    fn apply(&self, _data: &SoulsData, clump: &mut Clump, pelvis: Vec3, rots: &[Quat]) {
        let a = er_to_gta();
        let n = clump.num_frames();
        let mut model = vec![Quat::IDENTITY; n];
        let mut pos_model = vec![Vec3::ZERO; n];
        for k in 0..n {
            let parent = clump.parent(k);
            let (prot, ppos) = parent.map_or((Quat::IDENTITY, Vec3::ZERO), |p| (model[p], pos_model[p]));
            let reset = self.reference[k].1;
            let m = match self.driven[k] {
                Some((ei, cb)) => {
                    let delta = rots[ei] * self.rest_inv[ei];
                    let dg = Quat::from_mat3(&(a * Mat3::from_quat(delta) * a.transpose())).normalize();
                    dg * cb
                }
                None => prot * self.reference[k].0,
            };
            let _ = self.bind_model[k];
            model[k] = m;
            let mut p = ppos + prot * reset;
            if k == self.pelvis_frame {
                p = self.pelvis_bind + a * (pelvis - self.er_pelvis_rest);
            }
            pos_model[k] = p;
            let local_rot = prot.inverse() * m;
            let local_pos = if k == self.pelvis_frame { prot.inverse() * (p - ppos) } else { reset };
            clump.pose[k] = (local_rot.normalize(), local_pos);
        }
    }
}

// ------------------------------------------------------------------ helpers

/// The ends of a blade at `frame` from samples starting at `from` rounded down.
fn blade_at(blade: &[[f32; 6]], from: f32, frame: f32) -> Option<(Vec3, Vec3)> {
    let last = blade.len().checked_sub(1)?;
    let at = (frame - from.floor()).clamp(0.0, last as f32);
    let (a, b) = (blade[at as usize], blade[(at as usize + 1).min(last)]);
    let t = at.fract();
    let mix = |i: usize| Vec3::new(a[i] + (b[i] - a[i]) * t, a[i + 1] + (b[i + 1] - a[i + 1]) * t, a[i + 2] + (b[i + 2] - a[i + 2]) * t);
    Some((mix(0), mix(3)))
}

/// GTA heading of a world xy direction (fwd = (-sin h, cos h)).
pub fn heading_of(d: Vec2) -> f32 {
    (-d.x).atan2(d.y)
}

fn angle_diff(from: f32, to: f32) -> f32 {
    (to - from + PI).rem_euclid(std::f32::consts::TAU) - PI
}

fn turn_toward(h: f32, goal: f32, max: f32) -> f32 {
    h + angle_diff(h, goal).clamp(-max, max)
}

fn approach(v: f32, goal: f32, max: f32) -> f32 {
    v + (goal - v).clamp(-max, max)
}

/// Shortest distance between segments a-b and c-d.
pub fn segment_distance(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> f32 {
    let (u, v, w) = (b - a, d - c, a - c);
    let (uu, uv, vv, uw, vw) = (u.dot(u), u.dot(v), v.dot(v), u.dot(w), v.dot(w));
    let denom = uu * vv - uv * uv;
    let mut s = if denom > 1e-8 { ((uv * vw - vv * uw) / denom).clamp(0.0, 1.0) } else { 0.0 };
    let t = if vv > 1e-8 { ((uv * s + vw) / vv).clamp(0.0, 1.0) } else { 0.0 };
    if uu > 1e-8 {
        s = ((uv * t - uw) / uu).clamp(0.0, 1.0);
    }
    (a + u * s).distance(c + v * t)
}

// ------------------------------------------------------------------ world passes

use crate::ped::PedLogic;
use crate::world::{EntityId, World};

impl World {
    fn souls_of(&mut self, id: EntityId) -> Option<&mut Souls> {
        self.body_mut(id)?.logic.as_any_mut().downcast_mut::<PedLogic>()?.souls.as_deref_mut()
    }

    /// Before ProcessControl: the lock-on target.
    pub(crate) fn souls_pre(&mut self) {
        let Some(pid) = self.player_id() else { return };
        let Some(ppos) = self.body(pid).map(|b| b.phys.matrix.pos) else { return };
        let Some(s) = self.souls_of(pid) else { return };
        let (want_new, cur, heading) = (s.input.lock_pressed && !s.locked, s.target_id, s.heading);
        let alive_ped = |w: &World, id: EntityId| -> Option<Vec3> {
            let b = w.body(id)?;
            let l = b.logic.as_any().downcast_ref::<PedLogic>()?;
            (!l.is_player && l.tasks.health.alive()).then_some(b.phys.matrix.pos)
        };
        let mut target = cur.and_then(|id| alive_ped(self, id).map(|p| (id, p))).filter(|(_, p)| p.distance(ppos) <= LOCK_BREAK_RANGE);
        if want_new {
            let fwd = Vec2::new(-heading.sin(), heading.cos());
            target = self
                .body_ids()
                .into_iter()
                .filter_map(|id| alive_ped(self, id).map(|p| (id, p)))
                .filter(|(_, p)| p.distance(ppos) <= LOCK_ON_RANGE)
                // Nearest, favouring what is in front.
                .min_by(|a, b| {
                    let score = |p: Vec3| {
                        let d = Vec2::new(p.x - ppos.x, p.y - ppos.y);
                        d.length() * (2.0 - d.normalize_or_zero().dot(fwd))
                    };
                    score(a.1).total_cmp(&score(b.1))
                });
        }
        let Some(s) = self.souls_of(pid) else { return };
        s.target_id = target.map(|t| t.0);
        s.target = target.map(|t| t.1);
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
        let weapon_ty = s.gta_weapon;
        let mut landed = false;
        for id in self.body_ids() {
            if id == pid {
                continue;
            }
            let Some(b) = self.body(id) else { continue };
            let Some(l) = b.logic.as_any().downcast_ref::<PedLogic>() else { continue };
            if !l.tasks.health.alive() || l.vehicle.is_some() {
                continue;
            }
            let p = b.phys.matrix.pos;
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
            // Which side of the victim the attacker is on: 0 front, 1 left, 2 back, 3 right.
            let to = (ppos - p).truncate();
            let fwd = b.phys.matrix.fwd.truncate();
            let right = b.phys.matrix.right.truncate();
            let dir = if to.dot(fwd).abs() >= to.dot(right).abs() {
                if to.dot(fwd) >= 0.0 { 0 } else { 2 }
            } else if to.dot(right) >= 0.0 {
                3
            } else {
                1
            };
            if let Some(l) = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
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
            }
            landed = true;
        }
        if landed {
            if let Some(s) = self.souls_of(pid) {
                s.mark_hit(hit.stop);
            }
        }
    }
}
