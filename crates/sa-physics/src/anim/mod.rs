//! The RenderWare anim blend engine as SA uses it (`RpAnimBlend`, `CAnimBlendAssociation`,
//! `CAnimBlendNode`, `CAnimManager`): associations with blend amounts and deltas, partial
//! (upper-body) layers, phase-locked movement cycles and root-motion extraction.
//!
//! A [`Clump`] is one skinned ped: its frames are the HAnim bones in hierarchy order, frame 0
//! being the root whose translation is extracted into [`Clump::velocity`] (the ped's anim
//! moving shift, ped+0x4D8).

mod groups;

use std::{collections::HashMap, f32::consts::FRAC_PI_2, f32::consts::PI, sync::Arc};

use glam::{Quat, Vec3, Vec4};
use sa_formats::ifp;

pub use groups::GROUPS;

/// Association flags (`CAnimBlendAssociation+0x2E`).
pub mod af {
    pub const PLAYING: u16 = 0x1;
    pub const LOOPED: u16 = 0x2;
    /// Deleted once the blend has faded to 0.
    pub const DELETE_BLENDED_OUT: u16 = 0x4;
    /// At the end of a non-looped anim: fade out over 0.25 s, then delete.
    pub const FADE_OUT_FINISHED: u16 = 0x8;
    pub const PARTIAL: u16 = 0x10;
    pub const MOVEMENT: u16 = 0x20;
    pub const EXTRACT_Y: u16 = 0x40;
    pub const EXTRACT_X: u16 = 0x80;
    pub const WALK: u16 = 0x100;
    /// Not counted in the partial sum on the root bone.
    pub const NO_ROOT_PARTIAL_SUM: u16 = 0x400;
    pub const IGNORE_ROOT_TRANSLATION: u16 = 0x2000;
    pub const FACIAL: u16 = 0x8000;
}

/// Static definition of an anim assoc group (`AnimAssocDefinition`).
pub struct GroupDef {
    pub id: usize,
    pub name: &'static str,
    /// `ped` = anim/ped.ifp, otherwise `<block>.ifp` in anim/anim.img.
    pub block: &'static str,
    pub first_id: i16,
    pub anims: &'static [(&'static str, u16)],
}

pub mod group {
    pub const DEFAULT: usize = 0;
    pub const PLAYER: usize = 54;
    pub const PLAYER_ROCKET: usize = 57;
    pub const PLAYER_2ARMED: usize = 60;
    pub const PLAYER_BBBAT: usize = 63;
    pub const PLAYER_CSAW: usize = 66;
}

/// Ids in the `default` group used by the ped code.
pub mod anim_id {
    pub const WALK: i16 = 0;
    pub const RUN: i16 = 1;
    pub const SPRINT: i16 = 2;
    pub const IDLE: i16 = 3;
    pub const WALK_START: i16 = 5;
    pub const IDLE_ARMED: i16 = 11;
    pub const GUN_STAND: i16 = 49;
    pub const GUNMOVE_FWD: i16 = 50;
    pub const GUNMOVE_L: i16 = 51;
    pub const GUNMOVE_BWD: i16 = 52;
    pub const GUNMOVE_R: i16 = 53;
    pub const GUN_2_IDLE: i16 = 54;
    pub const JUMP_LAUNCH: i16 = 116;
    pub const JUMP_GLIDE: i16 = 118;
    pub const JUMP_LAND: i16 = 119;
    pub const FALL_FALL: i16 = 120;
    pub const FALL_GLIDE: i16 = 121;
    pub const FALL_LAND: i16 = 122;
    /// Weapon groups: fire, crouch fire, reload, crouch reload.
    pub const WEAPON_FIRE: i16 = 224;
    pub const WEAPON_CROUCHFIRE: i16 = 225;
    pub const WEAPON_RELOAD: i16 = 226;
    pub const WEAPON_CROUCHRELOAD: i16 = 227;
}

// ------------------------------------------------------------------ hierarchies

#[derive(Debug, Clone, Copy)]
struct Key {
    q: Quat,
    t: Vec3,
    /// Time since the previous key (key 0: its absolute time).
    dt: f32,
}

#[derive(Debug, Clone)]
pub struct Sequence {
    pub tag: i32,
    pub name: String,
    has_trans: bool,
    keys: Vec<Key>,
}

