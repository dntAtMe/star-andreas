//! NPC event responses (ped_events.md, ped_flee.md, ped_scanner.md, ped_avoid.md):
//! decision makers (the add-time roll), the events random peds react to (shot fired, whizzed
//! by, gun aimed at, potential get run over, seen panicked ped, dead ped, damage) and the
//! response tasks (smart flee 911/910, duck 415/427, react-to-gun-aimed-at 601 with hands up
//! 413 / cower 412, evasive step 502 / dive 504, shake fist 302).
//!
//! KillPedOnFoot (1000) with its melee child (1001: seek, then CTaskSimpleFightingControl)
//! lets an unarmed ped fight back (ped_combat.md).
//!
//! Not ported: acquaintances from ped.dat (no friend/enemy source columns beyond "same ped
//! type"), groups, inform friends / group (1700 / 1200) and the 300 look-at side effects,
//! the armed kill task (1002) and GiveWeaponAtStartOfFight, dragging targets out of cars,
//! FleeEntity 909, InvestigateDeadPed 600, the drive-away car responses, ambient speech.

use glam::{Vec2, Vec3};
use sa_formats::decision::Decision;

use crate::{
    anim::{AnimManager, Clump, af},
    damage::Rand,
    npc::{NpcIn, NpcState, Wander},
    ped::limit_radian_angle,
    pedtask::radian_angle_between_points,
    world::EntityId,
};

/// eEventType values used here.
pub mod ev {
    pub const DRAGGED_OUT_CAR: u8 = 7;
    pub const DAMAGE: u8 = 9;
    pub const DEAD_PED: u8 = 11;
    pub const POTENTIAL_GET_RUN_OVER: u8 = 12;
    pub const SHOT_FIRED: u8 = 15;
    pub const GUN_AIMED_AT: u8 = 31;
    pub const SHOT_FIRED_WHIZZED_BY: u8 = 49;
    pub const SEEN_PANICKED_PED: u8 = 65;
}

/// Group-0 anims of the responses.
pub mod ra {
    pub const WEAPON_CROUCH: i16 = 55;
    pub const GETUP: i16 = 112;
    pub const EV_STEP: i16 = 126;
    pub const EV_DIVE: i16 = 127;
    pub const HANDSUP: i16 = 142;
    pub const HANDS_COWER: i16 = 143;
    pub const SHAKE_FIST: i16 = 144;
}

/// `CDecisionMakerTypes`: PedEvent.txt and the loaded `.ped` decision makers.
#[derive(Debug, Clone)]
pub struct DecisionData {
    pub event_to_decision: [u8; 96],
    /// By the pedstats decision-maker index (0 GangMbr, 1 Cop, 2 R_Norm, 3 R_Tough, 4 R_Weak,
    /// 5 Fireman, 6 m_empty, 7 Indoors).
    pub dms: Vec<Vec<Decision>>,
    /// The RANDOM.ped template (index −1).
    pub random_ped: Vec<Decision>,
}

impl DecisionData {
    fn decision(&self, dm: i32, ty: u8) -> Decision {
        let d = self.event_to_decision.get(ty as usize).copied().unwrap_or(0) as usize;
        let table = if dm < 0 { Some(&self.random_ped) } else { self.dms.get(dm as usize) };
        table.and_then(|t| t.get(d)).copied().unwrap_or_default()
    }

    /// `CDecisionMakerTypes::MakeDecision` (0x606E70) → `CDecision::MakeDecision` (0x6040D0)
    /// → `Pick` (0x6007A0). One `rand()` unless the preferred task is a candidate.
    #[allow(clippy::too_many_arguments)]
    pub fn make_decision(&self, dm: i32, ty: u8, src_type: usize, in_car: bool, bans: [i32; 3], preferred: i32, rng: &mut Rand) -> i32 {
        let d = self.decision(dm, ty);
        let mut tasks = [200i32; 6];
        let mut cum = [0f32; 6];
        let mut n = 0;
        for i in 0..6 {
            let p = d.prob[i][src_type.min(3)];
            let t = d.task[i];
            if p > 0 && t != -1 && !bans.contains(&t) && d.flag[i][in_car as usize] {
                tasks[n] = t;
                cum[n] = p as f32;
                n += 1;
            }
        }
        if n == 0 {
            return 200;
        }
        for k in 1..6 {
            cum[k] += cum[k - 1];
        }
        let total = cum[5];
        for c in &mut cum {
            *c *= 1.0 / total;
        }
        if preferred != -1 && tasks.contains(&preferred) {
            return preferred;
        }
        let r = rng.next() as f32 * 3.051_850_9e-5;
        (0..6).find(|&k| r <= cum[k]).map_or(200, |k| tasks[k])
    }
}

/// The CEventPotentialGetRunOver response chosen by the handler (0x4C0BD0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunOverResp {
    /// CTaskComplexEvasiveStep (502): face −hit, EV_step.
    Step { hit: Vec2 },
    /// CTaskComplexEvasiveDiveAndGetUp (504): face +hit, EV_dive, get up.
    Dive { hit: Vec2 },
    /// CTaskSimpleHandsUp(3000) facing the car.
    HandsUp { face: f32 },
    /// CTaskSimpleShakeFist facing the car.
    ShakeFist { face: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    ShotFired { by: EntityId, start: Vec3, end: Vec3, no_sound: bool },
    WhizzedBy { by: EntityId, start: Vec3, end: Vec3, no_sound: bool },
    GunAimedAt { aimer: EntityId },
    GetRunOver { veh: EntityId, resp: Option<RunOverResp> },
    /// `fleer` and the source of its current event.
    SeenPanickedPed { fleer: EntityId, threat: Option<EntityId> },
    DeadPed { dead: EntityId },
    Damage { src: Option<EntityId> },
    /// CEventDraggedOutCar (bValid: never expires).
    DraggedOutCar { jacker: EntityId, veh: EntityId, was_driver: bool },
}

impl EventKind {
    pub fn ty(&self) -> u8 {
        match self {
            Self::ShotFired { .. } => ev::SHOT_FIRED,
            Self::WhizzedBy { .. } => ev::SHOT_FIRED_WHIZZED_BY,
            Self::GunAimedAt { .. } => ev::GUN_AIMED_AT,
            Self::GetRunOver { .. } => ev::POTENTIAL_GET_RUN_OVER,
            Self::SeenPanickedPed { .. } => ev::SEEN_PANICKED_PED,
            Self::DeadPed { .. } => ev::DEAD_PED,
            Self::Damage { .. } => ev::DAMAGE,
            Self::DraggedOutCar { .. } => ev::DRAGGED_OUT_CAR,
        }
    }

    /// `GetEventPriority`.
    pub fn priority(&self) -> u8 {
        match self {
            Self::ShotFired { .. } => 35,
            Self::WhizzedBy { .. } => 36,
            Self::GunAimedAt { .. } => 50,
            Self::GetRunOver { .. } => 51,
            Self::SeenPanickedPed { .. } => 13,
            Self::DeadPed { .. } => 15,
            Self::Damage { .. } => 65,
            Self::DraggedOutCar { .. } => 40,
        }
    }

    /// `GetSourceEntity`.
    pub fn source(&self) -> Option<EntityId> {
        match *self {
            Self::ShotFired { by, .. } | Self::WhizzedBy { by, .. } => Some(by),
            Self::GunAimedAt { aimer } => Some(aimer),
            Self::GetRunOver { veh, .. } => Some(veh),
            Self::SeenPanickedPed { fleer, .. } => Some(fleer),
            Self::DeadPed { dead } => Some(dead),
            Self::Damage { src } => src,
            Self::DraggedOutCar { jacker, .. } => Some(jacker),
        }
    }

    /// `IsTemporaryEvent` (0x4BC370).
    pub fn temporary(&self) -> bool {
        matches!(self, Self::GetRunOver { .. })
    }
}

/// An event stored in a ped's event group with its rolled response task (`+0x0E`).
#[derive(Debug, Clone, PartialEq)]
pub struct PedEvent {
    pub kind: EventKind,
    pub task: i32,
    /// The source's position when the event was added (the flee point).
    pub src_pos: Option<Vec3>,
    /// `CanFightBack(ped, source)` (0x4BC3E0) when it was added.
    pub can_fight: bool,
}

