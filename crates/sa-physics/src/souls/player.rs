//! The player's souls state machine, in GTA terms: heading `h` with forward
//! (-sin h, cos h), z up. Movement is returned as a velocity for the GTA ped physics,
//! which keeps collision, ground following and gravity.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
use std::sync::Arc;

use glam::{Vec2, Vec3};

use super::anim::AnimState;
use super::{ActionDef, AirAttackDef, AirKind, SoulsData, SwapDef};

// ------------------------------------------------------------------ tuning

pub(crate) const ANIM_FPS: f32 = 30.0;
const STAMINA_REGEN: f32 = 45.0;
const GUARD_REGEN_MULT: f32 = 0.5;
const SPRINT_DRAIN: f32 = 11.0;
const GUARD_STAMINA_TAKEN: f32 = 0.55;
const ROLL_COST: f32 = 12.0;
const BACKSTEP_COST: f32 = 8.0;
const JUMP_COST: f32 = 10.0;
const ACCEL: f32 = 26.0;
const DECEL: f32 = 32.0;
const WALK_TILT: f32 = 0.55;
const TURN_RUN: f32 = 1080.0;
const TURN_SPRINT: f32 = 480.0;
const TURN_LOCKED: f32 = 720.0;
pub(crate) const TURN_ACTION_DEFAULT: f32 = 360.0;
/// The dodge button rolls on release if held for less than this (frames); longer sprints.
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
/// Frames a jump may stay airborne past its arc before it counts as a fall.
const JUMP_BECOMES_FALL: f32 = 3.0;
const FALL_HEAVY_LANDING: f32 = 8.0;
const FALL_DAMAGE_START: f32 = 16.0;
const FALL_DEATH: f32 = 20.0;
/// Seconds without ground under the feet before walking off counts as a fall.
const LEDGE_GRACE: f32 = 0.12;
/// Equip load (weapons + an assumed set of armour) over the starting class's capacity.
const ARMOUR_WEIGHT: f32 = 14.0;
const MAX_LOAD: f32 = 48.2;
/// Positions of the blade checked per step.
pub(crate) const SWEEP_STEPS: usize = 5;

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

/// The app's input, folded over the frames between physics steps.
#[derive(Clone, Copy, Default, Debug)]
pub struct Input {
    /// Wished world direction (GTA xy) × stick tilt 0..1.
    pub mv: Vec2,
    pub walk: bool,
    pub dodge: Button,
    pub jump: Button,
    pub light: Button,
    pub heavy: Button,
    /// The left-hand button: guard, or the off-hand attack.
    pub guard: Button,
    pub crouch: bool,
    pub lock: bool,
    /// Switch the lock-on target: -1 left, +1 right.
    pub switch_target: i8,
    pub two_hand_right: bool,
    pub two_hand_left: bool,
    pub next_weapon: bool,
    pub next_left: bool,
}

impl Input {
    fn wish(&self) -> Option<Vec2> {
        (self.mv.length() > 0.1).then(|| self.mv.normalize())
    }

    fn tilt(&self) -> f32 {
        self.mv.length().min(1.0)
    }

    fn consume(&mut self) {
        for b in [&mut self.dodge, &mut self.jump, &mut self.light, &mut self.heavy, &mut self.guard] {
            b.consume();
        }
        self.crouch = false;
        self.lock = false;
        self.two_hand_right = false;
        self.two_hand_left = false;
        self.next_weapon = false;
        self.next_left = false;
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
    pub(crate) fn name(self) -> &'static str {
        ["Front", "Back", "Left", "Right"][self as usize]
    }

