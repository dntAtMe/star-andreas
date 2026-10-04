//! On-foot player: skinned ped model, IFP animation playback, movement on the
//! ported SA ped physics (`sa_physics::ped`) and a third-person orbit camera.
//!
//! The ped skeleton lives in GTA space (Z-up) under a single "model root"
//! entity rotated -90° about X, so animation keys apply untouched.

use std::{collections::HashMap, f32::consts::FRAC_PI_2, path::PathBuf, sync::Arc};

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
use sa_formats::{dff, ifp, img::Img, txd};
use sa_physics::{
    anim::{AnimManager, Clump, anim_id, group},
    physical::ef,
    ped::{PedLogic, ped_col_model, ped_physical},
    weapon::WeaponInfos,
    world::EntityId,
};

use crate::{
    saphys::{SaBody, SaPhys, SaPhysExt, gta_matrix},
    stream::{Streamer, convert_texture, make_image},
    world::{WorldRes, b2g, g2b},
};

const PED_MODEL: &str = "fam1";

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        let fly = std::env::var("SA_FLY").is_ok();
        app.insert_resource(if fly { Mode::Fly } else { Mode::Walk })
            .insert_resource(MouseLock(!fly))
            .add_systems(Startup, spawn_player)
            .add_systems(
                Update,
                (toggle_mode, cursor_lock, player_control).chain().before(crate::saphys::SaStep),
            )
            .add_systems(Update, animate_ped.after(crate::saphys::SaStep))
            .add_systems(
                PostUpdate,
                orbit_camera
                    .run_if(resource_equals(Mode::Walk))
                    .after(crate::saphys::SaSync)
                    .before(TransformSystems::Propagate),
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
    /// The ped's body in the SA physics world.
    pub sa: EntityId,
    /// Bone entity per DFF frame.
    pub bones: Vec<Entity>,
    /// DFF frame of each anim clump frame (HAnim node).
    pub node_frames: Vec<usize>,
    pub grounded: bool,
    pub frozen: bool,
}

/// Put the player ped's SA body into (or take it out of) a vehicle: while
/// inside it doesn't collide or move on its own.
pub fn ped_set_in_vehicle(sa: &mut SaPhys, id: EntityId, inside: bool) {
    if let Some(b) = sa.world.body_mut(id) {
        if inside {
            b.phys.eflags = (b.phys.eflags | ef::IS_STATIC) & !ef::USES_COLLISION;
        } else {
            b.phys.eflags = (b.phys.eflags & !ef::IS_STATIC) | ef::USES_COLLISION;
        }
        b.phys.move_speed = Vec3::ZERO;
    }
    if let Some(ped) = sa.logic_mut::<PedLogic>(id) {
        ped.standing = false;
        ped.anim_velocity = Vec2::ZERO;
    }
}

