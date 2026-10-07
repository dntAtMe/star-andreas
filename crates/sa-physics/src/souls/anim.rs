//! Souls animation: which clip the state plays, cross-fades, the upper-body layers
//! (one-handed carry, guard, grip / weapon swap) and the retargeting of the ER skeleton
//! onto the GTA ped skeleton.
//!
//! Retargeting: each mapped ER bone's model-space rotation change from its rest pose is
//! applied to the matching ped bone, after a fixed correction that turns the ped's
//! reference bone direction onto the ER rest direction (ER rests in an A-pose). The ped's
//! reference is its current GTA pose when retargeting starts: the DFF bind pose lies on
//! its back, GTA's anims stand it up through the root.

use glam::{Mat3, Quat, Vec2, Vec3};

use super::player::{ANIM_FPS, Air, Dir, Grip, Souls, State, angle_diff, heading_of};
use super::SoulsData;
use crate::anim::Clump;

/// Seconds for an arm layer to blend in or out.
const LAYER_BLEND: f32 = 0.12;
/// Coming to rest this soon after running plays the run's stop.
const STOP_WITHIN: f32 = 0.3;
const AIR_LOOP: &str = "a000_202040";
const FALL_START: &str = "a000_004000";
const FALL_START_FRAMES: f32 = 24.0;

/// A pose in ER model space: pelvis position and a rotation per exported bone.
pub(crate) type Pose = (Vec3, Vec<Quat>);

/// The animation layer state of one character.
#[derive(Clone, Debug, Default)]
pub(crate) struct AnimState {
    clip: String,
    from: Option<Pose>,
    shown: Option<Pose>,
    fade: f32,
    fade_len: f32,
    /// Stride phase 0..1 shared by the locomotion loops.
    phase: f32,
    /// Wall clock (frames) for the stance loops.
    clock: f32,
    guard: f32,
    carry: f32,
    swap: f32,
    /// A one-off clip while standing still: run stop, crouching down, standing up.
    rest: Option<(String, f32)>,
    ran: f32,
    ran_dir: u32,
    still: bool,
    crouched: bool,
    retarget: Option<Retarget>,
}

pub(crate) fn clip_name(cat: u8, id: u32) -> String {
    format!("a{cat:03}_{id:06}")
}

