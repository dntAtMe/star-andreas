//! On-foot player: skinned ped model, IFP animation playback, Rapier
//! kinematic character controller and a third-person orbit camera.
//!
//! The ped skeleton lives in GTA space (Z-up) under a single "model root"
//! entity rotated -90° about X, so animation keys apply untouched.

use std::{collections::HashMap, f32::consts::FRAC_PI_2, path::PathBuf};

use anyhow::{Context, Result};
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll},
    mesh::{
        Indices, PrimitiveTopology,
        skinning::{SkinnedMesh, SkinnedMeshInverseBindposes},
    },
    prelude::*,
    transform::TransformSystems,
    window::{CursorGrabMode, CursorOptions},
};
use bevy_rapier3d::prelude::*;
use sa_formats::{dff, ifp, txd};

use crate::{
    interp::Interp,
    stream::{Streamer, convert_texture, make_image},
    world::{WorldRes, g2b},
};

const PED_MODEL: &str = "fam1";
const CAPSULE_HALF: f32 = 0.5;
const CAPSULE_RADIUS: f32 = 0.35;
const GRAVITY: f32 = 18.0;
const JUMP_SPEED: f32 = 6.0;
const BLEND_TIME: f32 = 0.18;
const WALK_SPEED: f32 = 1.6;
const RUN_SPEED: f32 = 4.2;
const SPRINT_SPEED: f32 = 7.5;

const ANIM_IDLE: &str = "idle_stance";
const ANIM_WALK: &str = "walk_player";
const ANIM_RUN: &str = "run_player";
const ANIM_SPRINT: &str = "sprint_civi";
const ANIM_FALL: &str = "fall_fall";

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        let fly = std::env::var("SA_FLY").is_ok();
        app.insert_resource(if fly { Mode::Fly } else { Mode::Walk })
            .insert_resource(MouseLock(!fly))
            .add_systems(Startup, spawn_player)
            // Ped movement is computed per frame and *accumulated* into the kinematic
            // controller; the next fixed physics step applies all of it. (bevy_rapier
            // applies the controller by editing Transform, which only sticks for the
            // first physics step of a frame, so per-step movement would be lost.)
            .add_systems(Update, (toggle_mode, cursor_lock, player_control, animate_ped).chain())
            .add_systems(
                PostUpdate,
                orbit_camera.run_if(resource_equals(Mode::Walk)).before(TransformSystems::Propagate),
            );
    }
}

#[derive(Resource, Clone)]
pub struct GameRoot(pub PathBuf);

#[derive(Resource, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Mode {
    Walk,
    Fly,
}

#[derive(Resource)]
pub struct MouseLock(pub bool);

/// The entity the orbit camera follows (the ped, or the car being driven).
#[derive(Component)]
pub struct CamFollow {
    pub height: f32,
    pub dist: f32,
}

#[derive(Component)]
pub struct OrbitCam {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
}

#[derive(Component)]
pub struct Ped {
    /// Bone entity per DFF frame.
    bones: Vec<Entity>,
    /// Local bind pose per frame.
    bind: Vec<Transform>,
    /// Frame whose horizontal translation is root motion (stripped).
    root_bone: Option<usize>,
    vel_y: f32,
    air_time: f32,
    pub grounded: bool,
    pub frozen: bool,
    anim: AnimPlayer,
}

impl Ped {
    pub fn set_velocity_y(&mut self, v: f32) {
        self.vel_y = v;
    }
}