/// `CAnimBlendHierarchy`: one animation.
#[derive(Debug, Clone)]
pub struct Hierarchy {
    pub name: String,
    pub total_length: f32,
    pub seqs: Vec<Sequence>,
}

impl Hierarchy {
    /// The load-time processing of 0x4CF4E0 / 0x4D0D40: quaternion flips removed
    /// (0x4D1190), `totalLength` = latest key, key times turned into deltas.
    pub fn from_ifp(a: &ifp::Animation) -> Self {
        let mut total = 0f32;
        let seqs = a
            .tracks
            .iter()
            .map(|t| {
                let has_trans = t.keys.iter().any(|k| k.pos.is_some());
                let mut keys: Vec<Key> = Vec::with_capacity(t.keys.len());
                let mut prev_q: Option<Quat> = None;
                let mut prev_time = 0.0;
                for k in &t.keys {
                    let mut q = Quat::from_array(k.rot);
                    if let Some(p) = prev_q {
                        if p.dot(q) < 0.0 {
                            q = -q;
                        }
                    }
                    prev_q = Some(q);
                    keys.push(Key { q, t: k.pos.map(Vec3::from).unwrap_or(Vec3::ZERO), dt: k.time - prev_time });
                    prev_time = k.time;
                }
                if let Some(k) = t.keys.last() {
                    total = total.max(k.time);
                }
                Sequence { tag: t.bone_id, name: t.bone_name.clone(), has_trans, keys }
            })
            .collect();
        Self { name: a.name.clone(), total_length: total, seqs }
    }
}

/// Group id → anims (`CAnimBlendAssocGroup`), each with its definition flags.
pub struct AnimGroup {
    pub def: &'static GroupDef,
    pub anims: Vec<Option<(Arc<Hierarchy>, u16)>>,
}

/// `CAnimManager`: every group of [`GROUPS`] whose IFP block could be loaded.
#[derive(Default)]
pub struct AnimManager {
    pub groups: HashMap<usize, AnimGroup>,
}

impl std::fmt::Debug for AnimManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AnimManager({} groups)", self.groups.len())
    }
}

impl AnimManager {
    /// `blocks(name)` returns the parsed IFP block (`ped` or an anim.img entry).
    pub fn load(mut blocks: impl FnMut(&str) -> Option<Vec<ifp::Animation>>) -> Self {
        let mut cache: HashMap<String, Option<HashMap<String, Arc<Hierarchy>>>> = HashMap::new();
        let mut groups = HashMap::new();
        for def in GROUPS {
            let block = cache.entry(def.block.to_ascii_lowercase()).or_insert_with(|| {
                blocks(def.block).map(|v| {
                    v.iter().map(|a| (a.name.to_ascii_lowercase(), Arc::new(Hierarchy::from_ifp(a)))).collect()
                })
            });
            let Some(block) = block else { continue };
            let anims = def
                .anims
                .iter()
                .map(|&(name, flags)| block.get(&name.to_ascii_lowercase()).map(|h| (h.clone(), flags)))
                .collect();
            groups.insert(def.id, AnimGroup { def, anims });
        }
        Self { groups }
    }

    pub fn get(&self, group: usize, id: i16) -> Option<&(Arc<Hierarchy>, u16)> {
        let g = self.groups.get(&group)?;
        g.anims.get((id - g.def.first_id) as usize)?.as_ref()
    }

    pub fn group_by_name(name: &str) -> Option<usize> {
        GROUPS.iter().find(|g| g.name.eq_ignore_ascii_case(name)).map(|g| g.id)
    }
}

// ------------------------------------------------------------------ associations

/// `CAnimBlendNode`: one association's playback state on one clump frame.
#[derive(Debug, Clone, Copy, Default)]
struct Node {
    seq: Option<u16>,
    /// The "next" key (`frameA`) and the previous one (`frameB`).
    frame_a: i16,
    frame_b: i16,
    /// Time left until key `frame_a`.
    remaining: f32,
    theta: f32,
    inv_sin: f32,
}