/// `CGeneral::GetNodeHeadingFromVector` (0x53CDC0): the wander octant of a direction.
pub fn node_heading_from_vector(x: f32, y: f32) -> u8 {
    use std::f32::consts::{PI, TAU};
    let mut a = radian_angle_between_points(x, y, 0.0, 0.0);
    if a < 0.0 {
        a += TAU;
    }
    a = TAU - a + PI / 8.0;
    if a >= TAU {
        a -= TAU;
    }
    ((a * (1.0 / TAU) * 8.0).floor() as i32).clamp(0, 7) as u8
}

/// `CGeneral::GetRandomNumberInRange(int, int)`.
pub fn rand_range(rng: &mut Rand, a: i32, b: i32) -> i32 {
    a + ((rng.next() & 0xFFFF) as f32 * 3.051_757_8e-5 * (b - a) as f32) as i32
}

/// CTaskComplexSmartFleeEntity (911) with its CTaskComplexSmartFleePoint (910) sub-task.
#[derive(Debug, Clone)]
pub struct SmartFlee {
    pub threat: EntityId,
    /// 911 +0x10: the threat position at the last re-target (also 910's flee point).
    pub threat_pos: Vec3,
    /// 910 +0x0C: the ped's position when the flee started.
    start: Vec3,
    safe_dist: f32,
    /// `Say(347)` while fleeing (no audio in the port).
    pub scream: bool,
    /// +0x30: 7 sprint (4 walk for some handlers).
    pub move_state: u8,
    flee_until: u64,
    shift_ms: u32,
    shift_dist: f32,
    /// 911 re-target timer start.
    shift_at: u32,
    new_target: bool,
    dir: u8,
    wander: Option<Wander>,
    started: bool,
}

impl SmartFlee {
    /// `CTaskComplexSmartFleeEntity(threat, scream, safeDist, fleeTime, shiftTime, shiftDist)`.
    pub fn new(threat: EntityId, threat_pos: Vec3, scream: bool, safe_dist: f32, now: u32) -> Self {
        Self {
            threat,
            threat_pos,
            start: Vec3::ZERO,
            safe_dist,
            scream,
            move_state: 7,
            flee_until: now as u64 + 1_000_000,
            shift_ms: 1000,
            shift_dist: 1.0,
            shift_at: now,
            new_target: false,
            dir: 0,
            wander: None,
            started: false,
        }
    }
}

/// CTaskComplexReactToGunAimedAt (601).
#[derive(Debug, Clone)]
pub enum AimedAt {
    /// 902 AchieveHeading toward the aimer.
    Heading,
    /// 413 CTaskSimpleHandsUp(3000..5000).
    HandsUp { until: u32, anim: Option<u32> },
    /// 412 CTaskSimpleCower.
    Cower { anim: Option<u32> },
    /// 912 walk away from the aimer sensibly for 10 s.
    WalkAway { wander: Wander, until: u32 },
}

/// The active event response (task-manager slot 1 or 2).
#[derive(Debug, Clone)]
pub enum Resp {
    SmartFlee(SmartFlee),
    /// CTaskSimpleDuck(0, duration).
    Duck { until: u32, anim: Option<u32> },
    AimedAt { aimer: EntityId, stage: AimedAt },
    /// Standalone 413 / 412.
    HandsUp { until: u32, anim: Option<u32>, face: Option<f32> },
    Cower { anim: Option<u32> },
    ShakeFist { face: f32, anim: Option<u32> },
    EvasiveStep { heading: f32, anim: Option<u32> },
    /// stage 0 turn, 1 dive, 2 get up.
    EvasiveDive { heading: f32, stage: u8, anim: Option<u32> },
    /// CTaskComplexKillPedOnFoot (1000) → KillPedOnFootMelee (1001).
    KillPedOnFoot(KillPedOnFoot),
    /// ComputeDraggedOutCarResponse's sequences: GESTURE (pause 500..1500 ms, or turn to the
    /// dragger and give the finger), then `then`.
    Gesture { dragger: EntityId, pause_until: Option<u32>, anim: Option<u32>, then: Box<Resp> },
    /// 702 CTaskComplexEnterCarAsDriverTimed (requested from the world).
    EnterCar { veh: EntityId },
}

/// CTaskComplexKillPedOnFoot (1000) with the melee child: 907 CTaskComplexSeekEntity (run to
/// 1 m) or 1019 CTaskSimpleFightingControl (the CTaskSimpleFight driver).
#[derive(Debug, Clone)]
pub struct KillPedOnFoot {
    pub target: EntityId,
    /// 1019 is running (else 907 seek).
    pub fighting: bool,
    /// FightingControl +0x1C / +0x20.
    next_attack: u32,
    block_left: u32,
    /// 1002 when the ped holds a gun.
    pub armed: Option<crate::armed::Armed>,
}

impl KillPedOnFoot {
    pub fn new(target: EntityId) -> Self {
        Self { target, fighting: false, next_attack: 0, block_left: 0, armed: None }
    }
}

impl Resp {
    /// The entity whose position the response follows.
    pub fn threat(&self) -> Option<EntityId> {
        match self {
            Resp::SmartFlee(f) => Some(f.threat),
            Resp::KillPedOnFoot(k) => Some(k.target),
            Resp::Gesture { dragger, .. } => Some(*dragger),
            Resp::AimedAt { aimer, stage: AimedAt::WalkAway { .. } | AimedAt::HandsUp { .. } | AimedAt::Cower { .. } | AimedAt::Heading } => Some(*aimer),
            _ => None,
        }
    }
}

/// What the world tells a responding NPC each frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct RespIn {
    /// The response threat's current position (None when it is gone).
    pub threat_pos: Option<Vec3>,
    /// The threat is a living ped / lying on the ground / (wanted level, health) if the player.
    pub threat_alive: bool,
    pub threat_is_ped: bool,
    pub threat_down: bool,
    pub threat_wanted_hp: Option<(i32, f32)>,
    /// The threat sits in a vehicle / is in CTaskSimpleFall (FallAndGetUp's fall).
    pub threat_in_vehicle: bool,
    pub threat_falling: bool,
    /// IsTargetVisible (the world's cached line of sight), the target's spine and speed.
    pub threat_visible: bool,
    pub threat_aim: Option<Vec3>,
    pub threat_move_speed: Vec3,
    /// The ped's pedstats shooting rate (+0x30, read as a signed byte for GUN_PANIC).
    pub shooting_rate: u16,
}

fn anim_done(clump: &Clump, uid: Option<u32>) -> bool {
    uid.is_none_or(|u| clump.finished.contains(&u) || clump.by_uid(u).is_none_or(|a| a.is_finished()))
}

fn blend(clump: &mut Clump, m: &AnimManager, id: i16, delta: f32, finish_cb: bool) -> Option<u32> {
    clump.blend_animation(m, 0, id, delta).map(|i| {
        let a = &mut clump.assocs[i];
        a.finish_cb = finish_cb;
        a.uid
    })
}

fn fade_out(clump: &mut Clump, uid: Option<u32>) {
    if let Some(a) = uid.and_then(|u| clump.by_uid_mut(u)) {
        a.flags |= af::DELETE_BLENDED_OUT;
        a.blend_delta = -4.0;
    }
}

/// `CTaskSimpleAchieveHeading` (tolerance 0.2): true once the heading is reached.
fn achieve_heading(h: f32, aim_rot: &mut f32, cur_rot: f32) -> bool {
    *aim_rot = h;
    limit_radian_angle(h - cur_rot).abs() < 0.2
}

/// Per-frame ped data for the responses.
pub struct PedNow<'a> {
    pub pos: Vec3,
    pub move_speed: Vec3,
    pub aim_rot: &'a mut f32,
    pub cur_rot: f32,
}