struct AnimPlayer {
    cur: &'static str,
    time: f32,
    /// Previous clip being faded out: (name, time).
    prev: Option<(&'static str, f32)>,
    blend: f32,
}

impl AnimPlayer {
    fn play(&mut self, name: &'static str) {
        if self.cur != name {
            self.prev = Some((self.cur, self.time));
            self.cur = name;
            self.time = 0.0;
            self.blend = 0.0;
        }
    }
}

/// Animation keyed by DFF frame index.
struct Clip {
    duration: f32,
    frames: Vec<Option<Vec<ifp::Key>>>,
}

#[derive(Resource)]
struct Clips(HashMap<String, Clip>);

// ---------------------------------------------------------------- spawn

pub fn frame_transform(f: &dff::Frame) -> Transform {
    let m = Mat3::from_cols(f.rot[0].into(), f.rot[1].into(), f.rot[2].into());
    Transform { translation: f.pos.into(), rotation: Quat::from_mat3(&m).normalize(), scale: Vec3::ONE }
}

fn spawn_player(
    mut commands: Commands,
    world: Res<WorldRes>,
    root: Res<GameRoot>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) -> Result<(), BevyError> {
    let world = &world.0;
    let clump = dff::parse(world.file(&format!("{PED_MODEL}.dff")).context("ped dff")?)?;
    let textures: HashMap<String, (Handle<Image>, bool)> =
        txd::parse(world.file(&format!("{PED_MODEL}.txd")).context("ped txd")?)?
            .into_iter()
            .filter_map(|t| convert_texture(t, false))
            .map(|t| (t.name.clone(), t.alpha, make_image(t)))
            .map(|(n, a, img)| (n, (images.add(img), a)))
            .collect();

    let geo = clump.geometries.iter().find(|g| g.skin.is_some()).context("ped has no skinned geometry")?;
    let skin = geo.skin.as_ref().unwrap();
    let hroot = clump
        .frames
        .iter()
        .find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty()))
        .context("no HAnim hierarchy")?;
    let frame_of_node = |id: i32| clump.frames.iter().position(|f| f.hanim.as_ref().is_some_and(|h| h.node_id == id));

    // Skeleton.
    let min_z = geo.positions.iter().map(|p| p[2]).fold(f32::MAX, f32::min);
    let feet = -(CAPSULE_HALF + CAPSULE_RADIUS) - min_z;
    let model_base = Transform::from_xyz(0.0, feet, 0.0).with_rotation(Quat::from_rotation_x(-FRAC_PI_2));
    let model_root = commands.spawn((model_base, Visibility::default())).id();
    let bind: Vec<Transform> = clump.frames.iter().map(frame_transform).collect();
    let bones: Vec<Entity> = clump
        .frames
        .iter()
        .zip(&bind)
        .map(|(f, tf)| commands.spawn((*tf, Visibility::default(), Name::new(f.name.clone()))).id())
        .collect();
    for (i, f) in clump.frames.iter().enumerate() {
        let parent = if f.parent >= 0 { bones[f.parent as usize] } else { model_root };
        commands.entity(parent).add_child(bones[i]);
    }

    let joints: Vec<Entity> =
        hroot.nodes.iter().map(|&(id, _, _)| frame_of_node(id).map(|i| bones[i]).unwrap_or(model_root)).collect();
    let inverse_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(
        skin.inverse_bind.iter().map(Mat4::from_cols_array).collect::<Vec<_>>(),
    ));

    // One skinned mesh per material.
    for (mi, mat) in geo.materials.iter().enumerate() {
        let mut remap = vec![u32::MAX; geo.positions.len()];
        let (mut pos, mut nrm, mut uv, mut ji, mut jw, mut idx) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for t in geo.triangles.iter().filter(|t| t.material as usize == mi) {
            for &v in &t.v {
                let v = v as usize;
                if remap[v] == u32::MAX {
                    remap[v] = pos.len() as u32;
                    pos.push(geo.positions[v]);
                    nrm.push(geo.normals.get(v).copied().unwrap_or([0.0, 0.0, 1.0]));
                    uv.push(geo.uvs.first().and_then(|u| u.get(v)).copied().unwrap_or([0.0; 2]));
                    let w = skin.weights[v];
                    let sum: f32 = w.iter().sum();
                    jw.push(if sum > 0.0 { w.map(|x| x / sum) } else { [1.0, 0.0, 0.0, 0.0] });
                    ji.push(skin.indices[v].map(|i| i as u16));
                }
                idx.push(remap[v]);
            }
        }
        if idx.is_empty() {
            continue;
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nrm);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, bevy::mesh::VertexAttributeValues::Uint16x4(ji));
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, jw);
        mesh.insert_indices(Indices::U32(idx));

        let tex = mat.texture.as_ref().and_then(|t| textures.get(&t.name.to_ascii_lowercase()));
        let c = mat.color;
        let material = materials.add(StandardMaterial {
            base_color: Color::srgba_u8(c[0], c[1], c[2], c[3]),
            base_color_texture: tex.map(|t| t.0.clone()),
            alpha_mode: if tex.is_some_and(|t| t.1) { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
            perceptual_roughness: 0.85,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        let part = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                SkinnedMesh { inverse_bindposes: inverse_bindposes.clone(), joints: joints.clone() },
                NoFrustumCulling,
            ))
            .id();
        commands.entity(model_root).add_child(part);
    }

    // Animations, retargeted onto this skeleton's frames.
    let anims = ifp::parse(&std::fs::read(root.0.join("anim/ped.ifp")).context("ped.ifp")?)?;
    let mut clips = HashMap::new();
    for a in anims {
        let mut frames: Vec<Option<Vec<ifp::Key>>> = vec![None; clump.frames.len()];
        for t in a.tracks {
            let fi = frame_of_node(t.bone_id).or_else(|| {
                clump.frames.iter().position(|f| f.name.eq_ignore_ascii_case(&t.bone_name))
            });
            if let Some(fi) = fi {
                if !t.keys.is_empty() {
                    frames[fi] = Some(t.keys);
                }
            }
        }
        clips.insert(a.name.to_ascii_lowercase(), Clip { duration: a.duration, frames });
    }
    for name in [ANIM_IDLE, ANIM_WALK, ANIM_RUN, ANIM_SPRINT, ANIM_FALL] {
        if !clips.contains_key(name) {
            warn!("animation {name} missing");
        }
    }
    commands.insert_resource(Clips(clips));

    let root_bone = frame_of_node(0).or_else(|| clump.frames.iter().position(|f| f.name.eq_ignore_ascii_case("root")));
    let spawn: Vec<f32> =
        std::env::var("SA_PLAYER").unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
    // SA_PLAYER=x,y,z[,heading]: GTA coords; heading in degrees, 0 = north, 90 = west.
    let (spawn, heading) = match spawn[..] {
        [x, y, z] => (g2b([x, y, z]), 180.0),
        [x, y, z, hd] => (g2b([x, y, z]), hd),
        _ => (g2b([2495.0, -1682.0, 14.5]), 180.0), // outside CJ's house
    };
    commands
        .spawn((
            Transform::from_translation(spawn).with_rotation(Quat::from_rotation_y(f32::to_radians(heading))),
            Visibility::default(),
            RigidBody::KinematicPositionBased,
            Collider::capsule_y(CAPSULE_HALF, CAPSULE_RADIUS),
            KinematicCharacterController {
                offset: CharacterLength::Absolute(0.03),
                up: Vec3::Y,
                max_slope_climb_angle: 50f32.to_radians(),
                min_slope_slide_angle: 55f32.to_radians(),
                autostep: Some(CharacterAutostep {
                    max_height: CharacterLength::Absolute(0.45),
                    min_width: CharacterLength::Absolute(0.15),
                    include_dynamic_bodies: false,
                }),
                snap_to_ground: Some(CharacterLength::Absolute(0.35)),
                ..default()
            },
            Ped {
                bones,
                bind,
                root_bone,
                vel_y: 0.0,
                air_time: 0.0,
                grounded: false,
                frozen: true,
                anim: AnimPlayer { cur: ANIM_IDLE, time: 0.0, prev: None, blend: 1.0 },
            },
            CamFollow { height: 0.6, dist: 3.5 },
            Interp::new(model_root, model_base, Transform::from_translation(spawn)),
        ))
        .add_child(model_root);
    Ok(())
}

