//! The IK chain system (`IKChainManager_c` / `IKChain_c` / `BoneNode_c`) and its tasks
//! (`CTaskSimpleIKManager`, `CTaskSimpleIKLookAt`, `CTaskSimpleIKPointArm`): one CCD pass per
//! frame from the effector up to the bone below the root, per-bone Euler limits from
//! `ms_boneInfos`, a persistent chain pose slerped onto the anim keyframes.
//!
//! Everything works in the ped's model space (the skeleton's space); targets come in world
//! space and are moved there with the ped matrix.

use glam::{Mat4, Quat, Vec3};

use crate::anim::Clump;

/// `ms_boneInfos` (0x8D26D0): tag, parent tag, centre Euler (deg), −X, +X, −Y, +Y, −Z, +Z.
#[rustfmt::skip]
const BONE_INFOS: [(i32, i32, [f32; 3], [f32; 6]); 32] = [
    (0, -1, [-0.2, 0.4, 87.3], [0.0; 6]),
    (1, 0, [-180.0, -88.5, 90.0], [0.0; 6]),
    (2, 1, [2.1, 0.0, -0.8], [30.0, 30.0, 20.0, 20.0, 5.0, 60.0]),
    (3, 2, [2.6, 0.4, 6.1], [10.0, 10.0, 10.0, 10.0, 15.0, 10.0]),
    (4, 3, [0.0, 0.0, 0.0], [0.0; 6]),
    (5, 4, [-1.8, -0.5, -7.3], [65.0, 65.0, 15.0, 15.0, 45.0, 45.0]),
    (6, 5, [-1.4, -9.0, 81.5], [0.0; 6]),
    (7, 5, [3.2, 6.7, 81.7], [0.0; 6]),
    (8, 5, [0.0, 0.0, 111.0], [0.0; 6]),
    (21, 4, [22.0, 80.6, -166.7], [0.0; 6]),
    (22, 21, [-5.4, -45.0, -13.0], [0.0, 0.0, 0.0, 115.0, 90.0, 45.0]),
    (23, 22, [0.0, 0.0, -13.6], [0.0, 0.0, 0.0, 0.0, 135.0, 13.0]),
    (24, 23, [113.4, -4.0, 6.5], [0.0; 6]),
    (25, 24, [0.0, 0.0, 19.5], [0.0; 6]),
    (26, 25, [0.0, 0.0, 13.3], [0.0; 6]),
    (31, 4, [-22.0, -80.6, -166.7], [0.0; 6]),
    (32, 31, [5.4, 45.6, -13.0], [0.0, 0.0, 115.0, 0.0, 90.0, 45.0]),
    (33, 32, [0.0, 0.0, -13.6], [0.0, 0.0, 0.0, 0.0, 135.0, 13.0]),
    (34, 33, [-113.4, 4.0, 6.5], [0.0; 6]),
    (35, 34, [0.0, 0.0, 19.5], [0.0; 6]),
    (36, 35, [0.0, 0.0, 13.3], [0.0; 6]),
    (41, 1, [176.0, -7.7, 165.9], [0.0, 0.0, 20.0, 5.0, 30.0, 0.0]),
    (42, 41, [0.0, 0.0, -24.4], [0.0, 0.0, 0.0, 0.0, 110.0, 0.0]),
    (43, 42, [-1.4, -5.0, 10.0], [0.0; 6]),
    (44, 43, [0.0, 0.0, 90.0], [0.0; 6]),
    (51, 1, [-176.0, 7.7, 165.9], [0.0, 0.0, 5.0, 20.0, 30.0, 0.0]),
    (52, 51, [0.0, 0.0, -24.4], [0.0, 0.0, 0.0, 0.0, 110.0, 0.0]),
    (53, 52, [1.4, 5.0, 10.0], [0.0; 6]),
    (54, 53, [0.0, 0.0, 90.0], [0.0; 6]),
    (302, 3, [15.1, -1.9, 81.0], [0.0; 6]),
    (301, 3, [-6.0, 2.0, 84.3], [0.0; 6]),
    (201, 2, [0.0, 0.0, 88.1], [0.0; 6]),
];

fn bone_info(tag: i32) -> &'static (i32, i32, [f32; 3], [f32; 6]) {
    // GetIdFromBoneTag: an unknown tag reads entry -1 in the exe; use the root instead.
    BONE_INFOS.iter().find(|b| b.0 == tag).unwrap_or(&BONE_INFOS[0])
}