impl NpcState {
    /// Response from the handler of the picked event (`ComputeEventResponseTask`).
    pub fn compute_response(&mut self, e: &PedEvent, now: u32) -> Option<Resp> {
        let (src_pos, can_fight) = (e.src_pos, e.can_fight);
        let flee = |by: EntityId, scream: bool, safe: f32| src_pos.map(|p| Resp::SmartFlee(SmartFlee::new(by, p, scream, safe, now)));
        let duck = |ms: u32| Some(Resp::Duck { until: now + ms, anim: None });
        match (&e.kind, e.task) {
            // 0x4BC710 shot fired (whizzed-by uses the same responses [I]).
            (EventKind::ShotFired { by, .. } | EventKind::WhizzedBy { by, .. }, t) => match t {
                415 => duck(3000),
                427 => duck(38527),
                911 => flee(*by, true, 60.0),
                1000 if !can_fight => flee(*by, false, 60.0),
                1000 => Some(Resp::KillPedOnFoot(KillPedOnFoot::new(*by))),
                _ => None,
            },
            // 0x4C2840 gun aimed at.
            (EventKind::GunAimedAt { aimer }, t) => {
                if matches!(self.response, Some(Resp::SmartFlee(_))) {
                    return flee(*aimer, false, 60.0);
                }
                match t {
                    412 => Some(Resp::Cower { anim: None }),
                    413 => Some(Resp::HandsUp { until: now + 5000, anim: None, face: None }),
                    415 => duck(5000),
                    427 => duck(38527),
                    601 => Some(Resp::AimedAt { aimer: *aimer, stage: AimedAt::Heading }),
                    911 => flee(*aimer, false, 60.0),
                    1000 if !can_fight => flee(*aimer, false, 60.0),
                    1000 => Some(Resp::KillPedOnFoot(KillPedOnFoot::new(*aimer))),
                    _ => None,
                }
            }
            // 0x4C0BD0 potential get run over (the choice is made by the world).
            (EventKind::GetRunOver { resp, .. }, _) => match *resp {
                Some(RunOverResp::Step { hit }) => {
                    Some(Resp::EvasiveStep { heading: radian_angle_between_points(-hit.x, -hit.y, 0.0, 0.0), anim: None })
                }
                Some(RunOverResp::Dive { hit }) => {
                    Some(Resp::EvasiveDive { heading: radian_angle_between_points(hit.x, hit.y, 0.0, 0.0), stage: 0, anim: None })
                }
                Some(RunOverResp::HandsUp { face }) => Some(Resp::HandsUp { until: now + 3000, anim: None, face: Some(face) }),
                Some(RunOverResp::ShakeFist { face }) => Some(Resp::ShakeFist { face, anim: None }),
                None => None,
            },
            // 0x4C35F0 seen panicked ped: flee the original threat.
            (EventKind::SeenPanickedPed { threat, .. }, t) => match t {
                427 => duck(57599),
                911 => threat.and_then(|th| flee(th, true, 45.0)),
                _ => None,
            },
            // 0x4B9470 dead ped.
            (EventKind::DeadPed { dead }, t) => match t {
                427 => duck(57599),
                911 => flee(*dead, true, 60.0),
                _ => None,
            },
            // ComputeDraggedOutCarResponse (0x4BCC30).
            (EventKind::DraggedOutCar { jacker, veh, was_driver }, t) => {
                let gesture = |rng: &mut Rand, then: Resp| {
                    let pause = (rng.next() & 0x3FF) < 0x201;
                    let pause_until = pause.then(|| now + rand_range(rng, 500, 1500) as u32);
                    Resp::Gesture { dragger: *jacker, pause_until, anim: None, then: Box::new(then) }
                };
                match t {
                    911 => flee(*jacker, false, 60.0),
                    1000 if !can_fight => flee(*jacker, false, 60.0),
                    1000 => Some(gesture(&mut self.rng, Resp::KillPedOnFoot(KillPedOnFoot::new(*jacker)))),
                    702 if *was_driver => Some(gesture(&mut self.rng, Resp::EnterCar { veh: *veh })),
                    _ => None,
                }
            }
            // ComputeAttackResponse (0x4BF9B0).
            (EventKind::Damage { src }, t) => match (t, src) {
                (911, Some(s)) => flee(*s, false, 60.0),
                (1000, Some(s)) if !can_fight => flee(*s, false, 60.0),
                (1000, Some(s)) => Some(Resp::KillPedOnFoot(KillPedOnFoot::new(*s))),
                (415, _) => duck(rand_range(&mut self.rng, 2000, 5000) as u32),
                (427, _) => duck(0xE0FF),
                _ => None,
            },
        }
    }

    /// `CEventHandler::HandleEvents`: pick the highest-priority stored event (ties: the later)
    /// and respond when it takes priority over the current event.
    pub fn pick_event(&mut self) -> Option<PedEvent> {
        let mut best: Option<PedEvent> = None;
        for e in self.events.drain(..) {
            if best.as_ref().is_none_or(|b| e.kind.priority() >= b.kind.priority()) {
                best = Some(e);
            }
        }
        let e = best?;
        let cur = self.cur_event.as_ref().map(|c| c.kind.priority());
        (cur.is_none_or(|p| e.kind.priority() >= p)).then_some(e)
    }

    /// Start a response: temporary ones park the current non-temporary response.
    pub fn start_response(&mut self, e: PedEvent, r: Resp, clump: &mut Clump) {
        self.end_response_anims(clump);
        // The world updates the threat position from the next frame on.
        self.resp_in.threat_pos = e.src_pos;
        self.resp_in.threat_alive = true;
        if e.kind.temporary() {
            if let (Some(cur), Some(ce)) = (self.response.take(), self.cur_event.take()) {
                if !ce.kind.temporary() {
                    self.parked = Some((cur, ce));
                }
            }
        } else {
            self.parked = None;
        }
        self.cur_event = Some(e);
        self.response = Some(r);
        // The DEFAULT wander was asked to abort: its sub-task ends.
        if let Some(w) = self.wander.as_mut() {
            w.sub = None;
        }
    }

    fn end_response_anims(&mut self, clump: &mut Clump) {
        let uid = match &self.response {
            Some(Resp::Duck { anim, .. } | Resp::HandsUp { anim, .. } | Resp::Cower { anim } | Resp::ShakeFist { anim, .. }) => *anim,
            Some(Resp::AimedAt { stage: AimedAt::HandsUp { anim, .. } | AimedAt::Cower { anim }, .. }) => *anim,
            _ => None,
        };
        fade_out(clump, uid);
    }

    /// The response ended (the slot empties): restore a parked flee, or back to the wander.
    fn finish_response(&mut self, clump: &mut Clump) {
        self.end_response_anims(clump);
        self.response = None;
        self.cur_event = None;
        if let Some((r, e)) = self.parked.take() {
            self.response = Some(r);
            self.cur_event = Some(e);
        }
        // Force SetMoveAnim to blend the move anim again.
        self.last_move_state = 0;
    }