// ---------------------------------------------------------------- input

fn toggle_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mut mode: ResMut<Mode>,
    mut lock: ResMut<MouseLock>,
    rapier: ReadRapierContext,
    driving: Res<crate::vehicle::Driving>,
    cam: Single<(&Transform, &mut crate::FlyCam, &mut OrbitCam), Without<Ped>>,
    ped: Single<(Entity, &mut Transform, &mut Ped)>,
) -> Result<(), BevyError> {
    if !keys.just_pressed(KeyCode::F2) || driving.0.is_some() {
        return Ok(());
    }
    let (cam_tf, mut fly, mut orbit) = cam.into_inner();
    let (ped_e, mut ped_tf, mut ped) = ped.into_inner();
    let (yaw, pitch, _) = cam_tf.rotation.to_euler(EulerRot::YXZ);
    match *mode {
        Mode::Walk => {
            *mode = Mode::Fly;
            fly.yaw = yaw;
            fly.pitch = pitch;
        }
        Mode::Fly => {
            // Drop the player onto whatever is below the camera.
            let ctx = rapier.single()?;
            let filter = QueryFilter::default().exclude_collider(ped_e);
            if let Some((_, toi)) = ctx.cast_ray(cam_tf.translation, -Vec3::Y, 500.0, true, filter) {
                ped_tf.translation = cam_tf.translation - Vec3::Y * (toi - 1.2);
                ped.vel_y = 0.0;
            }
            *mode = Mode::Walk;
            lock.0 = true;
            orbit.yaw = yaw;
            orbit.pitch = pitch.clamp(-1.2, 0.5);
        }
    }
    Ok(())
}