/// Teleport the ped's SA body (Bevy-space position, Bevy yaw).
pub fn ped_teleport(sa: &mut SaPhys, id: EntityId, pos: Vec3, yaw: Option<f32>) {
    if let Some(b) = sa.world.body_mut(id) {
        b.phys.matrix.pos = Vec3::from(b2g(pos));
        b.phys.move_speed = Vec3::ZERO;
    }
    if let (Some(yaw), Some(ped)) = (yaw, sa.logic_mut::<PedLogic>(id)) {
        // Bevy yaw about +Y (0 = facing -Z = GTA north) equals the GTA heading.
        ped.cur_rot = yaw;
        ped.aim_rot = yaw;
    }
}

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
    mut sa: ResMut<SaPhys>,
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

    // Skeleton. The SA ped origin (1.0 above the feet) is also the model origin.
    let model_base = Transform::from_rotation(Quat::from_rotation_x(-FRAC_PI_2));
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
                crate::dynlight::DynLit,
                MeshMaterial3d(material),
                SkinnedMesh { inverse_bindposes: inverse_bindposes.clone(), joints: joints.clone() },
                NoFrustumCulling,
            ))
            .id();
        commands.entity(model_root).add_child(part);
    }

    // CAnimManager: ped.ifp plus the anim.img blocks.
    let anim_img = Img::open(&root.0.join("anim/anim.img")).ok();
    let ped_ifp = ifp::parse(&std::fs::read(root.0.join("anim/ped.ifp")).context("ped.ifp")?)?;
    let anims = Arc::new(AnimManager::load(|block| {
        if block.eq_ignore_ascii_case("ped") {
            return Some(ped_ifp.clone());
        }
        let data = anim_img.as_ref()?.get(&format!("{}.ifp", block.to_ascii_lowercase()))?;
        ifp::parse(data).ok()
    }));
    // CWeaponInfo::LoadWeaponData.
    let weapon_dat = sa_formats::weapondat::parse(&std::fs::read(root.0.join("data/weapon.dat")).context("weapon.dat")?)?;
    let weapons = Arc::new(WeaponInfos::load(&weapon_dat, AnimManager::group_by_name));
    sa.world.weapon_infos = Some(weapons.clone());

    // The anim clump: one frame per HAnim node, parented through the DFF frames.
    let node_frames: Vec<usize> = hroot.nodes.iter().filter_map(|&(id, _, _)| frame_of_node(id)).collect();
    let node_of_frame = |f: usize| node_frames.iter().position(|&n| n == f);
    let mut anim_clump = Clump::new(
        hroot
            .nodes
            .iter()
            .zip(&node_frames)
            .map(|(&(id, _, _), &fi)| {
                let mut parent = None;
                let mut f = clump.frames[fi].parent;
                while f >= 0 {
                    if let Some(n) = node_of_frame(f as usize) {
                        parent = Some(n);
                        break;
                    }
                    f = clump.frames[f as usize].parent;
                }
                (id, clump.frames[fi].name.clone(), bind[fi].rotation, bind[fi].translation, parent)
            })
            .collect(),
    );
    anim_clump.blend_animation(&anims, group::PLAYER, anim_id::IDLE, 1000.0);

    let spawn: Vec<f32> =
        std::env::var("SA_PLAYER").unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
    // SA_PLAYER=x,y,z[,heading]: GTA coords; heading in degrees, 0 = north, 90 = west.
    let (spawn, heading) = match spawn[..] {
        [x, y, z] => (g2b([x, y, z]), 180.0),
        [x, y, z, hd] => (g2b([x, y, z]), hd),
        _ => (g2b([2495.0, -1682.0, 14.5]), 180.0), // outside CJ's house
    };
    let tf = Transform::from_translation(spawn).with_rotation(Quat::from_rotation_y(f32::to_radians(heading)));
    let m = gta_matrix(&tf);
    // Frozen (static) until the collision around the spawn point has streamed in.
    let mut phys = ped_physical(m);
    phys.eflags |= ef::IS_STATIC;
    let mut logic = PedLogic::new(true, f32::to_radians(heading));
    logic.prev_pose = anim_clump.pose.clone();
    logic.clump = Some(Box::new(anim_clump));
    logic.tasks.anims = Some(anims);
    logic.tasks.infos = Some(weapons);
    let id = sa.world.add_body(phys, ped_col_model(), Box::new(logic));
    commands
        .spawn((
            tf,
            Visibility::default(),
            Ped { sa: id, bones, node_frames, grounded: false, frozen: true },
            CamFollow { height: 0.6, dist: 3.5 },
            SaBody::new(id, m),
        ))
        .add_child(model_root);
    Ok(())
}

// ---------------------------------------------------------------- input