/// `CAnimBlendAssociation`.
#[derive(Debug, Clone)]
pub struct Assoc {
    pub hier: Arc<Hierarchy>,
    pub group: i16,
    pub id: i16,
    pub flags: u16,
    pub blend: f32,
    /// Per second.
    pub blend_delta: f32,
    pub time: f32,
    pub speed: f32,
    time_step: f32,
    nodes: Vec<Node>,
    /// Unique per clump: stands in for the association pointer tasks keep.
    pub uid: u32,
    /// A finish callback is set (`SetFinishCallback`): when the anim reaches its end the
    /// uid is reported in [`Clump::finished`] and the callback cleared.
    pub finish_cb: bool,
}

impl Assoc {
    pub fn has(&self, f: u16) -> bool {
        self.flags & f != 0
    }

    /// 0x4D1490.
    fn update_blend(&mut self, dt: f32) -> bool {
        self.blend += self.blend_delta * dt;
        if self.blend <= 0.0 && self.blend_delta < 0.0 {
            self.blend = 0.0;
            self.blend_delta = 0.0;
            if self.has(af::DELETE_BLENDED_OUT) {
                return false;
            }
        }
        if self.blend > 1.0 {
            self.blend = 1.0;
            if self.blend_delta > 0.0 {
                self.blend_delta = 0.0;
            }
        }
        true
    }

    /// 0x4D13D0. Returns true when the finish callback fired.
    fn update_time(&mut self) -> bool {
        if !self.has(af::PLAYING) {
            return false;
        }
        let len = self.hier.total_length;
        if self.time >= len {
            self.flags &= !af::PLAYING;
            return false;
        }
        self.time += self.time_step;
        if self.time >= len {
            if self.has(af::LOOPED) {
                self.time -= len;
                return false;
            }
            self.time = len;
            if self.has(af::FADE_OUT_FINISHED) {
                self.flags |= af::DELETE_BLENDED_OUT;
                self.blend_delta = -4.0;
            }
            if self.finish_cb {
                self.finish_cb = false;
                return true;
            }
        }
        false
    }

    pub fn set_playing(&mut self, on: bool) {
        if on {
            self.flags |= af::PLAYING;
        } else {
            self.flags &= !af::PLAYING;
        }
    }

    pub fn time_step(&self) -> f32 {
        self.time_step
    }

    /// `SetCurrentTime`: wrap/clamp, then `FindKeyFrame` on every node.
    pub fn set_current_time(&mut self, t: f32) {
        let len = self.hier.total_length;
        self.time = t;
        if t >= len {
            if self.has(af::LOOPED) && len > 0.0 {
                while self.time >= len {
                    self.time -= len;
                }
            } else {
                self.time = len;
            }
        }
        let looped = self.has(af::LOOPED);
        let time = self.time;
        for n in &mut self.nodes {
            if let Some(s) = n.seq {
                n.find_key_frame(&self.hier.seqs[s as usize], time, looped);
            }
        }
    }

    pub fn start(&mut self, t: f32) {
        self.flags |= af::PLAYING;
        self.set_current_time(t);
    }

    fn sync(&mut self, other_time: f32, other_len: f32) {
        let t = if other_len > 0.0 { other_time / other_len * self.hier.total_length } else { 0.0 };
        self.set_current_time(t);
    }

    pub fn is_finished(&self) -> bool {
        self.time >= self.hier.total_length
    }
}

impl Node {
    /// 0x4D0190.
    fn calc_deltas(&mut self, seq: &Sequence) {
        let a = seq.keys[self.frame_a as usize].q;
        let b = seq.keys[self.frame_b as usize].q;
        let c = b.dot(a).clamp(-1.0, 1.0);
        self.theta = c.acos();
        self.inv_sin = if self.theta == 0.0 { 0.0 } else { 1.0 / self.theta.sin() };
    }

    /// 0x4D04A0: returns true when the track wrapped.
    fn next_key_frame(&mut self, seq: &Sequence, looped_flag: bool) -> bool {
        let n = seq.keys.len() as i16;
        if n < 2 {
            return false;
        }
        self.frame_b = self.frame_a;
        let mut looped = false;
        while self.remaining <= 0.0 {
            self.frame_a += 1;
            if self.frame_a >= n {
                if !looped_flag {
                    self.frame_a -= 1;
                    self.remaining = 0.0;
                    return false;
                }
                self.frame_a = 0;
                looped = true;
            }
            self.remaining += seq.keys[self.frame_a as usize].dt;
        }
        self.frame_b = self.frame_a - 1;
        if self.frame_b < 0 {
            self.frame_b += n;
        }
        self.calc_deltas(seq);
        looped
    }