fn cursor_lock(
    mode: Res<Mode>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut lock: ResMut<MouseLock>,
    mut cursor: Single<&mut CursorOptions>,
) {
    if *mode != Mode::Walk {
        return;
    }
    if buttons.just_pressed(MouseButton::Left) {
        lock.0 = true;
    }
    if keys.just_pressed(KeyCode::Escape) {
        lock.0 = false;
    }
    cursor.grab_mode = if lock.0 { CursorGrabMode::Locked } else { CursorGrabMode::None };
    cursor.visible = !lock.0;
}

fn player_control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    st: Res<Streamer>,
    driving: Res<crate::vehicle::Driving>,
    cam: Single<&OrbitCam>,
    mut probe: Local<Option<(Vec3, f32, std::time::Instant)>>,
    ped: Single<(
        &mut Ped,
        &mut KinematicCharacterController,
        Option<&KinematicCharacterControllerOutput>,
        &mut Transform,
    )>,
) {
    let dt = time.delta_secs();
    let (mut ped, mut kcc, out, mut tf) = ped.into_inner();
    let s = st.stats;
    if ped.frozen {
        // Wait for collision around the spawn point before enabling gravity.
        if s.pending == 0 && s.models_loading == 0 && s.spawned > 0 && time.elapsed_secs() > 1.0 {
            ped.frozen = false;
        }
        kcc.translation = None;
        return;
    }
    if driving.0.is_some() {
        kcc.translation = None;
        ped.anim.play(ANIM_IDLE);
        return;
    }

    let auto_walk = std::env::var("SA_AUTOWALK").is_ok();
    // Debug probe: distance covered in the first 5 simulated seconds of movement.
    if auto_walk {
        let (start, t, wall) = probe.get_or_insert((tf.translation, 0.0, std::time::Instant::now()));
        let before = *t;
        *t += dt;
        if before < 5.0 && *t >= 5.0 {
            let d = (tf.translation - *start).with_y(0.0).length();
            info!(
                "ped probe: {d:.2} m in 5.0 sim s ({:.2} m/s), wall {:.2} s",
                d / 5.0,
                wall.elapsed().as_secs_f32()
            );
        }
    }
    let active = *mode == Mode::Walk;
    let pressed = |k: KeyCode| active && keys.pressed(k);
    let mut input = Vec2::ZERO;
    if pressed(KeyCode::KeyW) || auto_walk {
        input.y += 1.0;
    }
    if pressed(KeyCode::KeyS) {
        input.y -= 1.0;
    }
    if pressed(KeyCode::KeyD) {
        input.x += 1.0;
    }
    if pressed(KeyCode::KeyA) {
        input.x -= 1.0;
    }
    let input = input.normalize_or_zero();
    let (sy, cy) = cam.yaw.sin_cos();
    let forward = Vec3::new(-sy, 0.0, -cy);
    let right = Vec3::new(cy, 0.0, -sy);
    let dir = forward * input.y + right * input.x;

    let (speed, gait) = if dir == Vec3::ZERO {
        (0.0, ANIM_IDLE)
    } else if pressed(KeyCode::AltLeft) {
        (WALK_SPEED, ANIM_WALK)
    } else if pressed(KeyCode::ShiftLeft) {
        (SPRINT_SPEED, ANIM_SPRINT)
    } else {
        (RUN_SPEED, ANIM_RUN)
    };

    ped.grounded = out.is_some_and(|o| o.grounded);
    if ped.grounded {
        ped.air_time = 0.0;
        ped.vel_y = ped.vel_y.max(-1.0);
        if pressed(KeyCode::Space) {
            ped.vel_y = JUMP_SPEED;
            ped.grounded = false;
        }
    } else {
        ped.air_time += dt;
    }
    ped.vel_y = (ped.vel_y - GRAVITY * dt).max(-50.0);
    let step = dir * speed * dt + Vec3::Y * ped.vel_y * dt;
    kcc.translation = Some(kcc.translation.unwrap_or(Vec3::ZERO) + step);

    if dir != Vec3::ZERO {
        let target = Quat::from_rotation_y((-dir.x).atan2(-dir.z));
        tf.rotation = tf.rotation.slerp(target, 1.0 - (-12.0 * dt).exp());
    }
    let anim = if ped.air_time > 0.25 { ANIM_FALL } else { gait };
    ped.anim.play(anim);
}