    /// Run the active response for one frame. Returns false when no response is active.
    pub fn process_response(
        &mut self,
        me: &mut PedNow,
        clump: &mut Clump,
        m: &AnimManager,
        tasks: &mut crate::pedtask::PedTasks,
        ri: &RespIn,
        i: &NpcIn,
    ) -> bool {
        let Some(mut r) = self.response.take() else { return false };
        let now = i.now_ms;
        let mut done = false;
        match &mut r {
            Resp::SmartFlee(f) => {
                done = self.smart_flee(f, me, ri, i);
            }
            Resp::KillPedOnFoot(k) => {
                done = self.kill_ped_on_foot(k, me, clump, m, tasks, ri, i);
            }
            Resp::Gesture { dragger: _, pause_until, anim, then } => {
                self.move_state = 1;
                match pause_until {
                    // CTaskSimplePause.
                    Some(t) => {
                        if now >= *t {
                            let next = std::mem::replace(&mut **then, Resp::Cower { anim: None });
                            self.response = Some(next);
                            return true;
                        }
                    }
                    // AchieveHeading(0.5, 0.2) to the dragger, then FlipOff (FUCKU).
                    None => {
                        let face = ri.threat_pos.map(|tp| limit_radian_angle(radian_angle_between_points(tp.x, tp.y, me.pos.x, me.pos.y)));
                        if anim.is_none() {
                            if face.is_none_or(|h| achieve_heading(h, me.aim_rot, me.cur_rot)) {
                                *anim = blend(clump, m, ra::SHAKE_FIST, 4.0, true);
                                self.last_move_state = 1;
                            }
                        } else if anim_done(clump, *anim) {
                            let next = std::mem::replace(&mut **then, Resp::Cower { anim: None });
                            self.last_move_state = 0;
                            self.response = Some(next);
                            return true;
                        }
                    }
                }
            }
            Resp::EnterCar { veh } => {
                self.enter_request = Some(*veh);
                done = true;
            }
            Resp::Duck { until, anim } => {
                self.move_state = 1;
                if anim.is_none() {
                    *anim = blend(clump, m, ra::WEAPON_CROUCH, 4.0, false);
                    self.last_move_state = 1;
                }
                done = now >= *until;
            }
            Resp::HandsUp { until, anim, face } => {
                self.move_state = 1;
                if let Some(h) = *face {
                    *me.aim_rot = h;
                }
                if anim.is_none() {
                    *anim = blend(clump, m, ra::HANDSUP, 4.0, false);
                    self.last_move_state = 1;
                }
                done = now >= *until;
            }
            Resp::Cower { anim } => {
                self.move_state = 1;
                if anim.is_none() {
                    *anim = blend(clump, m, ra::HANDS_COWER, 4.0, true);
                    self.last_move_state = 1;
                } else {
                    done = anim_done(clump, *anim);
                }
            }
            Resp::ShakeFist { face, anim } => {
                self.move_state = 1;
                *me.aim_rot = *face;
                if anim.is_none() {
                    *anim = blend(clump, m, ra::SHAKE_FIST, 4.0, true);
                    self.last_move_state = 1;
                } else {
                    done = anim_done(clump, *anim);
                }
            }
            Resp::EvasiveStep { heading, anim } => {
                self.move_state = 1;
                if anim.is_none() {
                    if achieve_heading(*heading, me.aim_rot, me.cur_rot) {
                        *anim = blend(clump, m, ra::EV_STEP, 8.0, true);
                        if let Some(a) = anim.and_then(|u| clump.by_uid_mut(u)) {
                            a.flags &= !af::DELETE_BLENDED_OUT;
                        }
                        self.last_move_state = 1;
                    }
                } else {
                    done = anim_done(clump, *anim);
                }
            }
            Resp::EvasiveDive { heading, stage, anim } => {
                self.move_state = 1;
                match *stage {
                    0 => {
                        if achieve_heading(*heading, me.aim_rot, me.cur_rot) {
                            *anim = blend(clump, m, ra::EV_DIVE, 8.0, true);
                            self.last_move_state = 1;
                            *stage = 1;
                        }
                    }
                    1 => {
                        if anim_done(clump, *anim) {
                            // Pause(0), then heading −= π/2 and CTaskSimpleGetUp.
                            let h = limit_radian_angle(me.cur_rot) - std::f32::consts::FRAC_PI_2;
                            *me.aim_rot = h;
                            *anim = blend(clump, m, ra::GETUP, 4.0, true);
                            *stage = 2;
                        }
                    }
                    _ => {
                        if anim_done(clump, *anim) {
                            fade_out(clump, *anim);
                            done = true;
                        }
                    }
                }
            }
            Resp::AimedAt { aimer, stage } => {
                let Some(ap) = ri.threat_pos else {
                    self.response = Some(r);
                    self.finish_response(clump);
                    return true;
                };
                match stage {
                    AimedAt::Heading => {
                        self.move_state = 1;
                        let h = limit_radian_angle(radian_angle_between_points(ap.x, ap.y, me.pos.x, me.pos.y));
                        if achieve_heading(h, me.aim_rot, me.cur_rot) {
                            *stage = if (ri.shooting_rate as u8 as i8) < 0 {
                                AimedAt::Cower { anim: blend(clump, m, ra::HANDS_COWER, 4.0, true) }
                            } else {
                                let ms = rand_range(&mut self.rng, 3000, 5000) as u32;
                                AimedAt::HandsUp { until: now + ms, anim: blend(clump, m, ra::HANDSUP, 4.0, false) }
                            };
                            self.last_move_state = 1;
                        }
                    }
                    AimedAt::HandsUp { until, anim } => {
                        self.move_state = 1;
                        if now >= *until {
                            fade_out(clump, *anim);
                            let d = node_heading_from_vector(me.pos.x - ap.x, me.pos.y - ap.y);
                            let mut w = Wander::new(d);
                            w.move_state = 4;
                            *stage = AimedAt::WalkAway { wander: w, until: now + 10000 };
                            self.last_move_state = 0;
                        }
                    }
                    AimedAt::Cower { anim } => {
                        self.move_state = 1;
                        if anim_done(clump, *anim) {
                            fade_out(clump, *anim);
                            self.last_move_state = 0;
                            r = Resp::SmartFlee(SmartFlee::new(*aimer, ap, false, 60.0, now));
                        }
                    }
                    AimedAt::WalkAway { wander, until } => {
                        if now >= *until {
                            done = true;
                        } else if let Some(paths) = self.paths.clone() {
                            let ms = me.pos;
                            self.process_wander(wander, &paths, ms, me.move_speed, me.aim_rot, me.cur_rot, i);
                        }
                    }
                }
            }
        }
        self.response = Some(r);
        if done {
            self.finish_response(clump);
        }
        true
    }

    /// 1000 Control (0x626260) with 1001 (0x62BE30 / 0x62BC10 / 0x626D90) and 1019
    /// (0x62A0A0). Returns true when the kill task ends.
    #[allow(clippy::too_many_arguments)]
    fn kill_ped_on_foot(
        &mut self,
        k: &mut KillPedOnFoot,
        me: &mut PedNow,
        clump: &mut Clump,
        m: &AnimManager,
        tasks: &mut crate::pedtask::PedTasks,
        ri: &RespIn,
        i: &NpcIn,
    ) -> bool {
        let now = i.now_ms;
        // Target gone or dead: the kill task ends.
        let Some(tp) = ri.threat_pos.filter(|_| ri.threat_alive) else {
            if let Some(mut f) = tasks.fight.take() {
                f.make_abortable(tasks, clump, m, false);
            }
            return true;
        };
        // 1000 Control: 1001 melee / 1002 armed by IsMelee(weapon).
        let melee = tasks.active_info().is_none_or(|w| w.fire_type == crate::weapon::fire::MELEE);
        if !melee {
            if let Some(mut f) = tasks.fight.take() {
                f.make_abortable(tasks, clump, m, false);
            }
            k.fighting = false;
            let mut a = k.armed.take().unwrap_or_default();
            let done = self.kill_ped_on_foot_armed(&mut a, k.target, me, clump, m, tasks, ri, i);
            k.armed = Some(a);
            return done;
        }
        if let Some(mut a) = k.armed.take() {
            a.abort(tasks);
            self.last_move_state = 0;
        }
        let d = tp - me.pos;
        let dist2 = d.length_squared();
        // UpdateTargetAndRange: the combo range (1.6 in every melee.dat entry).
        let range = tasks.melee.as_deref().map_or(1.6, |md| {
            let c = tasks.fight.as_ref().map_or(0, |f| (f.combo_set - 4).max(0));
            md.combo(c as i8 + 4).range
        });
        if !k.fighting {
            // 907 CTaskComplexSeekEntity: run (moveState 6) straight at the target, done at 1 m.
            self.move_state = 6;
            *me.aim_rot = limit_radian_angle(radian_angle_between_points(tp.x, tp.y, me.pos.x, me.pos.y));
            if dist2 <= 1.0 || dist2 < range * range {
                k.fighting = true;
                k.next_attack = 0;
            }
            return false;
        }
        // 1001 Control: past the give-up range (8 m) chase again.
        if dist2 > 8.0 * 8.0 {
            if let Some(mut f) = tasks.fight.take() {
                f.make_abortable(tasks, clump, m, false);
            }
            k.fighting = false;
            self.last_move_state = 0;
            return false;
        }
        // 1019 CTaskSimpleFightingControl.
        self.move_state = 1;
        self.last_move_state = 1;
        let rate = ri.shooting_rate as f32;
        let ms_step = (i.ts * 0.02 * 1000.0) as i32 as u32;
        let mut cmd: i8 = 0;
        if tasks.fight.is_none() {
            tasks.fight = Some(crate::melee::FightTask::new(Some(k.target), 0, 60000));
            k.next_attack = 0;
        } else if k.next_attack <= now {
            k.next_attack = 0;
            cmd = if tasks.fight_style != 4 && tasks.weapons[tasks.active_slot].ty == 0 { 12 } else { 11 };
        } else if ri.threat_is_ped && k.block_left == 0 {
            if self.rng.rand01() * 100.0 < 2.0 * rate * 0.025 {
                k.block_left = rand_range(&mut self.rng, 500, 2000) as u32;
                cmd = 2;
            }
        } else if k.block_left > 0 {
            k.block_left = k.block_left.saturating_sub(ms_step);
            cmd = 2;
        }
        if k.next_attack == 0 {
            let u = self.rng.next() as f32 * 3.051_850_9e-5;
            k.next_attack = ((u + 0.25) / (rate * 0.025 * 0.7 + 0.3) * 2000.0) as i32 as u32 + now;
        }
        let combo_set = tasks.fight.as_ref().map_or(0, |f| f.combo_set);
        if combo_set <= 1 {
            let mv = self.choose_movement(k, me, d, range, ri, ms_step);
            if mv > -1 {
                cmd = mv;
            }
        }
        // CTaskSimpleFight faces the target every frame.
        *me.aim_rot = limit_radian_angle(radian_angle_between_points(tp.x, tp.y, me.pos.x, me.pos.y));
        if let Some(f) = tasks.fight.as_mut() {
            f.ai_target_down = ri.threat_down;
            f.ai_target_wanted_hp = ri.threat_wanted_hp;
            f.control_fight(Some(k.target), cmd);
        }
        false
    }