    /// 0x4D0240.
    fn find_key_frame(&mut self, seq: &Sequence, mut t: f32, looped_flag: bool) {
        let n = seq.keys.len() as i16;
        if n < 1 {
            return;
        }
        self.frame_a = 0;
        self.frame_b = 0;
        if n == 1 {
            self.remaining = 0.0;
            self.calc_deltas(seq);
            return;
        }
        self.frame_a = 1;
        while t > seq.keys[self.frame_a as usize].dt {
            t -= seq.keys[self.frame_a as usize].dt;
            if self.frame_a + 1 >= n {
                if !looped_flag {
                    self.calc_deltas(seq);
                    self.remaining = 0.0;
                    return;
                }
                self.frame_a = 0;
            }
            self.frame_b = self.frame_a;
            self.frame_a += 1;
        }
        self.remaining = seq.keys[self.frame_a as usize].dt - t;
        self.calc_deltas(seq);
    }

    fn lerp_s(&self, seq: &Sequence) -> f32 {
        let next = &seq.keys[self.frame_a as usize];
        if next.dt == 0.0 { 0.0 } else { (next.dt - self.remaining) / next.dt }
    }

    /// Translation at the current keys (`GetCurrentTranslation`), times `w`.
    fn current_translation(&self, seq: &Sequence, w: f32) -> Vec3 {
        if w <= 0.0 {
            return Vec3::ZERO;
        }
        let s = self.lerp_s(seq);
        let (a, b) = (seq.keys[self.frame_b as usize].t, seq.keys[self.frame_a as usize].t);
        w * (a + (b - a) * s)
    }

    /// Translation of the last key (`GetEndTranslation`), times `w`.
    fn end_translation(seq: &Sequence, w: f32) -> Vec3 {
        if w <= 0.0 { Vec3::ZERO } else { w * seq.keys.last().map(|k| k.t).unwrap_or(Vec3::ZERO) }
    }
}

/// 0x59C300.
fn slerp(q1: Quat, q2: Quat, theta: f32, inv_sin: f32, s: f32) -> Vec4 {
    if theta == 0.0 {
        return Vec4::from(q2);
    }
    let (w1, w2) = if theta <= FRAC_PI_2 {
        (((1.0 - s) * theta).sin() * inv_sin, (s * theta).sin() * inv_sin)
    } else {
        (((1.0 - s) * (PI - theta)).sin() * inv_sin, -(s * (PI - theta)).sin() * inv_sin)
    };
    Vec4::from(q1) * w1 + Vec4::from(q2) * w2
}

/// Per-association scalars the node update needs.
#[derive(Clone, Copy)]
struct AssocView {
    flags: u16,
    blend: f32,
    time_step: f32,
}

/// 0x4D06C0: advance the node's keys and return (weighted t, weighted q, looped).
fn node_update(node: &mut Node, seq: &Sequence, a: AssocView, weight: f32) -> (Vec3, Vec4, bool) {
    let mut looped = false;
    if a.flags & af::PLAYING != 0 {
        node.remaining -= a.time_step;
        if node.remaining <= 0.0 {
            looped = node.next_key_frame(seq, a.flags & af::LOOPED != 0);
        }
    }
    let mut w = a.blend;
    if a.flags & af::PARTIAL == 0 {
        w *= weight;
    }
    let (mut t, mut q) = (Vec3::ZERO, Vec4::ZERO);
    if w > 0.0 {
        let s = node.lerp_s(seq);
        let (next, prev) = (&seq.keys[node.frame_a as usize], &seq.keys[node.frame_b as usize]);
        if seq.has_trans {
            t = w * (prev.t + (next.t - prev.t) * s);
        }
        q = w * slerp(prev.q, next.q, node.theta, node.inv_sin, s);
    }
    (t, q, looped)
}

fn add_aligned(rot: &mut Vec4, q: Vec4) {
    if q.dot(*rot) < 0.0 {
        *rot -= q;
    } else {
        *rot += q;
    }
}