/// Samples a clip at a frame.
pub(crate) fn sample(data: &SoulsData, name: &str, frame: f32, looped: bool) -> Option<Pose> {
    let c = data.clips.get(name)?;
    let nb = data.bones.len();
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

const LEFT_ARM: [&str; 6] = ["L_Clavicle", "L_UpperArm", "L_Forearm", "L_Hand", "L_Weapon", "L_Finger1"];
const RIGHT_ARM: [&str; 6] = ["R_Clavicle", "R_UpperArm", "R_Forearm", "R_Hand", "R_Weapon", "R_Finger1"];

/// Takes `arms` from `layer`, re-seated on the chest of `pose`, blended by `w`.
fn overlay(data: &SoulsData, pose: &mut Pose, layer: &Pose, arms: &[&[&str]], w: f32) {
    if w <= 0.0 {
        return;
    }
    let Some(chest) = data.bones.iter().position(|b| b == "Spine2") else { return };
    let seat = pose.1[chest] * layer.1[chest].inverse();
    for name in arms.iter().flat_map(|a| a.iter()) {
        let Some(i) = data.bones.iter().position(|b| b == name) else { continue };
        let target = seat * layer.1[i];
        pose.1[i] = pose.1[i].slerp(target, w);
    }
}

impl Souls {
    fn stance(&self) -> u8 {
        let w = |i: usize| self.data.weapons.get(i).map_or([0, 0], |w| w.stance);
        match self.grip {
            Grip::OneHand => w(self.weapon)[0],
            Grip::TwoHandRight => w(self.weapon)[1],
            Grip::TwoHandLeft => w(self.left)[1],
        }
    }

    /// (clip, frame, loops) for the current state.
    fn playing(&mut self, dt: f32) -> (String, f32, bool) {
        if let Some(a) = &self.air_attack {
            return (a.def.source.clone(), a.f, false);
        }
        let d = self.data.clone();
        let stance = self.stance();
        let grounded = matches!(self.state, State::Ground);
        let still = grounded && self.speed < 0.05;
        let st = &mut self.anim;
        let running_from = if self.crouching { (d.crouch_walk_speed + d.crouch_run_speed) / 2.0 } else { (d.walk_speed + d.run_speed) / 2.0 };
        if grounded && self.speed > running_from {
            st.ran = 0.0;
        } else {
            st.ran += dt;
        }
        if !still {
            st.rest = None;
        } else if self.crouching != st.crouched {
            st.rest = Some((clip_name(0, if self.crouching { 390000 } else { 390001 }), 0.0));
        } else if !st.still && st.ran < STOP_WITHIN {
            let options = if self.crouching {
                [clip_name(0, 322100), clip_name(0, 322100)]
            } else {
                let own = if self.grip == Grip::OneHand { 0 } else { stance };
                [clip_name(own, 22100 + st.ran_dir), clip_name(0, 22100 + st.ran_dir)]
            };
            st.rest = options.into_iter().find(|n| d.clips.contains_key(n)).map(|n| (n, 0.0));
        }
        st.still = still;
        st.crouched = self.crouching;
        if let Some((name, frame)) = &mut st.rest {
            *frame += ANIM_FPS * dt;
            match d.clips.get(name) {
                Some(c) if *frame < (c.frames - 1) as f32 => return (name.clone(), *frame, false),
                _ => st.rest = None,
            }
        }
        match &self.state {
            State::Air(Air { jumped: false, f, .. }) if *f < FALL_START_FRAMES => (FALL_START.into(), *f, false),
            State::Air(_) => (AIR_LOOP.into(), st.clock, true),
            State::Act(a) => (self.def(&a.id).map(|d| d.source.clone()).unwrap_or_default(), a.f, false),
            State::Ground => {
                if self.speed < 0.05 {
                    return if self.crouching { (clip_name(0, 300000), st.clock, true) } else { (clip_name(stance, 0), st.clock, true) };
                }
                let fwd = Vec2::new(-self.heading.sin(), self.heading.cos());
                let left = Vec2::new(-fwd.y, fwd.x);
                let (along, across) = (self.move_dir.dot(fwd), self.move_dir.dot(left));
                let dir = if along.abs() >= across.abs() { (along < 0.0) as u32 } else { 2 + (across < 0.0) as u32 };
                st.ran_dir = dir;
                let (id, native) = if self.crouching && self.speed > (d.crouch_walk_speed + d.crouch_run_speed) / 2.0 {
                    (320100 + dir, d.crouch_run_speed)
                } else if self.crouching {
                    (320000 + dir, d.crouch_walk_speed)
                } else if self.sprinting && self.speed > d.run_speed {
                    (20200, d.sprint_speed)
                } else if self.speed > (d.walk_speed + d.run_speed) / 2.0 {
                    (20100 + dir, [d.run_speed, d.run_back_speed, d.run_side_speed, d.run_side_speed][dir as usize])
                } else {
                    (20000 + dir, d.walk_speed)
                };
                let mut clip = clip_name(stance, id);
                if self.grip == Grip::OneHand || self.crouching || !d.clips.contains_key(&clip) {
                    clip = clip_name(0, id);
                }
                let last = d.clips.get(&clip).map_or(1.0, |c| (c.frames - 1) as f32);
                st.phase = (st.phase + self.speed / native.max(0.1) * ANIM_FPS * dt / last).fract();
                (clip, st.phase * last, true)
            }
        }
    }

    /// Advances the animation and writes the retargeted pose onto the clump.
    pub fn animate(&mut self, clump: &mut Clump, dt: f32) {
        let data = self.data.clone();
        if self.anim.retarget.is_none() {
            self.anim.retarget = Retarget::new(&data, clump);
            if let Some(r) = &self.anim.retarget {
                self.feet = r.feet;
            }
        }
        let frozen = self.hit_stop > 0.0;
        let dt = if frozen { 0.0 } else { dt };
        self.anim.clock += dt * ANIM_FPS;
        let (name, frame, looped) = self.playing(dt);
        let Some(target) = sample(&data, &name, frame, looped) else { return };
        let st = &mut self.anim;
        if name != st.clip {
            st.clip = name;
            st.from = st.shown.clone();
            st.fade = 0.0;
            st.fade_len = data.clips.get(&st.clip).map_or(4.0, |c| c.blend.max(1.0)) / ANIM_FPS;
        }
        st.fade += dt;
        let t = if st.from.is_some() && st.fade_len > 0.0 { (st.fade / st.fade_len).clamp(0.0, 1.0) } else { 1.0 };
        let t = t * t * (3.0 - 2.0 * t);
        let mut pose = match &st.from {
            Some(f) if t < 1.0 => (f.0.lerp(target.0, t), f.1.iter().zip(&target.1).map(|(a, b)| a.slerp(*b, t)).collect()),
            _ => target,
        };

        // Upper-body layers.
        let two_handed = self.grip != Grip::OneHand;
        let stance = self.stance();
        let grounded = matches!(self.state, State::Ground);
        let raising = self.guarding || matches!(&self.state, State::Act(a) if matches!(&a.id, super::player::ActionId::Base(n) if n == "GuardHit"));
        let step = dt / LAYER_BLEND;
        let st = &mut self.anim;
        st.guard = (st.guard + if raising { step } else { -step }).clamp(0.0, 1.0);
        let stopping = st.rest.is_some() && !self.crouching;
        let carrying = !two_handed && stance != 0 && grounded && (self.speed >= 0.05 || stopping);
        st.carry = (st.carry + if carrying { step } else { -step }).clamp(0.0, 1.0);
        let clock = st.clock;
        if let Some(layer) = sample(&data, &clip_name(stance, 0), clock, true) {
            overlay(&data, &mut pose, &layer, &[&RIGHT_ARM], self.anim.carry);
        }
        let (guard_stance, arms): (u8, &[&[&str]]) = if two_handed {
            (stance, &[&LEFT_ARM, &RIGHT_ARM])
        } else {
            (data.weapons.get(data.specials().0).map_or(0, |w| w.stance[0]), &[&LEFT_ARM])
        };
        if let Some(layer) = sample(&data, &clip_name(guard_stance, 100), clock, true) {
            overlay(&data, &mut pose, &layer, arms, self.anim.guard);
        }
        match self.swap_def().map(|(s, d)| (s.f, d.clone())) {
            Some((f, def)) => {
                self.anim.swap = (self.anim.swap + step).min(1.0);
                let (clip, at, w) = if f < def.start_len {
                    (def.start.clone(), f, self.anim.swap)
                } else {
                    let t = ((f - def.start_len) / def.end_len.max(1.0)).clamp(0.0, 1.0);
                    (def.end.clone(), f - def.start_len, self.anim.swap * (1.0 - t * t * (3.0 - 2.0 * t)))
                };
                if let Some(layer) = sample(&data, &clip, at, false) {
                    overlay(&data, &mut pose, &layer, &[&LEFT_ARM, &RIGHT_ARM], w);
                }
            }
            None => self.anim.swap = 0.0,
        }
        self.anim.shown = Some(pose.clone());
        if let Some(rt) = &self.anim.retarget {
            rt.apply(clump, &pose);
        }
    }
}

/// The side a ground move goes relative to the heading (for the strafe loops).
#[allow(dead_code)]
pub(crate) fn move_side(h: f32, dir: Vec2) -> Dir {
    if angle_diff(h, heading_of(dir)).abs() <= std::f32::consts::FRAC_PI_4 { Dir::Front } else { Dir::of(h, heading_of(dir)) }
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

#[derive(Clone, Debug)]
pub(crate) struct Retarget {
    /// Per clump frame: the ER bone driving it and its rest correction `C · B`.
    driven: Vec<Option<(usize, Quat)>>,
    rest_inv: Vec<Quat>,
    /// The reference pose's locals (for the frames the ER skeleton does not drive).
    reference: Vec<(Quat, Vec3)>,
    pelvis_frame: usize,
    pelvis_ref: Vec3,
    er_pelvis_rest: Vec3,
    pub feet: f32,
}

impl Retarget {
    pub(crate) fn new(data: &SoulsData, clump: &Clump) -> Option<Self> {
        let n = clump.num_frames();
        let a = er_to_gta();
        let reference = clump.pose.clone();
        let mut m_rot = vec![Quat::IDENTITY; n];
        let mut m_pos = vec![Vec3::ZERO; n];
        for k in 0..n {
            let (lr, lp) = reference[k];
            match clump.parent(k) {
                Some(p) => {
                    m_rot[k] = m_rot[p] * lr;
                    m_pos[k] = m_pos[p] + m_rot[p] * lp;
                }
                None => {
                    m_rot[k] = lr;
                    m_pos[k] = lp;
                }
            }
        }
        let er = |name: &str| data.bones.iter().position(|b| b == name);
        let mut driven = vec![None; n];
        for &(eb, ec, tag, ctag) in MAP {
            let (Some(ei), Some(k)) = (er(eb), clump.frame_of_tag(tag)) else { continue };
            let c = match (er(ec), clump.frame_of_tag(ctag)) {
                (Some(eci), Some(kc)) if !ec.is_empty() && ctag != 0 => {
                    let ed = (a * (data.rest[eci].0 - data.rest[ei].0)).normalize_or_zero();
                    let cd = (m_pos[kc] - m_pos[k]).normalize_or_zero();
                    if ed == Vec3::ZERO || cd == Vec3::ZERO { Quat::IDENTITY } else { Quat::from_rotation_arc(cd, ed) }
                }
                _ => Quat::IDENTITY,
            };
            driven[k] = Some((ei, c * m_rot[k]));
        }
        let pelvis_frame = clump.frame_of_tag(1)?;
        let er_pelvis_rest = data.rest[er("Pelvis")?].0;
        let pelvis_ref = m_pos[pelvis_frame];
        Some(Self {
            driven,
            rest_inv: data.rest.iter().map(|r| r.1.inverse()).collect(),
            reference,
            pelvis_frame,
            pelvis_ref,
            er_pelvis_rest,
            feet: er_pelvis_rest.y - pelvis_ref.z,
        })
    }

    pub(crate) fn apply(&self, clump: &mut Clump, pose: &Pose) {
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
                    let delta = pose.1[ei] * self.rest_inv[ei];
                    let dg = Quat::from_mat3(&(a * Mat3::from_quat(delta) * a.transpose())).normalize();
                    dg * cb
                }
                None => prot * self.reference[k].0,
            };
            model[k] = m;
            let p = if k == self.pelvis_frame { self.pelvis_ref + a * (pose.0 - self.er_pelvis_rest) } else { ppos + prot * reset };
            pos_model[k] = p;
            let local_rot = prot.inverse() * m;
            let local_pos = if k == self.pelvis_frame { prot.inverse() * (p - ppos) } else { reset };
            clump.pose[k] = (local_rot.normalize(), local_pos);
        }
    }
}