    /// `CTaskSimpleFightingControl::ChooseMovement` (0x624B50) against a ped.
    fn choose_movement(&mut self, k: &mut KillPedOnFoot, me: &PedNow, d: Vec3, range: f32, ri: &RespIn, ms_step: u32) -> i8 {
        let dist = d.length();
        let a = limit_radian_angle((-d.x).atan2(d.y) - me.cur_rot);
        if a.abs() > 0.2618 {
            // Turning: idle and postpone the attack.
            k.next_attack = k.next_attack.wrapping_add(ms_step);
            return 0;
        }
        if !ri.threat_is_ped {
            return if dist - range > 0.3 { 3 } else { -1 };
        }
        let gap = dist - range;
        let r16 = self.rng.next() & 0xF == 0;
        if gap > 0.1 {
            3
        } else if gap > -0.1 {
            if r16 { 7 } else { -1 }
        } else if dist >= 0.8 {
            if self.rng.next() & 0x3F == 0 {
                8
            } else if self.rng.next() & 0x3F == 0 {
                10
            } else {
                -1
            }
        } else if r16 {
            9
        } else {
            -1
        }
    }

    /// 911 ControlSubTask (0x65C780) + 910 Create/Control (0x65C140 / 0x65C1E0).
    fn smart_flee(&mut self, f: &mut SmartFlee, me: &mut PedNow, ri: &RespIn, i: &NpcIn) -> bool {
        let now = i.now_ms;
        let Some(tp) = ri.threat_pos else { return true };
        if !f.started {
            f.started = true;
            f.start = me.pos;
            f.shift_at = now;
            f.dir = node_heading_from_vector(me.pos.x - f.threat_pos.x, me.pos.y - f.threat_pos.y);
            let mut w = Wander::new(f.dir);
            w.move_state = f.move_state;
            w.sensible = false;
            f.wander = Some(w);
        }
        // 911: re-target every shiftTime ms once the threat moved more than shiftDist.
        if now >= f.shift_at.wrapping_add(f.shift_ms) && (f.threat_pos - tp).length_squared() > f.shift_dist * f.shift_dist {
            f.shift_at = now;
            if f.threat_pos != tp {
                f.new_target = true;
            }
            f.threat_pos = tp;
            if f.move_state > 4 {
                self.raised.push(EventKind::SeenPanickedPed { fleer: EntityId::Body(u32::MAX), threat: Some(f.threat) });
            }
        }
        // 910.
        if f.new_target {
            f.new_target = false;
            f.flee_until = now as u64 + 1_000_000;
            let d = node_heading_from_vector(me.pos.x - f.threat_pos.x, me.pos.y - f.threat_pos.y);
            if d != f.dir {
                f.dir = d;
                if let Some(w) = f.wander.as_mut() {
                    w.dir = d;
                }
            }
        } else if now as u64 >= f.flee_until {
            return self.end_flee(f);
        } else {
            let r2 = f.safe_dist * f.safe_dist;
            if (f.threat_pos - me.pos).length_squared() > r2 && (f.start - me.pos).length_squared() > r2 {
                return self.end_flee(f);
            }
        }
        if let (Some(mut w), Some(paths)) = (f.wander.take(), self.paths.clone()) {
            w.move_state = f.move_state;
            self.process_wander(&mut w, &paths, me.pos, me.move_speed, me.aim_rot, me.cur_rot, i);
            f.wander = Some(w);
        }
        false
    }

    /// Stand still (0..49 ms), `HandOverPathToDefaultWander`, done (the look-around + tired
    /// sequence only plays for moveState 6).
    fn end_flee(&mut self, f: &mut SmartFlee) -> bool {
        if let (Some(fw), Some(dw)) = (f.wander.as_ref(), self.wander.as_mut()) {
            if fw.last != dw.last || fw.next != dw.next {
                dw.last = fw.last;
                dw.next = fw.next;
                dw.dir = fw.dir;
                dw.sub = None;
            }
        }
        true
    }
}


/// CTaskComplexPolicePursuit (0x44F) → CTaskComplexArrestPed (0x44D) for a cop on foot.
#[derive(Debug, Clone)]
pub struct Pursuit {
    pub target: EntityId,
    pub kill: KillPedOnFoot,
    /// CTaskSimpleArrestPed (0x44C): the ARRESTgun anim.
    arresting: Option<Option<u32>>,
}

impl Pursuit {
    pub fn new(target: EntityId) -> Self {
        Self { target, kill: KillPedOnFoot::new(target), arresting: None }
    }

    pub fn arresting(&self) -> bool {
        self.arresting.is_some()
    }
}

impl NpcState {
    /// The cop's pursuit (the DEFAULT slot's CTaskComplexWanderCop sub-task). Returns false
    /// when the ped has no pursuit.
    pub fn process_pursuit(
        &mut self,
        me: &mut PedNow,
        clump: &mut Clump,
        m: &AnimManager,
        tasks: &mut crate::pedtask::PedTasks,
        ri: &RespIn,
        i: &NpcIn,
    ) -> bool {
        let Some(mut pu) = self.pursuit.take() else { return false };
        // The world ends the pursuit; without a living target the cop just stands.
        let Some(tp) = ri.threat_pos.filter(|_| ri.threat_alive) else {
            if let Some(mut f) = tasks.fight.take() {
                f.make_abortable(tasks, clump, m, false);
            }
            self.move_state = 1;
            self.pursuit = Some(pu);
            return true;
        };
        if let Some(anim) = pu.arresting.as_mut() {
            // CTaskSimpleArrestPed::ProcessPed: face the target while the anim plays.
            self.move_state = 1;
            self.last_move_state = 1;
            *me.aim_rot = limit_radian_angle(radian_angle_between_points(tp.x, tp.y, me.pos.x, me.pos.y));
            if anim.is_none() {
                *anim = blend(clump, m, ARREST_GUN, 4.0, true);
                self.arrest_request = Some(pu.target);
            }
            self.pursuit = Some(pu);
            return true;
        }
        if ri.threat_in_vehicle {
            // 0x2D2 (arrest from the car) is not ported: stand still (0xCB).
            if let Some(mut f) = tasks.fight.take() {
                f.make_abortable(tasks, clump, m, false);
            }
            pu.kill.fighting = false;
            self.move_state = 1;
            self.pursuit = Some(pu);
            return true;
        }
        // ArrestPed CreateNextSubTask: the target is down (CTaskSimpleFall) within 2 m height
        // and 3 m flat → SetDownTime(100000), CTaskSimpleArrestPed.
        let d = tp - me.pos;
        if ri.threat_falling && d.z.abs() <= 2.0 && d.truncate().length_squared() < 3.0 * 3.0 {
            if let Some(mut f) = tasks.fight.take() {
                f.make_abortable(tasks, clump, m, true);
            }
            pu.arresting = Some(None);
            self.pursuit = Some(pu);
            return true;
        }
        // 0x3E8 CTaskComplexKillPedOnFoot (the melee child).
        let mut k = pu.kill.clone();
        self.kill_ped_on_foot(&mut k, me, clump, m, tasks, ri, i);
        pu.kill = k;
        self.pursuit = Some(pu);
        true
    }
}

/// `ARRESTgun` (group 0, 139).
const ARREST_GUN: i16 = 139;