// ------------------------------------------------------------------ clump

/// The anim blend data of one skinned clump.
#[derive(Clone)]
pub struct Clump {
    /// Live associations, index 0 = most recently added (list head).
    pub assocs: Vec<Assoc>,
    /// Bone tag (HAnim node id) and name per frame; frame 0 is the root.
    tags: Vec<i32>,
    names: Vec<String>,
    /// Parent frame per frame (None for the root).
    parents: Vec<Option<usize>>,
    next_uid: u32,
    /// Uids whose finish callback fired in the last update.
    pub finished: Vec<u32>,
    /// Uids deleted (blended out) in the last update.
    pub deleted: Vec<u32>,
    /// Bind-pose local translation per frame (`AnimBlendFrameData.resetPos`).
    reset: Vec<Vec3>,
    /// Local (rotation, translation) per frame after the last update.
    pub pose: Vec<(Quat, Vec3)>,
    /// Root motion of the last update, anim-local (x right, y forward): the ped's
    /// anim moving shift.
    pub velocity: Vec3,
}

impl Clump {
    /// `bones`: (tag, name, bind rotation, bind translation, parent frame).
    pub fn new(bones: Vec<(i32, String, Quat, Vec3, Option<usize>)>) -> Self {
        let pose = bones.iter().map(|b| (b.2, b.3)).collect();
        Self {
            assocs: Vec::new(),
            tags: bones.iter().map(|b| b.0).collect(),
            names: bones.iter().map(|b| b.1.to_ascii_lowercase()).collect(),
            parents: bones.iter().map(|b| b.4).collect(),
            next_uid: 1,
            finished: Vec::new(),
            deleted: Vec::new(),
            reset: bones.iter().map(|b| b.3).collect(),
            pose,
            velocity: Vec3::ZERO,
        }
    }

    pub fn num_frames(&self) -> usize {
        self.tags.len()
    }

    /// `CAnimBlendAssocGroup::CopyAnimation`: a fresh association (blend 1, delta 0)
    /// with each sequence bound to its frame by bone tag, else by name.
    fn copy_animation(&self, man: &AnimManager, group: usize, id: i16) -> Option<Assoc> {
        let (hier, flags) = man.get(group, id)?;
        let mut nodes = vec![Node::default(); self.tags.len()];
        for (si, s) in hier.seqs.iter().enumerate() {
            let fi = self
                .tags
                .iter()
                .position(|&t| t == s.tag)
                .or_else(|| self.names.iter().position(|n| n.trim() == s.name.trim().to_ascii_lowercase()));
            if let Some(fi) = fi {
                if !s.keys.is_empty() {
                    nodes[fi].seq = Some(si as u16);
                }
            }
        }
        Some(Assoc {
            hier: hier.clone(),
            group: group as i16,
            id,
            flags: *flags,
            blend: 1.0,
            blend_delta: 0.0,
            time: 0.0,
            speed: 1.0,
            time_step: 0.0,
            nodes,
            uid: 0,
            finish_cb: false,
        })
    }

    /// `CAnimManager::AddAnimationAndSync` (0x4D3B30); returns the new association's index (0).
    fn add_and_sync(&mut self, mut a: Assoc, sync: Option<(f32, f32)>) -> usize {
        a.uid = self.next_uid;
        self.next_uid = self.next_uid.wrapping_add(1).max(1);
        match sync {
            Some((t, len)) if a.has(af::MOVEMENT) => {
                a.sync(t, len);
                a.flags |= af::PLAYING;
            }
            _ => a.start(0.0),
        }
        self.assocs.insert(0, a);
        0
    }

    /// `CAnimManager::AddAnimation` (0x4D3AA0): layered at blend 1, other assocs untouched.
    pub fn add_animation(&mut self, man: &AnimManager, group: usize, id: i16) -> Option<usize> {
        let a = self.copy_animation(man, group, id)?;
        let sync = if a.has(af::MOVEMENT) {
            self.assocs.iter().find(|m| m.has(af::MOVEMENT)).map(|m| (m.time, m.hier.total_length))
        } else {
            None
        };
        Some(self.add_and_sync(a, sync))
    }