/// `QuatToEuler` (0x617080), degrees, `q = qz·qy·qx`.
fn quat_to_euler(q: Quat) -> Vec3 {
    let (x, y, z, w) = (q.x, q.y, q.z, q.w);
    let m00 = 1.0 - 2.0 * y * y - 2.0 * z * z;
    let m01 = 2.0 * (x * y + z * w);
    let m02 = 2.0 * (x * z - y * w);
    let m12 = 2.0 * (y * z + x * w);
    let m22 = 1.0 - 2.0 * x * x - 2.0 * y * y;
    let sp = -m02;
    let c = (1.0 - sp * sp).max(0.0).sqrt();
    let k = 180.0 * 0.318_309_9;
    let ey = sp.atan2(c) * k;
    if sp == 1.0 || sp == -1.0 {
        let ex = (2.0 * (x * w - y * z)).atan2(1.0 - 2.0 * x * x - 2.0 * z * z) * k;
        Vec3::new(ex, ey, 0.0)
    } else {
        Vec3::new((m12 / c).atan2(m22 / c) * k, ey, (m01 / c).atan2(m00 / c) * k)
    }
}

/// `EulerToQuat` (0x6171F0), divided by the squared norm.
fn euler_to_quat(e: Vec3) -> Quat {
    let h = e * std::f32::consts::PI * (1.0 / 180.0) * 0.5;
    let (sx, cx) = h.x.sin_cos();
    let (sy, cy) = h.y.sin_cos();
    let (sz, cz) = h.z.sin_cos();
    let q = Quat::from_xyzw(
        sx * cy * cz - cx * sy * sz,
        cx * sy * cz + sx * cy * sz,
        cx * cy * sz - sx * sy * cz,
        cx * cy * cz + sx * sy * sz,
    );
    let n2 = q.length_squared();
    Quat::from_vec4(glam::Vec4::from(q) * (1.0 / n2))
}

/// `BoneNode_c`.
#[derive(Debug, Clone)]
struct BoneNode {
    tag: i32,
    /// Anim clump frame of this bone.
    frame: usize,
    /// Persistent IK local rotation / translation (seeded from the keyframe).
    q: Quat,
    t: Vec3,
    /// Index of the parent chain bone (None for the topmost: the chain root).
    parent: Option<usize>,
    world: Mat4,
    min: Vec3,
    max: Vec3,
}

impl BoneNode {
    /// `BoneNode_c::Limit` (0x617650).
    fn limit(&mut self) {
        let mut e = quat_to_euler(self.q);
        e.x = e.x.max(self.min.x).min(self.max.x);
        e.y = e.y.max(self.min.y).min(self.max.y);
        let (mut zmin, mut zmax) = (self.min.z, self.max.z);
        if self.tag == 5 {
            let c = bone_info(5).2[2];
            let f = (1.0 - e.x.abs() * 0.022_222_2).max(0.0);
            zmin = (zmin - c) * f + c;
            zmax = (zmax - c) * f + c;
        }
        e.z = e.z.max(zmin).min(zmax);
        self.q = euler_to_quat(e);
    }
}

/// `IKChain_c`.
#[derive(Debug, Clone)]
pub struct IkChain {
    /// [0] = effector bone, last = topmost bone below the root.
    bones: Vec<BoneNode>,
    root_frame: usize,
    effector_offset: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub blend: f32,
    pub bucket: u8,
}

impl IkChain {
    /// `IKChain_c::Init` + `SetupBones` (0x618370 / 0x617CA0).
    fn new(clump: &Clump, eff_tag: i32, eff_offset: Vec3, root_tag: i32, target: Vec3, speed: f32, bucket: u8) -> Option<Self> {
        let root_frame = clump.frame_of_tag(root_tag)?;
        let eff_frame = clump.frame_of_tag(eff_tag)?;
        if clump.pose[eff_frame].1 == Vec3::ZERO {
            return None;
        }
        let mut bones = Vec::new();
        let mut tag = eff_tag;
        while tag != root_tag && bones.len() < 32 {
            let frame = clump.frame_of_tag(tag)?;
            let info = bone_info(tag);
            let c = Vec3::from(info.2);
            let r = info.3;
            bones.push(BoneNode {
                tag,
                frame,
                q: clump.pose[frame].0,
                t: clump.pose[frame].1,
                parent: None,
                world: Mat4::IDENTITY,
                min: c - Vec3::new(r[0], r[2], r[4]),
                max: c + Vec3::new(r[1], r[3], r[5]),
            });
            tag = info.1;
            if tag < 0 {
                return None;
            }
        }
        let n = bones.len();
        for i in 0..n {
            let p = bone_info(bones[i].tag).1;
            bones[i].parent = (0..n).find(|&j| bones[j].tag == p);
        }
        Some(Self { bones, root_frame, effector_offset: eff_offset, target, speed, blend: 0.0, bucket })
    }

    /// `CalcWldMat` from the top: every bone's model-space matrix from its persistent pose.
    fn calc_world(&mut self, root: Mat4) {
        for i in (0..self.bones.len()).rev() {
            let parent = self.bones[i].parent.map_or(root, |p| self.bones[p].world);
            let b = &mut self.bones[i];
            b.world = parent * Mat4::from_rotation_translation(b.q, b.t);
        }
    }