fn toggle_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mut mode: ResMut<Mode>,
    mut lock: ResMut<MouseLock>,
    mut sa: ResMut<crate::saphys::SaPhys>,
    driving: Res<crate::vehicle::Driving>,
    cam: Single<(&Transform, &mut crate::FlyCam, &mut OrbitCam), Without<Ped>>,
    ped: Single<(Entity, &Ped)>,
) -> Result<(), BevyError> {
    if !keys.just_pressed(KeyCode::F2) || driving.0.is_some() {
        return Ok(());
    }
    let (cam_tf, mut fly, mut orbit) = cam.into_inner();
    let (ped_e, ped) = *ped;
    let (yaw, pitch, _) = cam_tf.rotation.to_euler(EulerRot::YXZ);
    match *mode {
        Mode::Walk => {
            *mode = Mode::Fly;
            fly.yaw = yaw;
            fly.pitch = pitch;
        }
        Mode::Fly => {
            // Drop the player onto whatever is below the camera (SA line of sight).
            if let Some(hit) = sa.cast_ray(cam_tf.translation, -Vec3::Y, 500.0, false, Some(ped_e)) {
                let pos = cam_tf.translation - Vec3::Y * (hit.toi - 1.2);
                ped_teleport(&mut sa, ped.sa, pos, None);
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
    dbg: Res<crate::debug::DebugUi>,
) {
    if *mode != Mode::Walk {
        return;
    }
    if dbg.open {
        // The debug UI needs a free cursor.
        lock.0 = false;
    } else if buttons.just_pressed(MouseButton::Left) {
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
    buttons: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    mode: Res<Mode>,
    lock: Res<MouseLock>,
    st: Res<Streamer>,
    driving: Res<crate::vehicle::Driving>,
    mut sa: ResMut<SaPhys>,
    mut spawn: ResMut<crate::vehicle::SpawnQueue>,
    mut auto_duck: Local<bool>,
    mut self_boom: Local<u32>,
    mut ped: Single<&mut Ped>,
) {
    let id = ped.sa;
    let s = st.stats;
    if ped.frozen {
        // Wait for collision around the spawn point before enabling gravity.
        if s.pending == 0 && s.models_loading == 0 && s.spawned > 0 && time.elapsed_secs() > 1.0 {
            ped.frozen = false;
            if let Some(b) = sa.world.body_mut(id) {
                b.phys.eflags &= !ef::IS_STATIC;
            }
            // SA_SPAWN=<model>: a car 5 m ahead (debug).
            if let Ok(m) = std::env::var("SA_SPAWN") {
                spawn.0.push(m);
            }
            // SA_WEAPON=<type>: start with that weapon (debug).
            if let Some(ty) = std::env::var("SA_WEAPON").ok().and_then(|v| v.parse::<u32>().ok()) {
                if let Some(l) = sa.logic_mut::<PedLogic>(id) {
                    let slot = l.tasks.give_weapon(ty, 500);
                    l.tasks.pd.chosen_slot = slot;
                }
            }
        }
        return;
    }
    if driving.0.is_some() {
        return;
    }

    // CPad for the on-foot player: WASD as the left stick (±128, +ud = backwards).
    let auto_walk = std::env::var("SA_AUTOWALK").is_ok();
    let active = *mode == Mode::Walk;
    let pressed = |k: KeyCode| active && keys.pressed(k);
    let just = |k: KeyCode| active && keys.just_pressed(k);
    let mut lr = 0.0;
    let mut ud = 0.0;
    if pressed(KeyCode::KeyW) || auto_walk {
        ud -= 128.0;
    }
    if pressed(KeyCode::KeyS) {
        ud += 128.0;
    }
    if pressed(KeyCode::KeyD) {
        lr += 128.0;
    }
    if pressed(KeyCode::KeyA) {
        lr -= 128.0;
    }
    // SA_SELFBOOM=<n>: n grenade explosions at the player's feet from 9 s (debug).
    let booms: u32 = std::env::var("SA_SELFBOOM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    if *self_boom < booms && time.elapsed_secs() > 9.0 + *self_boom as f32 * 0.7 {
        *self_boom += 1;
        if let Some(b) = sa.world.body(id) {
            let pos = b.phys.matrix.pos + Vec3::new(1.0, 0.0, -0.8);
            sa.world.add_explosion(None, None, sa_physics::effects::ExplosionType::Grenade, pos, 0, -1.0, false);
        }
    }
    let mouse = active && lock.0;
    let Some(logic) = sa.logic_mut::<PedLogic>(id) else { return };
    let pad = &mut logic.tasks.pad;
    pad.walk_lr = lr;
    pad.walk_ud = ud;
    pad.walk_key = pressed(KeyCode::AltLeft);
    pad.sprint = pressed(KeyCode::ShiftLeft);
    pad.sprint_just_down |= just(KeyCode::ShiftLeft);
    pad.jump_just_down |= just(KeyCode::Space);
    // SA_AUTOFIRE=1 / SA_AUTOAIM=1: hold fire / aim (debug).
    let auto_fire = std::env::var("SA_AUTOFIRE").is_ok();
    pad.aim = (mouse && buttons.pressed(MouseButton::Right)) || std::env::var("SA_AUTOAIM").is_ok();
    pad.fire = (mouse && buttons.pressed(MouseButton::Left)) || auto_fire;
    if auto_fire && (time.elapsed_secs() % 3.0) < time.delta_secs() {
        pad.fire_just_down = true;
    }
    pad.fire_just_down |= mouse && buttons.just_pressed(MouseButton::Left);
    pad.duck_just_down |= just(KeyCode::KeyC);
    // SA_AUTODUCK=1: crouch once (debug).
    if !*auto_duck && std::env::var("SA_AUTODUCK").is_ok() && time.elapsed_secs() > 8.0 {
        *auto_duck = true;
        pad.duck_just_down = true;
    }
    pad.enter_exit_just_down |= just(KeyCode::KeyF);
    // Next / previous weapon: mouse wheel, E / Q.
    pad.next_weapon_just_down |= just(KeyCode::KeyE) || (mouse && scroll.delta.y < 0.0);
    pad.prev_weapon_just_down |= just(KeyCode::KeyQ) || (mouse && scroll.delta.y > 0.0);

    let knocked = logic.knocked_down;
    if knocked > 75.0 {
        logic.knocked_down = 0.0;
    }
    ped.grounded = logic.standing;
}

// ---------------------------------------------------------------- animation

/// Copy the SA anim clump's pose (interpolated between physics steps) onto the bones.
fn animate_ped(sa: Res<SaPhys>, peds: Query<&Ped>, mut bones: Query<&mut Transform, Without<Ped>>) {
    let alpha = sa.alpha();
    for ped in &peds {
        let Some(logic) = sa.logic::<PedLogic>(ped.sa) else { continue };
        let Some(clump) = logic.clump.as_deref() else { continue };
        for (k, &(q, t)) in clump.pose.iter().enumerate() {
            let (pq, pt) = logic.prev_pose.get(k).copied().unwrap_or((q, t));
            let Some(&e) = ped.node_frames.get(k).and_then(|&f| ped.bones.get(f)) else { continue };
            if let Ok(mut tf) = bones.get_mut(e) {
                tf.rotation = pq.slerp(q, alpha);
                tf.translation = pt.lerp(t, alpha);
            }
        }
    }
}

// ---------------------------------------------------------------- camera

fn orbit_camera(
    time: Res<Time>,
    lock: Res<MouseLock>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut sa: ResMut<crate::saphys::SaPhys>,
    mut zoom: Local<Option<f32>>,
    mut idle: Local<f32>,
    target: Single<(Entity, &Transform, &CamFollow, Option<&crate::vehicle::Vehicle>)>,
    cam: Single<(&mut Transform, &mut OrbitCam), Without<CamFollow>>,
    mut sa_cam: ResMut<crate::camera::SaCam>,
) {
    // The target's Transform is already the interpolated SA pose (SaSync).
    let (target_e, target_tf, follow, car) = *target;
    // On foot the SA cameras (camera.rs) run instead.
    if car.is_none() {
        return;
    }
    sa_cam.reset_from_orbit();
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
    // Pull in when something is between the target and the camera (SA line of sight).
    if let Some(hit) = sa.cast_ray(target, back, oc.dist, false, Some(target_e)) {
        dist = (hit.toi - 0.25).max(0.4);
    }
    tf.translation = target + back * dist;
    tf.rotation = rot;
}