    /// `CAnimManager::BlendAnimation` (0x4D4610).
    pub fn blend_animation(&mut self, man: &AnimManager, group: usize, id: i16, delta: f32) -> Option<usize> {
        let (_, st_flags) = man.get(group, id)?;
        let is_move = st_flags & af::MOVEMENT != 0;
        let partial = st_flags & af::PARTIAL;
        let facial = st_flags & af::FACIAL;
        let mut found = None;
        let mut sync = None;
        let mut removed = false;
        for (i, a) in self.assocs.iter_mut().enumerate() {
            if is_move && a.has(af::MOVEMENT) {
                sync = Some((a.time, a.hier.total_length));
            }
            if a.id == id && a.group == group as i16 {
                found = Some(i);
                continue;
            }
            if a.flags & af::PARTIAL != partial || a.flags & af::FACIAL != facial {
                continue;
            }
            if a.blend > 0.0 {
                let d = -delta * a.blend;
                if d < a.blend_delta || partial == 0 {
                    a.blend_delta = d.min(-0.05);
                }
            } else {
                a.blend_delta = -1.0;
            }
            a.flags |= af::DELETE_BLENDED_OUT;
            removed = true;
        }
        if let Some(i) = found {
            let a = &mut self.assocs[i];
            a.blend_delta = (1.0 - a.blend) * delta;
            if a.time == a.hier.total_length {
                a.start(0.0);
            }
            return Some(i);
        }
        let a = self.copy_animation(man, group, id)?;
        let i = self.add_and_sync(a, sync);
        let a = &mut self.assocs[i];
        if !removed && partial == 0 {
            a.blend = 1.0;
        } else {
            a.blend = 0.0;
            a.blend_delta = delta;
        }
        Some(i)
    }

    /// `RpAnimBlendClumpGetAssociation(clump, animId)`: newest with that id, any group.
    pub fn get(&self, id: i16) -> Option<&Assoc> {
        self.assocs.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: i16) -> Option<&mut Assoc> {
        self.assocs.iter_mut().find(|a| a.id == id)
    }

    pub fn index_of(&self, id: i16) -> Option<usize> {
        self.assocs.iter().position(|a| a.id == id)
    }

    /// `RpAnimBlendClumpUpdateAnimations` (0x4D34F0), on-screen path, skinned callbacks.
    /// `dt` = ts * 0.02 seconds.
    pub fn update(&mut self, dt: f32) {
        // Blends (deleting faded-out assocs), then the movement phase lock.
        self.finished.clear();
        self.deleted.clear();
        let mut keep = Vec::with_capacity(self.assocs.len());
        for mut a in self.assocs.drain(..) {
            if a.update_blend(dt) {
                keep.push(a);
            } else {
                self.deleted.push(a.uid);
            }
        }
        self.assocs = keep;
        let (mut sum_a, mut sum_b) = (0f32, 0f32);
        let mut found_non_movement = false;
        let mut live = Vec::new();
        for (i, a) in self.assocs.iter().enumerate() {
            if a.hier.seqs.is_empty() {
                continue;
            }
            if live.len() < 11 {
                live.push(i);
            }
            if a.has(af::MOVEMENT) {
                sum_a += a.hier.total_length / a.speed * a.blend;
                sum_b += a.blend;
            } else {
                found_non_movement = true;
            }
        }
        for a in &mut self.assocs {
            if !a.has(af::PLAYING) {
                continue;
            }
            let mv = a.has(af::MOVEMENT);
            a.time_step = if sum_a == 0.0 {
                (if mv { a.hier.total_length } else { a.speed }) * dt
            } else {
                (if mv { a.hier.total_length / sum_a * sum_b } else { a.speed }) * dt
            };
        }

        for fi in 0..self.tags.len() {
            if fi == 0 {
                self.update_root(&live, found_non_movement);
            } else {
                self.update_frame(fi, &live, found_non_movement);
            }
        }
        for a in &mut self.assocs {
            if a.update_time() {
                self.finished.push(a.uid);
            }
        }
    }

    /// Delete an association at once (`delete assoc`).
    pub fn delete(&mut self, uid: u32) {
        if let Some(i) = self.assocs.iter().position(|a| a.uid == uid) {
            self.assocs.remove(i);
            self.deleted.push(uid);
        }
    }