    /// `IKChain_c::Update` (0x6184B0): one CCD pass (`MoveBonesToTarget` 0x6178B0) then
    /// `BlendKeyframe` (0x616E30) into the clump pose. `target` is in model space.
    fn update(&mut self, clump: &mut Clump, target: Vec3) {
        if self.bones.is_empty() {
            return;
        }
        let root = clump.ltm(self.root_frame);
        self.calc_world(root);
        for i in 0..self.bones.len() {
            let bone_pos = self.bones[i].world.w_axis.truncate();
            let eff_pos = self.bones[0].world.transform_point3(self.effector_offset);
            let v1 = eff_pos - bone_pos;
            if v1.length() <= 1e-5 {
                continue;
            }
            let v2 = target - bone_pos;
            if v2.length() <= 1e-5 {
                continue;
            }
            let (v1, v2) = (v1.normalize(), v2.normalize());
            let d = v1.dot(v2);
            if d >= 0.997 {
                continue;
            }
            let angle = d.clamp(-1.0, 1.0).acos() * self.speed;
            let axis = v1.cross(v2);
            if axis.length_squared() < 1e-12 {
                continue;
            }
            let parent = self.bones[i].parent.map_or(root, |p| self.bones[p].world);
            let (_, prot, _) = parent.to_scale_rotation_translation();
            let local_axis = (prot.inverse() * axis).normalize();
            let b = &mut self.bones[i];
            b.q = Quat::from_axis_angle(local_axis, angle) * b.q;
            b.limit();
            self.calc_world(root);
        }
        let t = self.blend;
        for b in &self.bones {
            let anim_q = clump.pose[b.frame].0;
            if t >= 1.0 {
                clump.pose[b.frame].0 = b.q;
            } else if t > 0.0 {
                let to = if anim_q.dot(b.q) < 0.0 { -b.q } else { b.q };
                clump.pose[b.frame].0 = anim_q.slerp(to, t);
            }
        }
    }

    /// `IsFacingTarget` (0x617E60): the hand points within ~18° of the target.
    pub fn is_facing_target(&self) -> bool {
        let Some(e) = self.bones.first() else { return false };
        let d1 = e.world.transform_vector3(self.effector_offset).normalize_or_zero();
        let d2 = (self.target - e.world.w_axis.truncate()).normalize_or_zero();
        d1.dot(d2) >= 0.95 && self.blend > 0.98
    }
}

/// `CTaskSimpleIKChain` (LookAt / PointArm).
#[derive(Debug, Clone)]
pub struct IkTask {
    eff_tag: i32,
    eff_offset: Vec3,
    bucket: u8,
    /// -1 = infinite.
    time_ms: i64,
    blend_ms: i64,
    speed: f32,
    /// World-space target.
    target: Vec3,
    pub chain: Option<IkChain>,
    blend: f32,
    end_time: i64,
    target_blend: f32,
    blend_end: i64,
    aborting: bool,
}

impl IkTask {
    fn new(eff_tag: i32, eff_offset: Vec3, bucket: u8, time_ms: i64, target: Vec3, speed: f32, blend_ms: i64) -> Self {
        Self {
            eff_tag,
            eff_offset,
            bucket,
            time_ms,
            blend_ms,
            speed,
            target,
            chain: None,
            blend: 0.0,
            end_time: 0,
            target_blend: 0.0,
            blend_end: 0,
            aborting: false,
        }
    }

    /// `UpdatePointArm` / `UpdateLookAt`.
    fn retarget(&mut self, target: Vec3, speed: f32, blend_ms: i64, time_ms: i64, now: i64) {
        self.target = target;
        self.speed = speed;
        self.blend_ms = blend_ms;
        self.end_time = if time_ms < 0 { -1 } else { now + time_ms };
        self.target_blend = 1.0;
        self.blend_end = now + blend_ms;
        self.aborting = false;
        if let Some(c) = &mut self.chain {
            c.speed = speed;
        }
    }

    /// `BlendOut(ms)` (0x633C40).
    pub fn blend_out(&mut self, ms: i64, now: i64) {
        if !self.aborting {
            if self.time_ms == -1 {
                self.time_ms = 0;
            }
            self.end_time = now + ms;
            self.aborting = true;
        }
    }