/// `CPedGeometryAnalyser::ComputeEntityBoundingBoxCorners` (0x5F1FA0), normal path: the
/// entity's bbox grown by the 0.35 nav pad, flattened to a 2D box at height `z`.
pub fn entity_bbox_corners(m: &crate::physical::Matrix, bmin: Vec3, bmax: Vec3, z: f32) -> [Vec3; 4] {
    let pad = 0.35;
    let h = (bmax - bmin) * 0.5 + Vec3::splat(pad);
    let c = m.transform((bmax + bmin) * 0.5);
    let axes = [(m.right, h.x), (m.fwd, h.y), (m.up, h.z)];
    let w = |(v, e): (Vec3, f32)| 2.0 * e * v.truncate().length_squared();
    let (wr, wf, wu) = (w(axes[0]), w(axes[1]), w(axes[2]));
    let xi = if wr <= wf || wr <= wu {
        if wf <= wu { 2 } else { 1 }
    } else {
        0
    };
    let (x, hx) = axes[xi];
    let f = c + x * hx;
    let b = c - x * hx;
    let a = Vec2::new(x.x, x.y).normalize_or_zero();
    let (mut e, mut wd) = (0.0, 0.0);
    for (k, &(y, hy)) in axes.iter().enumerate() {
        if k == xi {
            continue;
        }
        let y2 = y.truncate();
        e += (a.dot(y2) * hy).abs();
        wd += (a.perp_dot(y2) * hy).abs();
    }
    let l = (a * e).extend(0.0);
    let pp = Vec3::new(wd * a.y, -wd * a.x, 0.0);
    let mut out = [f + l - pp, b - l - pp, b - l + pp, f + l + pp];
    for p in &mut out {
        p.z = z;
    }
    out
}

/// `ComputeEntityHitSide` (0x5F3730) with the edge planes of 0x5F1670.
pub fn entity_hit_side(pos: Vec3, c: &[Vec3; 4]) -> Vec2 {
    let plane = |i: usize| {
        let prev = c[(i + 3) % 4];
        let e = (c[i] - prev).truncate().normalize_or_zero();
        let n = Vec2::new(e.y, -e.x);
        (n, -n.dot(prev.truncate()))
    };
    let (n1, d1) = plane(1);
    let (n3, d3) = plane(3);
    let s1 = n1.dot(pos.truncate()) + d1;
    let s3 = n3.dot(pos.truncate()) + d3;
    if s1 > 0.0 {
        n1
    } else if s3 > 0.0 {
        n3
    } else if s3 < s1 {
        n1
    } else {
        n3
    }
}

/// A ped as the event code sees it.
#[derive(Debug, Clone, Copy)]
struct PedView {
    id: EntityId,
    pos: Vec3,
    fwd: Vec3,
    ped_type: u8,
    player: bool,
    alive: bool,
    npc: bool,
    in_601: bool,
    melee: bool,
}

fn npc_mut(w: &mut crate::world::World, id: EntityId) -> Option<&mut NpcState> {
    w.body_mut(id)?.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>()?.npc.as_mut()
}

fn npc_ref(w: &crate::world::World, id: EntityId) -> Option<&NpcState> {
    w.body(id)?.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?.npc.as_ref()
}

impl crate::world::World {
    fn ped_views(&self) -> Vec<PedView> {
        let infos = self.weapon_infos.clone();
        self.body_ids()
            .into_iter()
            .filter_map(|id| {
                let b = self.body(id)?;
                let p = b.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?;
                let t = &p.tasks;
                let w = t.weapons.get(t.active_slot).map_or(0, |w| w.ty);
                let melee = infos.as_ref().is_none_or(|i| i.get(w, 1).fire_type == 0);
                Some(PedView {
                    id,
                    pos: b.phys.matrix.pos,
                    fwd: b.phys.matrix.fwd,
                    ped_type: p.npc.as_ref().map_or(0, |n| n.ped_type),
                    player: p.is_player,
                    alive: t.health.alive() && p.vehicle.is_none(),
                    npc: p.npc.is_some(),
                    in_601: p.npc.as_ref().is_some_and(|n| matches!(n.response, Some(Resp::AimedAt { .. }))),
                    melee,
                })
            })
            .collect()
    }

    /// `CEvent::AffectsPed` of the events used here.
    fn event_affects(&mut self, k: &EventKind, p: &PedView, views: &[PedView]) -> bool {
        if !p.alive || p.player {
            return false;
        }
        let pos_of = |id: EntityId| views.iter().find(|v| v.id == id).map(|v| v.pos);
        match *k {
            // CEventGunShot::AffectsPed (0x4B2CD0): within 45 m of the shooter; a silenced
            // shot only with the muzzle in front and in sight.
            EventKind::ShotFired { by, start, no_sound, .. } => {
                if by == p.id {
                    return false;
                }
                let Some(sp) = pos_of(by).or_else(|| self.body(by).map(|b| b.phys.matrix.pos)) else { return false };
                if (p.pos - sp).length_squared() > 45.0 * 45.0 {
                    return false;
                }
                if !no_sound {
                    return true;
                }
                if (start - p.pos).dot(p.fwd) <= 0.0 {
                    return false;
                }
                let o = crate::world::LosOpts { ignore: Some(p.id), ignore2: Some(by), ..Default::default() };
                self.process_line_of_sight(start, p.pos, &o).is_none()
            }
            // CEventGunShotWhizzedBy::AffectsPed (0x4B5120): 2D within 2 m of the bullet line.
            EventKind::WhizzedBy { start, end, .. } => {
                let dir = (end - start).normalize_or_zero();
                let t = (p.pos - start).dot(dir);
                if t <= 0.0 {
                    return false;
                }
                let c = start + dir * t;
                (p.pos - c).truncate().length_squared() < 2.0 * 2.0
            }
            // CEventDeadPed::AffectsPed (0x4B4830).
            EventKind::DeadPed { dead } => {
                dead != p.id && pos_of(dead).is_some_and(|d| (d - p.pos).length_squared() < 20.0 * 20.0)
            }
            // CEventSeenPanickedPed::AffectsPed (0x4B53C0): within 10 m, no sight test.
            EventKind::SeenPanickedPed { fleer, threat } => {
                fleer != p.id && threat.is_some() && pos_of(fleer).is_some_and(|f| (f - p.pos).length_squared() < 100.0)
            }
            EventKind::GunAimedAt { .. } => !p.in_601,
            EventKind::GetRunOver { .. } | EventKind::Damage { .. } | EventKind::DraggedOutCar { .. } => true,
        }
    }

    /// `GetEventSourceType` (0x4ABAC0) without acquaintances: 2 same ped type (respected),
    /// 1 the player, 0 anything else.
    fn source_type(src: Option<EntityId>, p: &PedView, views: &[PedView]) -> usize {
        let Some(s) = src.and_then(|s| views.iter().find(|v| v.id == s)) else { return 0 };
        if !s.player && s.ped_type == p.ped_type {
            2
        } else if s.player {
            1
        } else {
            0
        }
    }

    /// `CEventGroup::Add` for an NPC: AffectsPed, the side-effect checks and the roll.
    fn add_ped_event(&mut self, dd: &DecisionData, p: &PedView, kind: EventKind, views: &[PedView]) {
        if !self.event_affects(&kind, p, views) {
            return;
        }
        let ty = kind.ty();
        let src = match kind {
            // CEventPotentialGetRunOver::GetSourceEntity: the driver.
            EventKind::GetRunOver { veh, .. } => self
                .body(veh)
                .filter(|b| b.phys.status == crate::physical::Status::Player)
                .and_then(|_| self.player_id()),
            _ => kind.source(),
        };
        let st = Self::source_type(src, p, views);
        let Some(dm) = npc_ref(self, p.id).map(|n| n.dm) else { return };
        let mut rng = std::mem::replace(&mut self.rng, Rand::new(1));
        for t in [1200, 1700, 300] {
            dd.make_decision(dm, ty, st, false, [-1; 3], t, &mut rng);
        }
        let task = dd.make_decision(dm, ty, st, false, [1700, 1200, 300], -1, &mut rng);
        self.rng = rng;
        if task == 200 {
            return;
        }
        let src_pos = match kind {
            EventKind::SeenPanickedPed { threat, .. } => threat.and_then(|t| self.body(t)).map(|b| b.phys.matrix.pos),
            _ => kind.source().and_then(|s| self.body(s)).map(|b| b.phys.matrix.pos),
        };
        // CanFightBack (0x4BC3E0): a melee ped only fights a melee attacker.
        let can_fight = !p.melee || src.and_then(|s| views.iter().find(|v| v.id == s)).is_some_and(|s| s.melee);
        if let Some(n) = npc_mut(self, p.id) {
            if n.events.len() < 16 {
                n.events.push(PedEvent { kind, task, src_pos, can_fight });
            }
        }
    }