    /// Of `to` relative to heading `h`.
    pub(crate) fn of(h: f32, to: f32) -> Dir {
        let off = angle_diff(h, to);
        if off.abs() <= FRAC_PI_4 {
            Dir::Front
        } else if off.abs() >= PI - FRAC_PI_4 {
            Dir::Back
        } else if off > 0.0 {
            Dir::Left
        } else {
            Dir::Right
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Load {
    Light,
    Medium,
    Heavy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grip {
    /// Weapon in the right hand, the left-hand item in the left.
    OneHand,
    /// The right weapon in both hands.
    TwoHandRight,
    /// The left item in both hands.
    TwoHandLeft,
}

/// The attacks in effect: a weapon and how it is held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Moveset {
    pub weapon: usize,
    pub two_hand: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Req {
    Light,
    Heavy,
    Left,
    Dodge,
    Jump,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ActionId {
    Base(String),
    Attack(Moveset, String),
}

#[derive(Clone, Debug)]
pub(crate) struct Act {
    pub id: ActionId,
    pub f: f32,
    landed: u32,
    paid: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Air {
    /// Horizontal velocity carried through the air.
    vel: Vec2,
    /// Came from a jump rather than off a ledge.
    pub jumped: bool,
    /// Frames since leaving the arc or the ledge.
    pub f: f32,
}

impl Air {
    fn falling(&self) -> bool {
        !self.jumped || self.f >= JUMP_BECOMES_FALL
    }
}

#[derive(Clone, Debug)]
pub(crate) enum State {
    Ground,
    Act(Act),
    Air(Air),
}

#[derive(Clone, Debug)]
pub(crate) struct AirAttack {
    pub kind: AirKind,
    pub moveset: Moveset,
    pub def: AirAttackDef,
    pub f: f32,
    hit_done: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Swap {
    pub kind: String,
    pub f: f32,
    grip: Grip,
    weapon: usize,
    left: usize,
    applied: bool,
}

/// A blade swing live this step, in the world.
#[derive(Clone, Debug)]
pub struct ActiveHit {
    pub damage: f32,
    /// Poise damage dealt.
    pub poise: f32,
    pub stop: f32,
    pub radius: f32,
    /// A heavy blow (knocks down on a poise break).
    pub heavy: bool,
    pub sweep: Vec<(Vec3, Vec3)>,
}

/// What the physics gets from a step.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepOut {
    /// Ground velocity, m/s, GTA world xy.
    pub vel: Vec2,
    /// Vertical velocity to force (jump arcs), m/s.
    pub vz: Option<f32>,
    /// Fall damage (GTA health) taken on landing.
    pub fall_damage: f32,
}

/// What the HUD shows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hud {
    pub stamina: f32,
    pub max_stamina: f32,
    pub locked: Option<Vec3>,
    pub invincible: bool,
}

/// The souls state of the player ped.
pub struct Souls {
    pub data: Arc<SoulsData>,
    pub input: Input,
    pub stamina: f32,
    /// Indices into `data.weapons`: the right-hand weapon and the left-hand item.
    pub weapon: usize,
    pub left: usize,
    pub grip: Grip,
    pub load: Load,
    pub heading: f32,
    pub(crate) speed: f32,
    pub(crate) move_dir: Vec2,
    pub(crate) sprinting: bool,
    sprint_spent: bool,
    pub(crate) crouching: bool,
    pub(crate) guarding: bool,
    guard_t: f32,
    guard_counter: f32,
    dodge_hold: f32,
    dodge_armed: bool,
    buffer: Option<Req>,
    pub(crate) state: State,
    pub(crate) air_attack: Option<AirAttack>,
    pub(crate) swap: Option<Swap>,
    /// Seconds left frozen after landing a hit.
    pub hit_stop: f32,
    /// Lock-on: target position (set by the world each step) and its body.
    pub target: Option<Vec3>,
    pub target_id: Option<crate::world::EntityId>,
    pub locked: bool,
    pub(crate) anim: AnimState,
    /// The hit live this step (filled by `step`, consumed by the world).
    pub active_hit: Option<ActiveHit>,
    /// Height of the ped origin above the feet (from the retarget).
    pub feet: f32,
    /// Origin z the current jump left from, and the highest point since leaving the ground.
    jump_base: f32,
    peak: f32,
    off_ground: f32,
    /// The GTA weapon type / model standing in for the right-hand weapon.
    pub gta_weapon: u32,
    pub gta_model: i32,
    /// The GTA weapon type last taken over (`set_gta_weapon`).
    from_gta: u32,
}

impl Souls {
    pub fn new(data: Arc<SoulsData>, heading: f32, gta_weapon: u32) -> Self {
        let stamina = data.max_stamina;
        let (shield, fist, _) = data.specials();
        let weapon = data.weapon_index(super::er_weapon_for(gta_weapon)).unwrap_or(fist);
        let mut s = Self {
            data,
            input: Input::default(),
            stamina,
            weapon,
            left: shield,
            grip: Grip::OneHand,
            load: Load::Medium,
            heading,
            speed: 0.0,
            move_dir: Vec2::new(-heading.sin(), heading.cos()),
            sprinting: false,
            sprint_spent: false,
            crouching: false,
            guarding: false,
            guard_t: 0.0,
            guard_counter: 0.0,
            dodge_hold: 0.0,
            dodge_armed: false,
            buffer: None,
            state: State::Ground,
            air_attack: None,
            swap: None,
            hit_stop: 0.0,
            target: None,
            target_id: None,
            locked: false,
            anim: AnimState::default(),
            active_hit: None,
            feet: 1.0,
            jump_base: 0.0,
            peak: 0.0,
            off_ground: 0.0,
            gta_weapon: 0,
            gta_model: -1,
            from_gta: gta_weapon,
        };
        s.equip_changed();
        s
    }

    /// The right-hand weapon from a GTA melee weapon type (the GTA weapon selection).
    pub fn set_gta_weapon(&mut self, ty: u32) {
        if ty == self.from_gta {
            return;
        }
        self.from_gta = ty;
        if let Some(w) = self.data.weapon_index(super::er_weapon_for(ty)) {
            self.weapon = w;
            self.equip_changed();
        }
    }

    /// Load tier and GTA stand-ins after the hands change.
    fn equip_changed(&mut self) {
        let w = |i: usize| self.data.weapons.get(i).map_or(0.0, |w| w.weight);
        let ratio = (w(self.weapon) + w(self.left) + ARMOUR_WEIGHT) / MAX_LOAD;
        self.load = if ratio < 0.3 {
            Load::Light
        } else if ratio < 0.7 {
            Load::Medium
        } else {
            Load::Heavy
        };
        let in_right = if self.grip == Grip::TwoHandLeft { self.left } else { self.weapon };
        let name = self.data.weapons.get(in_right).map_or("", |w| w.name.as_str());
        (self.gta_weapon, self.gta_model) = super::gta_weapon_for(name);
    }

    pub fn hud(&self) -> Hud {
        Hud {
            stamina: self.stamina,
            max_stamina: self.data.max_stamina,
            locked: if self.locked { self.target } else { None },
            invincible: self.invincible(),
        }
    }

    /// What the left hand shows (one-handed only): an index into `data.weapons`.
    pub fn left_item(&self) -> Option<usize> {
        (self.grip == Grip::OneHand).then_some(self.left)
    }

    pub(crate) fn fwd(&self) -> Vec2 {
        Vec2::new(-self.heading.sin(), self.heading.cos())
    }

    pub(crate) fn leftv(&self) -> Vec2 {
        Vec2::new(-self.heading.cos(), -self.heading.sin())
    }

    pub fn moveset(&self) -> Moveset {
        match self.grip {
            Grip::OneHand => Moveset { weapon: self.weapon, two_hand: false },
            Grip::TwoHandRight => Moveset { weapon: self.weapon, two_hand: true },
            Grip::TwoHandLeft => Moveset { weapon: self.left, two_hand: true },
        }
    }

    pub(crate) fn def(&self, id: &ActionId) -> Option<&ActionDef> {
        match id {
            ActionId::Base(n) => self.data.base.get(n),
            ActionId::Attack(m, k) => self.data.attacks.get(&(m.weapon, m.two_hand, k.clone())),
        }
    }

    fn has(&self, m: Moveset, kind: &str) -> bool {
        self.data.attacks.contains_key(&(m.weapon, m.two_hand, kind.to_string()))
    }

    fn pick(&self, kinds: &[&str]) -> Option<ActionId> {
        let m = self.moveset();
        kinds.iter().find(|k| self.has(m, k)).map(|k| ActionId::Attack(m, k.to_string()))
    }

    fn paired(&self) -> bool {
        self.grip == Grip::OneHand && self.left == self.weapon && self.has(self.moveset(), "PairedLight1")
    }

    /// The left button guards with a shield, or with anything held in both hands.
    fn left_guards(&self) -> bool {
        self.grip != Grip::OneHand || self.left == self.data.specials().0
    }

    fn pick_left(&self, paired: &[&str], single: &[&str]) -> Option<ActionId> {
        if self.paired() {
            return self.pick(paired);
        }
        let m = Moveset { weapon: self.left, two_hand: false };
        single.iter().find(|k| self.has(m, k)).map(|k| ActionId::Attack(m, k.to_string()))
    }

    pub fn invincible(&self) -> bool {
        match &self.state {
            State::Act(a) => self.def(&a.id).is_some_and(|d| a.f >= d.iframes.0 && a.f < d.iframes.1),
            _ => false,
        }
    }

    fn airborne(&self) -> bool {
        match &self.state {
            State::Air(_) => true,
            State::Act(a) => matches!(&a.id, ActionId::Base(n) if n.starts_with("Jump")) && self.def(&a.id).is_some_and(|d| d.motion_at(a.f)[1] > 0.0),
            _ => false,
        }
    }

    /// Raised and settled, or still held through the guard-hit reaction (the block holds
    /// between blows, as in the game).
    fn guard_up(&self) -> bool {
        let reacting = matches!(&self.state, State::Act(a) if matches!(&a.id, ActionId::Base(n) if n == "GuardHit"));
        (self.guarding && self.guard_t >= GUARD_RAISE_FRAMES) || (reacting && self.input.guard.held)
    }

    fn spend(&mut self, cost: f32) {
        self.stamina = (self.stamina - cost).max(0.0);
    }

    fn swap_busy(&self) -> bool {
        self.swap.as_ref().is_some_and(|s| self.data.swaps.get(&s.kind).is_some_and(|d| s.f < d.start_len + d.free_from))
    }

    fn start(&mut self, id: ActionId) {
        let cost = match &id {
            ActionId::Base(n) if n.starts_with("Roll") || n.starts_with("CrouchRoll") => ROLL_COST,
            ActionId::Base(n) if n == "Backstep" => BACKSTEP_COST,
            ActionId::Base(n) if n.starts_with("Jump") => JUMP_COST,
            _ => 0.0,
        };
        self.spend(cost);
        let crouch_roll = matches!(&id, ActionId::Base(n) if n.starts_with("CrouchRoll"));
        if !matches!(&id, ActionId::Base(n) if n == "GuardHit") {
            self.guard_t = 0.0;
        }
        self.state = State::Act(Act { id, f: 0.0, landed: 0, paid: 0 });
        self.speed = 0.0;
        self.sprinting = false;
        self.crouching &= crouch_roll;
        self.guarding = false;
    }

    /// Turns for a move toward `goal`; locked on, squares up to one of four sides.
    fn square_up(&mut self, goal: f32, locked: bool) -> Dir {
        let side = if locked { Dir::of(self.heading, goal) } else { Dir::Front };
        self.heading = match side {
            Dir::Front => goal,
            Dir::Back => goal + PI,
            Dir::Left => goal - FRAC_PI_2,
            Dir::Right => goal + FRAC_PI_2,
        };
        side
    }

    fn locked_on(&self) -> bool {
        self.locked && self.target.is_some()
    }

    fn start_dodge(&mut self, wish: Option<Vec2>) {
        let Some(d) = wish else {
            self.start(ActionId::Base("Backstep".into()));
            return;
        };
        let side = self.square_up(heading_of(d), self.locked_on());
        let load = format!("{:?}", self.load);
        let n = if self.crouching { "CrouchRoll" } else { "Roll" };
        self.start(ActionId::Base(format!("{n}(Load::{load}, Dir::{})", side.name())));
    }

    fn start_jump(&mut self, pos: Vec3) {
        let inp = self.input;
        let kind = match inp.wish() {
            None => "Stand".to_string(),
            Some(d) if self.sprinting => {
                self.heading = heading_of(d);
                "Sprint".to_string()
            }
            Some(d) => {
                let side = self.square_up(heading_of(d), self.locked_on());
                let gait = if inp.walk || inp.tilt() < WALK_TILT { "Walk" } else { "Run" };
                let suffix = if side == Dir::Front { "" } else { side.name() };
                format!("{gait}{suffix}")
            }
        };
        self.jump_base = pos.z;
        self.peak = pos.z;
        self.air_attack = None;
        self.start(ActionId::Base(format!("Jump(JumpKind::{kind})")));
    }

    fn next_light(&self, from: &ActionId) -> Option<ActionId> {
        match from {
            ActionId::Attack(_, k) if matches!(k.as_str(), "RunLight" | "RollAttack" | "BackstepAttack" | "CrouchAttack") => {
                self.pick(&["Light2", "Light1"])
            }
            ActionId::Attack(_, k) => {
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
            ActionId::Base(n) if n.starts_with("Roll") || n.starts_with("CrouchRoll") => self.pick(&["RollAttack", "Light1"]),
            ActionId::Base(n) if n == "Backstep" => self.pick(&["BackstepAttack", "Light1"]),
            ActionId::Base(n) if n == "SprintStop" => self.pick(&["RunLight", "Light1"]),
            _ => self.pick(&["Light1"]),
        }
    }

    fn next_left(&self, from: &ActionId) -> Option<ActionId> {
        let bump = |k: &str, p: &str| -> Option<String> {
            let n: u32 = k.strip_prefix(p)?.parse().ok()?;
            Some(format!("{p}{}", if n < 6 { n + 1 } else { 1 }))
        };
        let (paired, single) = match from {
            ActionId::Attack(_, k) => (
                bump(k, "PairedLight").unwrap_or("PairedLight1".into()),
                bump(k, "LeftLight").unwrap_or("LeftLight1".into()),
            ),
            ActionId::Base(n) if n.starts_with("Roll") || n.starts_with("CrouchRoll") => ("PairedRoll".into(), "LeftLight1".into()),
            ActionId::Base(n) if n == "Backstep" => ("PairedBackstep".into(), "LeftLight1".into()),
            ActionId::Base(n) if n == "SprintStop" => ("PairedRun".into(), "LeftLight1".into()),
            _ => ("PairedLight1".into(), "LeftLight1".into()),
        };
        self.pick_left(&[&paired, "PairedLight1"], &[&single, "LeftLight1"])
    }

    fn next_heavy(&self, from: &ActionId) -> Option<ActionId> {
        if self.guard_counter > 0.0 {
            return self.pick(&["GuardCounter", "Heavy1Charge"]);
        }
        match from {
            ActionId::Attack(_, k) if k == "Heavy1" || k == "Heavy1Charge" => self.pick(&["Heavy2Charge", "Heavy1Charge"]),
            ActionId::Base(n) if n == "SprintStop" => self.pick(&["RunHeavy", "Heavy1Charge"]),
            _ => self.pick(&["Heavy1Charge", "Heavy1"]),
        }
    }

    fn read_buttons(&mut self, df: f32) {
        let (listening, listening_dodge) = match &self.state {
            State::Act(a) if !matches!(&a.id, ActionId::Base(n) if n.starts_with("Jump")) => {
                self.def(&a.id).map_or((true, true), |d| (a.f >= d.input_from, a.f >= d.input_dodge_from))
            }
            _ => (true, true),
        };
        let inp = self.input;
        if inp.dodge.pressed {
            self.dodge_hold = 0.0;
            self.dodge_armed = true;
        }
        if inp.dodge.held {
            self.dodge_hold += df;
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
            if inp.jump.pressed {
                self.buffer = Some(Req::Jump);
            }
            if inp.heavy.pressed {
                self.buffer = Some(Req::Heavy);
            }
            if inp.guard.pressed && !self.left_guards() {
                self.buffer = Some(Req::Left);
            }
            if inp.light.pressed {
                self.buffer = Some(Req::Light);
            }
        }
    }

    fn sprint_held(&self) -> bool {
        self.input.dodge.held && self.dodge_hold >= SPRINT_HOLD_FRAMES
    }

    /// One physics step. `pos` is the ped origin (GTA world), `standing` whether GTA has
    /// ground under it.
    pub fn step(&mut self, pos: Vec3, standing: bool, dt: f32) -> StepOut {
        let df = dt * ANIM_FPS;
        self.active_hit = None;
        if self.input.lock {
            self.locked = !self.locked && self.target.is_some();
        }
        if self.target.is_none() {
            self.locked = false;
        }
        self.read_buttons(df);
        let inp = self.input;
        self.input.consume();

        if self.hit_stop > 0.0 {
            self.hit_stop -= dt;
            return StepOut::default();
        }
        self.guard_counter = (self.guard_counter - df).max(0.0);
        if let Some(a) = &mut self.air_attack {
            a.f += df;
        }
        // A grip or weapon change runs on the upper body alongside everything else.
        if let Some(mut swap) = self.swap.take() {
            let def = self.data.swaps.get(&swap.kind).cloned();
            swap.f += df;
            if let Some(def) = def {
                if !swap.applied && swap.f >= def.apply {
                    self.grip = swap.grip;
                    self.weapon = swap.weapon;
                    self.left = swap.left;
                    swap.applied = true;
                    self.equip_changed();
                }
                if swap.f < def.total() {
                    self.swap = Some(swap);
                }
            }
        }
        self.off_ground = if standing { 0.0 } else { self.off_ground + dt };

        let mut out = StepOut::default();
        let regen = match self.state.clone() {
            State::Ground => self.ground(inp, pos, dt, df, &mut out),
            State::Act(a) => self.act(a, inp, pos, standing, dt, df, &mut out),
            State::Air(a) => {
                self.air(a, inp, pos, standing, df, &mut out);
                false
            }
        };
        if regen {
            let m = if self.guarding { GUARD_REGEN_MULT } else { 1.0 };
            self.stamina = (self.stamina + STAMINA_REGEN * m * dt).min(self.data.max_stamina);
        }
        self.live_blade(pos, df);
        out
    }

    fn ground(&mut self, inp: Input, pos: Vec3, dt: f32, df: f32, out: &mut StepOut) -> bool {
        let wish = inp.wish();
        if self.swap_busy() {
        } else if let Some(req) = self.buffer.take() {
            if self.stamina > 0.0 {
                let attack = match req {
                    Req::Dodge => {
                        self.start_dodge(wish);
                        return false;
                    }
                    Req::Jump => {
                        self.start_jump(pos);
                        return false;
                    }
                    Req::Left if self.sprinting => self.pick_left(&["PairedRun", "PairedLight1"], &["LeftLight1"]),
                    Req::Left => self.pick_left(&["PairedLight1"], &["LeftLight1"]),
                    Req::Light if self.sprinting => self.pick(&["RunLight", "Light1"]),
                    Req::Light if self.crouching => self.pick(&["CrouchAttack", "RollAttack", "Light1"]),
                    Req::Light => self.pick(&["Light1"]),
                    Req::Heavy if self.sprinting => self.pick(&["RunHeavy", "Heavy1Charge"]),
                    Req::Heavy if self.guard_counter > 0.0 => self.pick(&["GuardCounter", "Heavy1Charge"]),
                    Req::Heavy => self.pick(&["Heavy1Charge", "Heavy1"]),
                };
                if let Some(a) = attack {
                    self.start(a);
                    return false;
                }
            }
        }
        // Grip and weapon changes, one at a time.
        if self.swap.is_none() {
            let (shield, fist, torch) = self.data.specials();
            let back = if self.grip == Grip::TwoHandLeft { "ToOneHandFromLeft" } else { "ToOneHandFromRight" };
            let n = self.data.weapons.len();
            let change = if inp.two_hand_right {
                Some(if self.grip == Grip::TwoHandRight { (back, Grip::OneHand) } else { ("ToTwoHandRight", Grip::TwoHandRight) }).map(|(k, g)| (k, g, self.weapon, self.left))
            } else if inp.two_hand_left {
                Some(if self.grip == Grip::TwoHandLeft { (back, Grip::OneHand) } else { ("ToTwoHandLeft", Grip::TwoHandLeft) }).map(|(k, g)| (k, g, self.weapon, self.left))
            } else if inp.next_weapon {
                // The shield is never a right-hand weapon.
                let mut w = (self.weapon + 1) % n;
                if w == shield {
                    w = (w + 1) % n;
                }
                Some(("NextWeapon", self.grip, w, self.left))
            } else if inp.next_left && self.grip == Grip::OneHand {
                // Shield, nothing, torch, then each weapon.
                let order: Vec<usize> = [shield, fist, torch].into_iter().chain((0..n).filter(|&i| i != shield && i != fist && i != torch)).collect();
                let at = order.iter().position(|&i| i == self.left).unwrap_or(0);
                Some(("NextLeft", self.grip, self.weapon, order[(at + 1) % order.len()]))
            } else {
                None
            };
            if let Some((kind, grip, weapon, left)) = change {
                if self.data.swaps.contains_key(kind) {
                    self.swap = Some(Swap { kind: kind.into(), f: 0.0, grip, weapon, left, applied: false });
                }
            }
        }
        if inp.crouch {
            self.crouching = !self.crouching;
        }
        // Walked off a ledge.
        if self.off_ground > LEDGE_GRACE {
            self.fall(pos, Vec2::ZERO);
            return false;
        }
        let was_sprinting = self.sprinting;
        self.sprinting = self.sprint_held() && wish.is_some() && !self.sprint_spent && self.stamina > 0.0;
        if self.sprinting {
            self.crouching = false;
            self.stamina -= SPRINT_DRAIN * dt;
            if self.stamina <= 0.0 {
                self.stamina = 0.0;
                self.sprinting = false;
                self.sprint_spent = true;
            }
        }
        if was_sprinting && wish.is_none() && self.speed > self.data.run_speed + 0.5 {
            self.start(ActionId::Base("SprintStop".into()));
            return true;
        }
        if inp.guard.held && !self.sprinting && self.left_guards() {
            self.guarding = true;
            self.guard_t += df;
        } else {
            self.guarding = false;
            self.guard_t = 0.0;
        }
        let d = &self.data;
        let locked = self.locked_on();
        let strafing = locked && !self.sprinting;
        let walking = inp.walk || inp.tilt() < WALK_TILT;
        let want = match wish {
            None => 0.0,
            Some(_) if self.sprinting => d.sprint_speed,
            Some(_) if self.crouching && walking => d.crouch_walk_speed,
            Some(_) if self.crouching => d.crouch_run_speed,
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
        if strafing {
            if let Some(w) = wish {
                self.move_dir = w;
            }
        } else if let Some(w) = wish {
            let rate = if self.sprinting { TURN_SPRINT } else { TURN_RUN };
            self.heading = turn_toward(self.heading, heading_of(w), rate.to_radians() * dt);
            self.move_dir = self.fwd();
        }
        out.vel = self.move_dir * self.speed;
        !self.sprinting
    }

    /// Turns toward the lock-on target.
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
                let fixed = matches!(&a.id, ActionId::Base(n) if n.starts_with("Roll") || n.starts_with("CrouchRoll") || n == "Backstep" || n.starts_with("Jump"));
                if fixed || !d.can_turn(a.f) {
                    return;
                }
                d.turn_rate(a.f)
            }
            _ => return,
        };
        self.heading = turn_toward(self.heading, heading_of(to), rate.to_radians() * dt);
    }

    #[allow(clippy::too_many_arguments)]
    fn act(&mut self, mut a: Act, inp: Input, pos: Vec3, standing: bool, dt: f32, df: f32, out: &mut StepOut) -> bool {
        let Some(def) = self.def(&a.id).cloned() else {
            self.state = State::Ground;
            return true;
        };
        let wish = inp.wish();
        let prev = a.f;
        // Letting go inside the charge window swaps to the uncharged swing.
        if let (Some((from, to)), ActionId::Attack(m, k)) = (def.charge, &a.id) {
            if a.f >= from && a.f < to && !inp.heavy.held {
                let release = if k == "Heavy2Charge" { "Heavy2" } else { "Heavy1" };
                if self.has(*m, release) {
                    self.start(ActionId::Attack(*m, release.into()));
                    return false;
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
        let locked = self.locked_on();
        if def.can_turn(a.f) && !locked {
            if let Some(w) = wish {
                self.heading = turn_toward(self.heading, heading_of(w), def.turn_rate(a.f).to_radians() * dt);
            }
        }
        let (m0, m1) = (def.motion_at(prev), def.motion_at(a.f));
        let step = self.fwd() * (m1[2] - m0[2]) + self.leftv() * (m1[0] - m0[0]);
        out.vel = step / dt.max(1e-4);

        let is_jump = matches!(&a.id, ActionId::Base(n) if n.starts_with("Jump"));
        if is_jump {
            self.jump(a, &def, inp, pos, standing, out);
            return false;
        }
        if self.off_ground > LEDGE_GRACE {
            self.fall(pos, out.vel);
            return false;
        }
        if let Some(req) = self.buffer {
            let open = a.f
                >= match req {
                    Req::Light => def.cancel_light,
                    Req::Heavy => def.cancel_heavy,
                    Req::Left => def.cancel_left,
                    Req::Dodge => def.cancel_dodge,
                    Req::Jump => def.cancel_jump,
                };
            if open {
                self.buffer = None;
                if self.stamina > 0.0 {
                    let next = match req {
                        Req::Dodge => {
                            self.start_dodge(wish);
                            return false;
                        }
                        Req::Jump => {
                            self.start_jump(pos);
                            return false;
                        }
                        Req::Light => self.next_light(&a.id),
                        Req::Heavy => self.next_heavy(&a.id),
                        Req::Left => self.next_left(&a.id),
                    };
                    if let Some(n) = next {
                        self.start(n);
                        return false;
                    }
                }
            }
        }
        let regen = a.f >= def.cancel_move;
        if inp.guard.held && a.f >= def.cancel_guard && self.left_guards() {
            self.state = State::Ground;
            self.speed = 0.0;
        } else if let (Some(w), true) = (wish, a.f >= def.cancel_move) {
            self.state = State::Ground;
            self.move_dir = w;
            let d = &self.data;
            self.speed = match &a.id {
                ActionId::Base(n) if n.starts_with("Roll(Load::Heavy") => d.run_speed * 0.4,
                ActionId::Base(n) if n.starts_with("Roll") || n == "LandRun" => d.run_speed,
                ActionId::Base(n) if n.starts_with("CrouchRoll") => d.crouch_run_speed,
                ActionId::Base(n) if n == "LandSprint" => {
                    if self.sprint_held() { d.sprint_speed } else { d.run_speed }
                }
                ActionId::Base(n) if n.starts_with("LandStrafeWalk") => d.walk_speed,
                ActionId::Base(n) if n == "LandStrafe(Dir::Front)" => d.run_speed,
                ActionId::Base(n) if n == "LandStrafe(Dir::Back)" => d.run_back_speed,
                ActionId::Base(n) if n.starts_with("LandStrafe") => d.run_side_speed,
                _ => 0.0,
            };
        } else if a.f >= def.total {
            self.state = State::Ground;
            self.speed = 0.0;
        } else {
            self.state = State::Act(a);
        }
        regen
    }

    /// The jump is the authored arc: height from the animation until it ends, then GTA's
    /// gravity takes over.
    fn jump(&mut self, a: Act, def: &ActionDef, inp: Input, pos: Vec3, standing: bool, out: &mut StepOut) {
        let up = def.motion_at(a.f)[1];
        self.try_air_attack(a.f >= def.cancel_light);
        if up <= 0.0 {
            // Still crouching into it: leave from wherever the feet are.
            self.jump_base = pos.z;
            self.peak = pos.z;
            self.state = State::Act(a);
            return;
        }
        let y = self.jump_base + up;
        self.peak = self.peak.max(pos.z);
        // Coming down onto something.
        let descending = def.motion_at(a.f)[1] < def.motion_at((a.f - 1.0).max(0.0))[1];
        if descending && standing {
            self.land(pos, inp, true);
            return;
        }
        let dt = 1.0 / ANIM_FPS;
        out.vz = Some(((y - pos.z) / dt).clamp(-20.0, 20.0));
        if a.f < def.total {
            self.state = State::Act(a);
            return;
        }
        let n = def.motion.len();
        let rise = if n >= 2 { (def.motion[n - 1][1] - def.motion[n - 2][1]) * ANIM_FPS } else { 0.0 };
        out.vz = Some(rise);
        self.state = State::Air(Air { vel: out.vel, jumped: true, f: 0.0 });
    }

    fn fall(&mut self, pos: Vec3, vel: Vec2) {
        self.sprinting = false;
        self.guarding = false;
        self.crouching = false;
        self.peak = pos.z;
        self.state = State::Air(Air { vel, jumped: false, f: 0.0 });
    }

    fn air(&mut self, mut a: Air, inp: Input, pos: Vec3, standing: bool, df: f32, out: &mut StepOut) {
        a.f += df;
        self.peak = self.peak.max(pos.z);
        out.vel = a.vel;
        self.try_air_attack(a.jumped);
        if standing && a.f > 1.0 {
            out.fall_damage = self.land(pos, inp, a.jumped && !a.falling());
        } else {
            self.state = State::Air(a);
        }
    }

    /// Starts a jump attack from a queued press, once per jump.
    fn try_air_attack(&mut self, allowed: bool) {
        if !allowed || self.air_attack.is_some() || self.stamina <= 0.0 {
            return;
        }
        let kind = match self.buffer {
            Some(Req::Light) => AirKind::Light,
            Some(Req::Heavy) => AirKind::Heavy,
            Some(Req::Left) if self.paired() => AirKind::Paired,
            _ => return,
        };
        self.buffer = None;
        let m = self.moveset();
        let key = if kind == AirKind::Paired { (m.weapon, false, kind) } else { (m.weapon, m.two_hand, kind) };
        let Some(def) = self.data.air.get(&key).cloned() else { return };
        self.spend(def.stamina);
        self.air_attack = Some(AirAttack { kind, moveset: m, def, f: 0.0, hit_done: false });
    }

    /// Returns fall damage (GTA health).
    fn land(&mut self, pos: Vec3, inp: Input, jumped: bool) -> f32 {
        let fall = self.peak - pos.z;
        let attack = self.air_attack.take();
        let mut damage = 0.0;
        if fall >= FALL_DEATH {
            return 1000.0;
        }
        if fall >= FALL_DAMAGE_START {
            let t = (fall - FALL_DAMAGE_START) / (FALL_DEATH - FALL_DAMAGE_START);
            damage = 100.0 * (0.3 + 0.6 * t);
        }
        if let Some(at) = attack {
            let (landing, short) = match at.kind {
                AirKind::Paired => ("PairedJumpLand", "PairedJumpLandShort"),
                AirKind::Heavy => ("JumpHeavyLand", "JumpHeavyLandShort"),
                AirKind::Light => ("JumpLightLand", "JumpLightLandShort"),
            };
            let finished = at.f >= at.def.to;
            let kind = if finished && self.has(at.moveset, short) { short } else { landing };
            let id = ActionId::Attack(at.moveset, kind.into());
            let Some(def) = self.def(&id).cloned() else {
                self.start(ActionId::Base("LandLight".into()));
                return damage;
            };
            self.start(id);
            if let State::Act(act) = &mut self.state {
                act.f = if finished { 0.0 } else { at.f.min(def.hits.first().map_or(0.0, |h| h.from)) };
                act.paid = u32::MAX;
                act.landed = if at.hit_done || finished { u32::MAX } else { 0 };
            }
        } else if fall >= FALL_HEAVY_LANDING {
            self.start(ActionId::Base("LandHeavy".into()));
        } else if !jumped {
            self.start(ActionId::Base("LandFall".into()));
        } else if let Some(w) = inp.wish() {
            if self.locked_on() && !self.sprint_held() {
                let side = self.square_up(heading_of(w), true);
                let walking = inp.walk || inp.tilt() < WALK_TILT;
                let n = if walking { "LandStrafeWalk" } else { "LandStrafe" };
                self.start(ActionId::Base(format!("{n}(Dir::{})", side.name())));
                return damage;
            }
            self.heading = heading_of(w);
            self.start(ActionId::Base(if self.sprint_held() { "LandSprint" } else { "LandRun" }.into()));
        } else {
            self.start(ActionId::Base("LandLight".into()));
        }
        damage
    }

    /// Fills `active_hit` with the blade of whatever swing is live.
    fn live_blade(&mut self, pos: Vec3, df: f32) {
        let attack_of = |m: Moveset| self.data.weapons.get(m.weapon).map_or(100.0, |w| w.attack);
        if let Some(at) = &self.air_attack {
            if at.hit_done || at.f < at.def.from || at.f >= at.def.to {
                return;
            }
            let landing = match at.kind {
                AirKind::Paired => "PairedJumpLand",
                AirKind::Heavy => "JumpHeavyLand",
                AirKind::Light => "JumpLightLand",
            };
            let hit = self.data.attacks.get(&(at.moveset.weapon, at.moveset.two_hand, landing.to_string())).and_then(|d| d.hits.first().cloned());
            let (mv, gd, stop) = hit.map_or((1.0, 1.0, 0.1), |h| (h.mv, h.guard_damage, h.stop));
            let sweep = self.sweep(pos, &at.def.blade, at.def.from, at.f, df);
            if !sweep.is_empty() {
                self.active_hit = Some(ActiveHit {
                    damage: attack_of(at.moveset) * mv * DAMAGE_SCALE,
                    poise: gd * 5.0,
                    stop,
                    radius: at.def.radius,
                    heavy: at.kind == AirKind::Heavy,
                    sweep,
                });
            }
            return;
        }
        let State::Act(a) = &self.state else { return };
        let ActionId::Attack(m, kind) = &a.id else { return };
        let Some(def) = self.def(&a.id) else { return };
        let Some((_, hit)) = def.hits.iter().enumerate().find(|(i, h)| a.f >= h.from && a.f < h.to && a.landed & (1 << i) == 0) else { return };
        let heavy = kind.contains("Heavy") || kind == "GuardCounter";
        let sweep = self.sweep(pos, &hit.blade, hit.from, a.f, df);
        if !sweep.is_empty() {
            self.active_hit = Some(ActiveHit {
                damage: attack_of(*m) * hit.mv * DAMAGE_SCALE,
                poise: hit.guard_damage * 5.0,
                stop: hit.stop,
                radius: hit.radius,
                heavy,
                sweep,
            });
        }
    }

    fn sweep(&self, pos: Vec3, blade: &[[f32; 6]], from: f32, f: f32, df: f32) -> Vec<(Vec3, Vec3)> {
        let start = (f - df).max(from);
        (0..SWEEP_STEPS)
            .filter_map(|s| {
                let at = start + (f - start) * s as f32 / (SWEEP_STEPS - 1) as f32;
                blade_at(blade, from, at).map(|(p, q)| (self.place(pos, p), self.place(pos, q)))
            })
            .collect()
    }

    /// Character-space [left, up, forward] → GTA world.
    fn place(&self, pos: Vec3, v: Vec3) -> Vec3 {
        let (f, l) = (self.fwd(), self.leftv());
        Vec3::new(pos.x + l.x * v.x + f.x * v.z, pos.y + l.y * v.x + f.y * v.z, pos.z - self.feet + v.y)
    }

    /// The live blade connected: hit-stop, and it can't hit again this swing.
    pub fn mark_hit(&mut self, stop: f32) {
        if let Some(a) = &mut self.air_attack {
            a.hit_done = true;
        } else if let State::Act(a) = &mut self.state {
            {
                let f = a.f;
                let def = match &a.id {
                    ActionId::Attack(m, k) => self.data.attacks.get(&(m.weapon, m.two_hand, k.clone())),
                    ActionId::Base(n) => self.data.base.get(n),
                };
                if let Some(i) = def.and_then(|d| d.hits.iter().position(|h| f >= h.from && f < h.to)) {
                    a.landed |= 1 << i;
                }
            }
        }
        self.hit_stop = stop * HIT_STOP_SCALE;
    }

    /// A hit taken from `from` (GTA world) worth `damage` GTA health. Returns whether the
    /// GTA damage still applies (false: dodged, jumped or blocked).
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
        // A grip change is knocked out of the hands.
        self.swap = None;
        if !self.airborne() {
            let level = hurt_level(damage);
            let side = if toward.length_squared() < 1e-6 { Dir::Front } else { Dir::of(self.heading, heading_of(toward)) };
            self.start(ActionId::Base(format!("Hurt(HurtLevel::{level}, Dir::{})", side.name())));
        }
        true
    }

    pub(crate) fn swap_def(&self) -> Option<(&Swap, &SwapDef)> {
        let s = self.swap.as_ref()?;
        Some((s, self.data.swaps.get(&s.kind)?))
    }
}

/// `DamageIn::piece` marking souls' own fall damage (GTA's is dropped in souls mode).
pub const OWN_FALL_PIECE: u8 = 99;

/// The ER hurt level for an amount of GTA damage.
pub(crate) fn hurt_level(damage: f32) -> &'static str {
    if damage < 10.0 {
        "Small"
    } else if damage < 25.0 {
        "Middle"
    } else if damage < 50.0 {
        "Large"
    } else {
        "Knockdown"
    }
}

// ------------------------------------------------------------------ helpers

/// The ends of a blade at `frame` from samples starting at `from` rounded down.
pub(crate) fn blade_at(blade: &[[f32; 6]], from: f32, frame: f32) -> Option<(Vec3, Vec3)> {
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

pub(crate) fn angle_diff(from: f32, to: f32) -> f32 {
    (to - from + PI).rem_euclid(std::f32::consts::TAU) - PI
}

pub(crate) fn turn_toward(h: f32, goal: f32, max: f32) -> f32 {
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