    /// `CTaskSimpleIKChain::ProcessPed` (0x633C80). Returns true when done.
    fn process(&mut self, clump: &Clump, now: i64, ts: f32) -> bool {
        let dt = (ts * 0.02 * 1000.0) as i64;
        let Some(chain) = &mut self.chain else {
            let Some(c) = IkChain::new(clump, self.eff_tag, self.eff_offset, 4, self.target, self.speed, self.bucket) else {
                return true;
            };
            self.chain = Some(c);
            self.end_time = if self.time_ms == -1 { -1 } else { now + self.time_ms };
            self.target_blend = 1.0;
            self.blend_end = now + self.blend_ms;
            self.chain.as_mut().unwrap().blend = self.blend;
            return false;
        };
        chain.target = self.target;
        if self.time_ms != -1 && now > self.end_time {
            self.chain = None;
            return true;
        }
        if self.time_ms != -1 && now >= self.end_time - self.blend_ms {
            self.target_blend = 0.0;
            self.blend_end = self.end_time;
        }
        if now > self.blend_end {
            self.blend = self.target_blend;
        } else {
            // QUIRK: the denominator is (remaining - dt).
            let mut f = dt as f32 / (self.blend_end - dt - now) as f32;
            if f > 1.0 || f.is_nan() {
                f = 1.0;
            }
            self.blend += (self.target_blend - self.blend) * f;
        }
        chain.blend = self.blend;
        false
    }
}

/// `CTaskSimpleIKManager` (secondary slot 5): 0 look-at, 1 right arm, 2 left arm.
#[derive(Debug, Clone, Default)]
pub struct IkManager {
    pub slots: [Option<IkTask>; 3],
}

impl IkManager {
    /// `IKChainManager_c::PointArm` (0x618B60) for a world position.
    pub fn point_arm(&mut self, arm: usize, target: Vec3, speed: f32, blend_ms: i64, now: i64) {
        let slot = arm + 1;
        match &mut self.slots[slot] {
            Some(t) => t.retarget(target, speed, blend_ms, 999_999, now),
            None => {
                let tag = if arm == 1 { 34 } else { 24 };
                let mut t = IkTask::new(tag, Vec3::new(0.05, 0.0, 0.0), slot as u8, 999_999, target, speed, blend_ms);
                t.retarget(target, speed, blend_ms, 999_999, now);
                self.slots[slot] = Some(t);
            }
        }
    }

    /// `IKChainManager_c::LookAt` (0x618970) at a world position.
    pub fn look_at(&mut self, target: Vec3, time_ms: i64, speed: f32, blend_ms: i64, now: i64) {
        match &mut self.slots[0] {
            Some(t) => t.retarget(target, speed, blend_ms, time_ms, now),
            None => {
                let mut t = IkTask::new(5, Vec3::new(0.0, 0.05, 0.0), 0, time_ms, target, speed, blend_ms);
                t.retarget(target, speed, blend_ms, time_ms, now);
                self.slots[0] = Some(t);
            }
        }
    }

    /// Follow a moving target (the original tracks the target entity): slot 0 look-at,
    /// 1 right arm, 2 left arm.
    pub fn set_target(&mut self, slot: usize, target: Vec3) {
        if let Some(t) = &mut self.slots[slot] {
            t.target = target;
            if let Some(c) = &mut t.chain {
                c.target = target;
            }
        }
    }

    pub fn abort_point_arm(&mut self, arm: usize, ms: i64, now: i64) {
        if let Some(t) = &mut self.slots[arm + 1] {
            t.blend_out(ms, now);
        }
    }

    pub fn abort_look_at(&mut self, ms: i64, now: i64) {
        if let Some(t) = &mut self.slots[0] {
            t.blend_out(ms, now);
        }
    }

    pub fn is_arm_pointing(&self, arm: usize) -> bool {
        self.slots[arm + 1].is_some()
    }

    pub fn is_looking(&self) -> bool {
        self.slots[0].is_some()
    }

    /// The task ProcessPed (blend ramps), in the ped's intelligence step.
    pub fn process(&mut self, clump: &Clump, now: i64, ts: f32) {
        for s in &mut self.slots {
            if let Some(t) = s {
                if t.process(clump, now, ts) {
                    *s = None;
                }
            }
        }
    }

    /// `IKChainManager_c::Update` for this ped (after the physics): buckets 0..2 in order.
    /// `to_model` maps world positions into the ped's model space.
    pub fn update_chains(&mut self, clump: &mut Clump, to_model: impl Fn(Vec3) -> Vec3) {
        for bucket in 0..3u8 {
            for t in self.slots.iter_mut().flatten() {
                if let Some(c) = &mut t.chain {
                    if c.bucket == bucket {
                        let target = to_model(c.target);
                        c.update(clump, target);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euler_round_trip() {
        for e in [Vec3::new(10.0, 20.0, 30.0), Vec3::new(-45.0, 5.0, -100.0), Vec3::new(113.4, -4.0, 6.5)] {
            let q = euler_to_quat(e);
            let back = quat_to_euler(q);
            assert!((back - e).abs().max_element() < 1e-3, "{e:?} -> {back:?}");
        }
    }
}