    pub fn by_uid(&self, uid: u32) -> Option<&Assoc> {
        self.assocs.iter().find(|a| a.uid == uid)
    }

    pub fn by_uid_mut(&mut self, uid: u32) -> Option<&mut Assoc> {
        self.assocs.iter_mut().find(|a| a.uid == uid)
    }

    pub fn frame_of_tag(&self, tag: i32) -> Option<usize> {
        self.tags.iter().position(|&t| t == tag)
    }

    pub fn parent(&self, frame: usize) -> Option<usize> {
        self.parents[frame]
    }

    /// Model-space matrix of a frame (the pose's LTM; the skeleton root's parent is the
    /// ped origin).
    pub fn ltm(&self, frame: usize) -> glam::Mat4 {
        let (q, t) = self.pose[frame];
        let local = glam::Mat4::from_rotation_translation(q, t);
        match self.parents[frame] {
            Some(p) => self.ltm(p) * local,
            None => local,
        }
    }

    fn view(&self, i: usize) -> AssocView {
        let a = &self.assocs[i];
        AssocView { flags: a.flags, blend: a.blend, time_step: a.time_step }
    }

    /// `FrameUpdateCallBackSkinned` (0x4D2B90).
    fn update_frame(&mut self, fi: usize, live: &[usize], found_non_movement: bool) {
        let mut p = 0.0;
        if found_non_movement {
            for &i in live {
                let a = &self.assocs[i];
                if a.nodes[fi].seq.is_some() && a.has(af::PARTIAL) {
                    p += a.blend;
                }
            }
        }
        let (mut pos, mut bt, mut rot) = (Vec3::ZERO, 0f32, Vec4::ZERO);
        let mut any = false;
        for &i in live {
            let v = self.view(i);
            let a = &mut self.assocs[i];
            let Some(s) = a.nodes[fi].seq else { continue };
            any = true;
            let seq = &a.hier.seqs[s as usize];
            let (t, q, _) = node_update(&mut a.nodes[fi], seq, v, 1.0 - p);
            if seq.has_trans {
                pos += t;
                bt += a.blend;
            }
            add_aligned(&mut rot, q);
        }
        if !any {
            return;
        }
        if rot.length_squared() > 0.0 {
            self.pose[fi].0 = Quat::from_vec4(rot.normalize());
        }
        self.pose[fi].1 = pos * bt + (1.0 - bt) * self.reset[fi];
    }