    /// `CPlayerPed::Compute3rdPersonMouseTarget(bGun)` (0x60B650) while the player aims on
    /// foot (PC mouse mode): the living ped on the crosshair ray within the weapon's target
    /// range (guns) or straight out of the camera from the ped's plane (melee); peds only. It
    /// lingers 1 s and is cleared when the aim button is released.
    pub(crate) fn compute_mouse_target(&mut self) {
        let now = self.now_ms;
        let Some(pid) = self.player_id() else { return };
        let Some((aim, ty, skill, pos)) = self.body(pid).and_then(|b| {
            let p = b.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?;
            let ty = p.tasks.active_weapon().ty;
            Some((p.tasks.pad.aim && p.vehicle.is_none() && p.tasks.health.alive(), ty, p.tasks.weapon_skill(ty), b.phys.matrix.pos))
        }) else {
            return;
        };
        if !aim {
            self.mouse_target = None;
            return;
        }
        let Some(infos) = self.weapon_infos.clone() else { return };
        let info = infos.get(ty, skill);
        let range = info.target_range;
        let gun = info.fire_type != crate::weapon::fire::MELEE;
        let cam = self.cam_info();
        let (src, end) = if gun {
            cam.target_vector(range, pos)
        } else {
            let mut src = cam.pos;
            let k = (src - pos).dot(cam.front);
            if k < 0.0 {
                src -= cam.front * k;
            }
            (src, src + cam.front * range)
        };
        let o = crate::world::LosOpts { buildings: false, peds_only: true, ignore: Some(pid), ..Default::default() };
        let hit = self.process_line_of_sight(src, end, &o).map(|h| h.0).filter(|&e| {
            e != pid && self.body(e).and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>()).is_some_and(|p| p.tasks.health.alive())
        });
        if let Some(h) = hit {
            self.mouse_target = Some(h);
            self.mouse_target_until = now + 1000;
        } else if self.mouse_target.is_some() && self.mouse_target_until < now {
            self.mouse_target = None;
        }
    }

    /// The player's gun aimed at a ped (0x6860B4 free-aim path): a camera ray of the weapon
    /// range hitting a ped that can see the player.
    fn gun_aimed_at_target(&mut self, views: &[PedView]) -> Option<(EntityId, EntityId)> {
        let pid = self.player_id()?;
        let (aim, ty) = {
            let p = self.body(pid)?.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?;
            if p.vehicle.is_some() {
                return None;
            }
            (p.tasks.pad.aim, p.tasks.weapons.get(p.tasks.active_slot).map_or(0, |w| w.ty))
        };
        if !aim {
            return None;
        }
        let info = self.weapon_infos.as_ref()?.get(ty, 1).clone();
        if info.fire_type == 0 || info.flags & crate::weapon::wf::CANAIM == 0 {
            return None;
        }
        let cam = self.cam_info();
        let o = crate::world::LosOpts { ignore: Some(pid), ..Default::default() };
        let (hit, _, _) = self.process_line_of_sight(cam.pos, cam.pos + cam.front * info.weapon_range, &o)?;
        let t = views.iter().find(|v| v.id == hit && v.npc && v.alive)?;
        let pp = views.iter().find(|v| v.id == pid)?.pos;
        // IsInSeeingRange (0x600C60): within the seeing range and in front.
        let seeing = if (7..=16).contains(&t.ped_type) { 40.0 } else { 15.0 };
        ((pp - t.pos).length() < seeing && (pp - t.pos).dot(t.fwd) > 0.0).then_some((hit, pid))
    }

    /// `CCarCtrl::SlowCarDownForPedsSectorList` (0x425440), the run-over event part (B), and
    /// the event-12 handler's response choice (0x4C0BD0) for NPCs.
    fn run_over_events(&mut self, views: &[PedView]) -> Vec<(EntityId, EventKind)> {
        use crate::physical::{EntityType, Status};
        let player_veh = self.player_id().and_then(|p| {
            self.body(p)?.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?.vehicle.as_ref().map(|v| v.veh)
        });
        let mut out = Vec::new();
        let vehs: Vec<_> = self
            .body_ids()
            .into_iter()
            .filter_map(|id| {
                let b = self.body(id)?;
                (b.phys.kind == EntityType::Vehicle).then(|| (id, b.phys.matrix, b.phys.move_speed, b.phys.status, b.col.bbox_min, b.col.bbox_max))
            })
            .collect();
        for (vid, m, mv, status, bmin, bmax) in vehs {
            let is_player = Some(vid) == player_veh;
            if !(is_player || status == Status::Physics) {
                continue;
            }
            let fwd = m.fwd.dot(mv);
            if fwd == 0.0 || fwd.abs() <= 0.05 {
                continue;
            }
            let r = if is_player { 44.0 } else { 11.0 };
            let k50 = fwd.abs() * 50.0;
            for p in views.iter().filter(|p| p.npc && p.alive) {
                let d = p.pos - m.pos;
                if !(d.x.abs() < r && d.y.abs() < r) || d.z.abs() >= 6.0 {
                    continue;
                }
                let t = d.truncate().dot(m.fwd.truncate());
                if (p.pos.z - (m.pos.z + t * m.fwd.z)).abs() >= 3.0 {
                    continue;
                }
                let long = d.dot(m.fwd);
                if long.signum() != fwd.signum()
                    || long.abs() <= bmax.y
                    || long.abs() - bmax.y >= k50
                    || d.dot(m.right).abs() > bmax.x + 0.35
                {
                    continue;
                }
                // Handler 0x4C0BD0, NPC branch.
                let facing = (m.pos - p.pos).dot(p.fwd) > 0.0;
                let c = entity_bbox_corners(&m, bmin, bmax, p.pos.z);
                let hit = entity_hit_side(p.pos, &c);
                let pt = if fwd > 0.0 { (c[0] + c[3]) * 0.5 } else { (c[1] + c[2]) * 0.5 };
                let rel = p.pos - pt;
                let lng = rel.dot(m.fwd);
                let lat = rel.dot(m.right);
                // Far (more than 2 s away): only the honk reactions (no horn in the port).
                if lng.abs() >= (2.0 * fwd * 50.0).abs() {
                    continue;
                }
                let rnd = self.rng.rand01();
                let (margin, a, b, cc) = if fwd.abs() > 0.3 { (0.175, 0.05, 0.1, 0.2) } else { (0.7, 0.02, 0.05, 0.1) };
                let face = limit_radian_angle(radian_angle_between_points(m.pos.x, m.pos.y, p.pos.x, p.pos.y));
                let resp = if facing {
                    if lat.abs() > bmax.x - margin {
                        if rnd >= 0.5 * cc { RunOverResp::Step { hit } } else { RunOverResp::ShakeFist { face } }
                    } else if rnd >= cc {
                        RunOverResp::Dive { hit }
                    } else {
                        RunOverResp::HandsUp { face }
                    }
                } else if rnd > b {
                    continue; // horn off
                } else if rnd > a {
                    RunOverResp::Step { hit }
                } else {
                    RunOverResp::Dive { hit }
                };
                out.push((p.id, EventKind::GetRunOver { veh: vid, resp: Some(resp) }));
            }
        }
        out
    }

    /// `CWorld::Process`: hand the global events (and this frame's scanner / damage events)
    /// to the NPCs, update the responses' threat positions, then flush.
    pub(crate) fn process_ped_events(&mut self) {
        let globals = std::mem::take(&mut self.ped_events);
        let Some(dd) = self.decisions.clone() else { return };
        let views = self.ped_views();
        let mut local: Vec<(EntityId, EventKind)> = Vec::new();
        if let Some((t, aimer)) = self.gun_aimed_at_target(&views) {
            local.push((t, EventKind::GunAimedAt { aimer }));
        }
        local.extend(self.run_over_events(&views));
        let player = self.player_id();
        for v in views.iter().filter(|v| v.npc) {
            if let Some(src) = npc_mut(self, v.id).and_then(|n| n.damaged_by.take()) {
                // GenerateDamageEvent: CCrime 2 for the player's damage.
                if src.is_some() && src == player {
                    self.report_crime(2, Some(v.id), src);
                }
                local.push((v.id, EventKind::Damage { src }));
            }
            // CEventDraggedOutCar (added at the drag; answered after the get-up).
            if let Some((jacker, veh, was_driver)) = npc_mut(self, v.id).and_then(|n| n.dragged_out.take()) {
                local.push((v.id, EventKind::DraggedOutCar { jacker, veh, was_driver }));
            }
            if let Some(veh) = npc_mut(self, v.id).and_then(|n| n.enter_request.take()) {
                self.start_enter_car_timed(v.id, veh);
            }
            let due = npc_ref(self, v.id).and_then(|n| n.leave_and_flee).filter(|&(_, t)| self.now_ms >= t);
            if let Some((jacker, _)) = due {
                if self.start_leave_car(v.id) {
                    if let Some(n) = npc_mut(self, v.id) {
                        n.leave_and_flee = None;
                        n.flee_after_leave = Some(jacker);
                    }
                }
            }
            let run_over = self
                .body_mut(v.id)
                .and_then(|b| b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>())
                .map(|l| std::mem::take(&mut l.run_over_by_player))
                .unwrap_or(false);
            if run_over {
                self.report_crime(if v.ped_type == 6 { 11 } else { 10 }, Some(v.id), player);
            }
        }
        for v in views.iter().filter(|v| v.npc && v.alive) {
            for k in &globals {
                self.add_ped_event(&dd, v, k.clone(), &views);
            }
        }
        for (id, k) in local {
            if let Some(v) = views.iter().find(|v| v.id == id) {
                self.add_ped_event(&dd, v, k, &views);
            }
        }
        // The responses' threats: position, alive, lying down, the player's wanted level.
        let wanted = self.wanted.level;
        let now = self.now_ms;
        for v in views.iter().filter(|v| v.npc) {
            let threat = npc_ref(self, v.id)
                .and_then(|n| n.response.as_ref().and_then(|r| r.threat()).or_else(|| n.pursuit.as_ref().map(|p| p.target)));
            let (in_veh, falling) = {
                let tl = threat.and_then(|t| self.body(t)).and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>());
                (
                    tl.is_some_and(|l| l.vehicle.is_some()),
                    tl.is_some_and(|l| matches!(l.tasks.health.fall, Some(crate::peddamage::FallAndGetUp::Fall { .. }))),
                )
            };
            let (tp, alive, is_ped, down, wanted_hp) = {
                let tb = threat.and_then(|t| self.body(t));
                let tl = tb.and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>());
                (
                    tb.map(|b| b.phys.matrix.pos),
                    tl.is_none_or(|l| l.tasks.health.alive() && l.tasks.health.health > 0.0),
                    tl.is_some(),
                    tl.is_some_and(|l| l.tasks.health.fall.is_some() || l.knocked_down > 0.0),
                    tl.filter(|l| l.is_player).map(|l| (wanted, l.tasks.health.health)),
                )
            };
            // IsTargetVisible for the kill tasks (cached 10 s), the target's spine and speed.
            let killing = npc_ref(self, v.id).is_some_and(|n| matches!(n.response, Some(Resp::KillPedOnFoot(_))) || n.pursuit.is_some());
            let (visible, aim, speed) = match (threat, tp) {
                (Some(t), Some(tpos)) if killing && is_ped => {
                    let cache = npc_ref(self, v.id).map(|n| n.los).unwrap_or_default();
                    let visible = match cache.cached(now, v.pos, tpos) {
                        Some(b) => b,
                        None => {
                            let from = self.ped_bone_world(v.id, 5, Vec3::new(0.1, 0.0, 0.0)).unwrap_or(v.pos + Vec3::Z * 0.6);
                            let to = self.ped_bone_world(t, 5, Vec3::new(0.1, 0.0, 0.0)).unwrap_or(tpos + Vec3::Z * 0.6);
                            let veh = self.body(t).and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>()).and_then(|l| l.vehicle.as_ref().map(|v| v.veh));
                            let o = crate::world::LosOpts { peds: false, see_through: true, ignore: Some(v.id), ignore2: veh, ..Default::default() };
                            let clear = self.process_line_of_sight(from, to, &o).is_none();
                            if let Some(n) = npc_mut(self, v.id) {
                                n.los.store(now, clear, v.pos, tpos);
                            }
                            clear
                        }
                    };
                    (visible, self.ped_bone_world(t, 3, Vec3::ZERO), self.body(t).map_or(Vec3::ZERO, |b| b.phys.move_speed))
                }
                _ => (false, None, Vec3::ZERO),
            };
            if let Some(n) = npc_mut(self, v.id) {
                n.resp_in.threat_visible = visible;
                n.resp_in.threat_aim = aim;
                n.resp_in.threat_move_speed = speed;
                n.resp_in.threat_pos = tp;
                n.resp_in.threat_alive = alive;
                n.resp_in.threat_is_ped = is_ped;
                n.resp_in.threat_down = down;
                n.resp_in.threat_wanted_hp = wanted_hp;
                n.resp_in.threat_in_vehicle = in_veh;
                n.resp_in.threat_falling = falling;
            }
        }
        self.update_police(&views);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Car;
    impl crate::world::BodyLogic for Car {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn car_driving_at_a_ped_raises_an_evasive_response() {
        use crate::{
            collision::ColModel,
            physical::{EntityType, Matrix, Physical, Status},
            world::World,
        };
        let mut w = World::default();
        // A ped 6 m north of a car moving north at 0.2 units/frame (10 m/s).
        let mut pl = crate::ped::PedLogic::new(false, std::f32::consts::PI);
        pl.npc = Some(NpcState::new(1, 4, 1, 0, 0, std::sync::Arc::new(crate::paths::PathFind::default()), 0));
        let pm = Matrix { pos: Vec3::new(0.0, 6.0, 1.0), ..Matrix::IDENTITY };
        let mut pm2 = pm;
        crate::ped::set_heading(&mut pm2, std::f32::consts::PI); // facing south, at the car
        let ped = w.add_body(crate::ped::ped_physical(pm2), crate::ped::ped_col_model(), Box::new(pl));
        let mut car = Physical::new(EntityType::Vehicle, Matrix { pos: Vec3::new(0.0, 0.0, 1.0), ..Matrix::IDENTITY });
        car.status = Status::Physics;
        car.move_speed = Vec3::new(0.0, 0.2, 0.0);
        let col = ColModel { bbox_min: Vec3::new(-1.0, -2.2, -0.5), bbox_max: Vec3::new(1.0, 2.2, 0.8), ..Default::default() };
        w.add_body(car, col, Box::new(Car));
        let views = w.ped_views();
        let ev = w.run_over_events(&views);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].0, ped);
        // Squarely in the path and facing the car: a dive (or hands up 20 %).
        assert!(matches!(ev[0].1, EventKind::GetRunOver { resp: Some(RunOverResp::Dive { .. } | RunOverResp::HandsUp { .. }), .. }), "{ev:?}");
    }

    #[test]
    fn node_heading_octants() {
        // +Y is octant 0, +X 2, −Y 4, −X 6 (clockwise, as the path wander directions).
        assert_eq!(node_heading_from_vector(0.0, 1.0), 0);
        assert_eq!(node_heading_from_vector(1.0, 0.0), 2);
        assert_eq!(node_heading_from_vector(0.0, -1.0), 4);
        assert_eq!(node_heading_from_vector(-1.0, 0.0), 6);
    }

    #[test]
    fn roll_is_weighted_and_respects_bans() {
        let mut d = Decision::default();
        d.task[0] = 911;
        d.prob[0] = [50, 50, 5, 70];
        d.flag[0] = [true, false];
        d.task[1] = 300;
        d.prob[1] = [20, 20, 0, 0];
        d.flag[1] = [true, false];
        let mut table = vec![Decision::default(); sa_formats::decision::NUM_DECISIONS];
        let mut ev2 = [0u8; 96];
        ev2[15] = 6;
        table[6] = d;
        let data = DecisionData { event_to_decision: ev2, dms: vec![vec![]; 2].into_iter().chain([table]).collect(), random_ped: vec![] };
        let mut rng = Rand::new(3);
        // Player column: 300 banned → always 911.
        for _ in 0..20 {
            assert_eq!(data.make_decision(2, 15, 1, false, [1700, 1200, 300], -1, &mut rng), 911);
        }
        // In a car nothing is allowed.
        assert_eq!(data.make_decision(2, 15, 1, true, [1700, 1200, 300], -1, &mut rng), 200);
        // Preferred check.
        assert_eq!(data.make_decision(2, 15, 1, false, [-1; 3], 300, &mut rng), 300);
    }
}