// ---------------------------------------------------------------- animation

fn sample(keys: &[ifp::Key], duration: f32, t: f32) -> (Quat, Option<Vec3>) {
    let t = if duration > 0.0 { t % duration } else { 0.0 };
    let q = |k: &ifp::Key| Quat::from_array(k.rot).normalize();
    let i = keys.partition_point(|k| k.time <= t);
    let (a, b) = (&keys[i.saturating_sub(1)], &keys[i.min(keys.len() - 1)]);
    let f = if b.time > a.time { ((t - a.time) / (b.time - a.time)).clamp(0.0, 1.0) } else { 0.0 };
    let pos = match (a.pos, b.pos) {
        (Some(p), Some(r)) => Some(Vec3::from(p).lerp(r.into(), f)),
        (p, _) => p.map(Vec3::from),
    };
    (q(a).slerp(q(b), f), pos)
}

fn animate_ped(
    time: Res<Time>,
    clips: Option<Res<Clips>>,
    mut peds: Query<&mut Ped>,
    mut bones: Query<&mut Transform, Without<Ped>>,
) {
    let Some(clips) = clips else { return };
    let dt = time.delta_secs();
    for mut ped in &mut peds {
        let ped = &mut *ped;
        ped.anim.time += dt;
        if let Some((_, t)) = &mut ped.anim.prev {
            *t += dt;
        }
        ped.anim.blend = (ped.anim.blend + dt / BLEND_TIME).min(1.0);
        if ped.anim.blend >= 1.0 {
            ped.anim.prev = None;
        }
        let Some(cur) = clips.0.get(ped.anim.cur) else { continue };
        let prev = ped.anim.prev.and_then(|(n, t)| clips.0.get(n).map(|c| (c, t)));

        for (fi, &bone) in ped.bones.iter().enumerate() {
            let bind = ped.bind[fi];
            let pose = |clip: &Clip, t: f32| match &clip.frames[fi] {
                Some(keys) => sample(keys, clip.duration, t),
                None => (bind.rotation, None),
            };
            let (mut rot, mut pos) = pose(cur, ped.anim.time);
            if let Some((pc, pt)) = prev {
                let (prot, ppos) = pose(pc, pt);
                rot = prot.slerp(rot, ped.anim.blend);
                pos = match (ppos, pos) {
                    (Some(a), Some(b)) => Some(a.lerp(b, ped.anim.blend)),
                    (a, b) => b.or(a),
                };
            }
            let mut pos = pos.unwrap_or(bind.translation);
            if Some(fi) == ped.root_bone {
                // Root motion is driven by the controller; keep only the vertical bob.
                pos.x = bind.translation.x;
                pos.y = bind.translation.y;
            }
            if let Ok(mut tf) = bones.get_mut(bone) {
                tf.rotation = rot;
                tf.translation = pos;
            }
        }
    }
}