    /// Root with 2D velocity extraction (0x4D1680).
    fn update_root(&mut self, live: &[usize], found_non_movement: bool) {
        let fi = 0;
        let mut p = 0.0;
        if found_non_movement {
            for &i in live {
                let a = &self.assocs[i];
                if a.nodes[fi].seq.is_some() && a.has(af::PARTIAL) && !a.has(af::NO_ROOT_PARTIAL_SUM) {
                    p += a.blend;
                }
            }
        }
        let weight = 1.0 - p;
        let node_w = |a: &Assoc| if a.has(af::PARTIAL) { a.blend } else { a.blend * weight };
        let (mut prev, mut cur, mut end, mut total) = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
        let mut rot = Vec4::ZERO;
        let mut looped_any = false;
        let mut any = false;
        for &i in live {
            let a = &self.assocs[i];
            let Some(s) = a.nodes[fi].seq else { continue };
            let seq = &a.hier.seqs[s as usize];
            if seq.has_trans && !a.has(af::IGNORE_ROOT_TRANSLATION) && a.has(af::EXTRACT_Y) {
                let v = a.nodes[fi].current_translation(seq, node_w(a));
                prev.y += v.y;
                if a.has(af::EXTRACT_X) {
                    prev.x += v.x;
                }
            }
        }
        for &i in live {
            let v = self.view(i);
            let a = &mut self.assocs[i];
            let Some(s) = a.nodes[fi].seq else { continue };
            any = true;
            let w = if a.flags & af::PARTIAL != 0 { a.blend } else { a.blend * weight };
            let seq = &a.hier.seqs[s as usize];
            let (t, q, l) = node_update(&mut a.nodes[fi], seq, v, weight);
            add_aligned(&mut rot, q);
            if seq.has_trans && !a.has(af::IGNORE_ROOT_TRANSLATION) {
                total += t;
                if a.has(af::EXTRACT_Y) {
                    cur.y += t.y;
                    if a.has(af::EXTRACT_X) {
                        cur.x += t.x;
                    }
                    looped_any |= l;
                    if l {
                        let e = Node::end_translation(seq, w);
                        end.y += e.y;
                        if a.has(af::EXTRACT_X) {
                            end.x += e.x;
                        }
                    }
                }
            }
        }
        if !any {
            self.velocity = Vec3::ZERO;
            return;
        }
        if rot.length_squared() > 0.0 {
            self.pose[fi].0 = Quat::from_vec4(rot.normalize());
        }
        self.velocity.x = cur.x - prev.x;
        self.velocity.y = cur.y - prev.y;
        if looped_any {
            self.velocity.x += end.x;
            self.velocity.y += end.y;
        }
        let reset = self.reset[fi];
        let mut t = Vec3::new(total.x - cur.x, total.y - cur.y, total.z);
        if t.z >= -0.8 {
            t.z += (if t.z >= -0.4 { 1.0 } else { t.z * 2.5 + 2.0 }) * reset.z;
        }
        t.x += reset.x;
        t.y += reset.y;
        self.pose[fi].1 = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(name: &str, len_keys: usize, dx_per_key: f32, bones: &[i32]) -> ifp::Animation {
        ifp::Animation {
            name: name.into(),
            duration: (len_keys - 1) as f32 / 30.0,
            tracks: bones
                .iter()
                .map(|&b| ifp::Track {
                    bone_name: format!("b{b}"),
                    bone_id: b,
                    keys: (0..len_keys)
                        .map(|k| ifp::Key {
                            time: k as f32 / 30.0,
                            rot: Quat::from_rotation_z(k as f32 * 0.1).to_array(),
                            pos: (b == 0).then_some([0.0, k as f32 * dx_per_key, 0.0]),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn manager() -> AnimManager {
        AnimManager::load(|b| match b {
            "ped" => Some(vec![anim("walk_player", 31, 0.05, &[0, 1, 2]), anim("IDLE_STANCE", 2, 0.0, &[0, 1, 2])]),
            "colt45" => Some(vec![anim("colt45_fire", 10, 0.0, &[2])]),
            _ => None,
        })
    }

    fn clump() -> Clump {
        Clump::new(
            (0..3)
                .map(|i| (i, format!("b{i}"), Quat::IDENTITY, Vec3::new(0.0, 0.0, 1.0), (i > 0).then(|| i as usize - 1)))
                .collect(),
        )
    }

    #[test]
    fn walk_root_motion_matches_clip_speed() {
        let man = manager();
        let mut c = clump();
        c.blend_animation(&man, group::PLAYER, anim_id::WALK, 4.0).unwrap();
        // 30 steps of 1/30 s: one full cycle covers 1.5 m forward.
        let mut moved = 0.0;
        for _ in 0..30 {
            c.update(1.0 / 30.0);
            moved += c.velocity.y;
        }
        assert!((moved - 1.5).abs() < 1e-3, "{moved}");
        // Root stays at the reset position in x/y.
        assert!(c.pose[0].1.y.abs() < 1e-4);
    }

    #[test]
    fn partial_fire_only_touches_its_bones_and_fades_out() {
        let man = manager();
        let mut c = clump();
        c.blend_animation(&man, group::PLAYER, anim_id::IDLE, 4.0).unwrap();
        c.update(0.02);
        let mut base = c.clone();
        let colt = AnimManager::group_by_name("colt45").unwrap();
        c.blend_animation(&man, colt, anim_id::WEAPON_FIRE, 1000.0).unwrap();
        assert_eq!(c.assocs.len(), 2, "partial must not fade the base anim");
        c.update(0.02);
        c.update(0.02);
        base.update(0.02);
        base.update(0.02);
        assert!(c.pose[1].0.abs_diff_eq(base.pose[1].0, 1e-5), "bone 1 not in the fire anim");
        assert!(!c.pose[2].0.abs_diff_eq(base.pose[2].0, 1e-3), "bone 2 follows the fire anim");
        for _ in 0..60 {
            c.update(0.02);
        }
        assert_eq!(c.assocs.len(), 1, "fire anim deleted after finishing + fade");
    }
}