// ---------------------------------------------------------------- camera

fn orbit_camera(
    time: Res<Time>,
    fixed: Res<Time<Fixed>>,
    lock: Res<MouseLock>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    rapier: ReadRapierContext,
    mut zoom: Local<Option<f32>>,
    mut idle: Local<f32>,
    target: Single<(Entity, &Transform, &CamFollow, Option<&crate::vehicle::Vehicle>, Option<&Interp>)>,
    cam: Single<(&mut Transform, &mut OrbitCam), Without<CamFollow>>,
) {
    let (target_e, body_tf, follow, car, interp) = *target;
    // Follow the rendered (interpolated) pose, not the raw physics step.
    let target_tf = &interp.map(|i| i.pose(fixed.overstep_fraction())).unwrap_or(*body_tf);
    let (mut tf, mut oc) = cam.into_inner();
    let dt = time.delta_secs();
    let moved = lock.0 && motion.delta != Vec2::ZERO;
    if moved {
        oc.yaw -= motion.delta.x * 0.003;
        oc.pitch = (oc.pitch - motion.delta.y * 0.003).clamp(-1.3, 0.6);
        *idle = 0.0;
    } else {
        *idle += dt;
    }
    // Zoom is relative to the target's default distance.
    let z = zoom.get_or_insert(1.0);
    if scroll.delta.y != 0.0 {
        *z = (*z * 0.9f32.powf(scroll.delta.y)).clamp(0.4, 5.0);
    }
    oc.dist = follow.dist * *z;
    // Swing behind a moving car when the mouse is left alone (GTA-style).
    if let Some(v) = car {
        if *idle > 1.0 && v.speed.abs() > 3.0 {
            let car_yaw = target_tf.rotation.to_euler(EulerRot::YXZ).0;
            let diff = (car_yaw - oc.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            oc.yaw += diff * (1.0 - (-2.5 * dt).exp());
            oc.pitch += (-0.2 - oc.pitch) * (1.0 - (-2.0 * dt).exp());
        }
    }
    let rot = Quat::from_euler(EulerRot::YXZ, oc.yaw, oc.pitch, 0.0);
    let target = target_tf.translation + Vec3::Y * follow.height;
    let back = rot * Vec3::Z;
    let mut dist = oc.dist;
    if let Ok(ctx) = rapier.single() {
        let filter = QueryFilter::default().exclude_rigid_body(target_e);
        if let Some((_, toi)) = ctx.cast_ray(target, back, oc.dist, true, filter) {
            dist = (toi - 0.25).max(0.4);
        }
    }
    tf.translation = target + back * dist;
    tf.rotation = rot;
}
