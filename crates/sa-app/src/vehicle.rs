//! Drivable cars: DFF model with carcols paint, simulated by the ported SA
//! physics (`sa_physics::automobile`).
//!
//! The model hierarchy stays in GTA space (Z-up) under a model root rotated
//! -90° about X. The car entity's transform follows its SA body; a kinematic
//! Rapier proxy keeps the (still Rapier-based) ped from walking through it.

use std::{collections::HashMap, f32::consts::FRAC_PI_2};

use anyhow::{Context, Result};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use bevy_rapier3d::prelude::*;
use sa_formats::{
    col, dff, txd,
    vehicle::{self, CarColors, Handling, VehicleDef},
};
use sa_physics::{
    automobile::{Automobile, CarInput, VehicleHandling},
    collision::ColModel as SaColModel,
    physical::{EntityType, Physical, Status, VehicleClass, VehicleInfo},
    world::EntityId,
};

use crate::{
    player::{CamFollow, GameRoot, Mode, Ped, frame_transform},
    saphys::{SaBody, SaPhys, SaStep, gta_matrix},
    stream::{convert_texture, make_image},
    world::{WorldRes, g2b},
};
/// Cars cycled by the spawn key.
const SPAWN_LIST: &[&str] = &["greenwoo", "sabre", "infernus", "bobcat", "savanna", "elegy", "banshee", "sultan"];

pub struct VehiclePlugin;

impl Plugin for VehiclePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Driving>()
            .add_systems(Startup, load_vehicle_db)
            .add_systems(Update, (spawn_key, auto_drive, enter_exit, feed_inputs).chain().before(SaStep))
            .add_systems(Update, update_wheels.after(SaStep));
    }
}

/// The car the player is currently driving.
#[derive(Resource, Default)]
pub struct Driving(pub Option<Entity>);

#[derive(Resource)]
struct VehicleDb {
    defs: HashMap<String, VehicleDef>,
    handling: HashMap<String, Handling>,
    colors: CarColors,
    /// models/generic/vehicle.txd: shared textures (lights, grunge, ...).
    generic: HashMap<String, (Handle<Image>, bool)>,
    next_spawn: usize,
}

struct Wheel {
    /// Wheel dummy (rest centre), GTA model space.
    dummy: Vec3,
    /// Index into the SA wheel arrays (0 FL, 1 RL, 2 FR, 3 RR).
    sa_index: usize,
    front: bool,
    pivot: Entity,
}

#[derive(Component)]
pub struct Vehicle {
    pub name: String,
    pub sa: EntityId,
    wheels: Vec<Wheel>,
    /// Forward speed in m/s (from the SA body).
    pub speed: f32,
    /// Seat offset (Bevy local space) for placing the hidden driver.
    seat: Vec3,
}

// ---------------------------------------------------------------- loading

fn load_vehicle_db(mut commands: Commands, root: Res<GameRoot>, mut images: ResMut<Assets<Image>>) -> Result<(), BevyError> {
    let read = |p: &str| -> Result<String> {
        Ok(String::from_utf8_lossy(&std::fs::read(root.0.join(p)).with_context(|| p.to_string())?).into_owned())
    };
    let defs = vehicle::parse_vehicles_ide(&read("data/vehicles.ide")?).into_iter().map(|d| (d.model.clone(), d)).collect();
    let handling = vehicle::parse_handling(&read("data/handling.cfg")?);
    let colors = vehicle::parse_carcols(&read("data/carcols.dat")?);
    let generic = load_txd(&std::fs::read(root.0.join("models/generic/vehicle.txd"))?, &mut images)?;
    commands.insert_resource(VehicleDb { defs, handling, colors, generic, next_spawn: 0 });
    Ok(())
}

fn load_txd(data: &[u8], images: &mut Assets<Image>) -> Result<HashMap<String, (Handle<Image>, bool)>> {
    Ok(txd::parse(data)?
        .into_iter()
        .filter_map(|t| convert_texture(t, false))
        .map(|t| (t.name.clone(), t.alpha, make_image(t)))
        .map(|(n, a, img)| (n, (images.add(img), a)))
        .collect())
}

/// Material marker colours used by SA vehicle DFFs.
enum Paint {
    Primary,
    Secondary,
    FrontLight,
    RearLight,
    Plain,
}

fn paint_of(c: [u8; 4]) -> Paint {
    match [c[0], c[1], c[2]] {
        [60, 255, 0] => Paint::Primary,
        [255, 0, 175] => Paint::Secondary,
        [255, 175, 0] | [0, 255, 200] => Paint::FrontLight,
        [185, 255, 0] | [255, 60, 0] => Paint::RearLight,
        _ => Paint::Plain,
    }
}

/// Mesh of one geometry split per material, in the geometry's own frame space.
fn geometry_meshes(geo: &dff::Geometry) -> Vec<(usize, Mesh)> {
    let mut out = Vec::new();
    for mi in 0..geo.materials.len() {
        let mut remap = vec![u32::MAX; geo.positions.len()];
        let (mut pos, mut nrm, mut uv, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for t in geo.triangles.iter().filter(|t| t.material as usize == mi) {
            for &v in &t.v {
                let v = v as usize;
                if v >= remap.len() {
                    continue;
                }
                if remap[v] == u32::MAX {
                    remap[v] = pos.len() as u32;
                    pos.push(geo.positions[v]);
                    nrm.push(geo.normals.get(v).copied().unwrap_or([0.0, 0.0, 1.0]));
                    uv.push(geo.uvs.first().and_then(|u| u.get(v)).copied().unwrap_or([0.0; 2]));
                }
                idx.push(remap[v]);
            }
        }
        if idx.len() < 3 || idx.len() % 3 != 0 {
            continue;
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nrm);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_indices(Indices::U32(idx));
        out.push((mi, mesh));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn spawn_vehicle(
    commands: &mut Commands,
    world: &crate::world::World,
    sa: &mut SaPhys,
    db: &VehicleDb,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    name: &str,
    pos: Vec3,
    yaw: f32,
    color_seed: usize,
) -> Result<Entity> {
    let def = db.defs.get(name).with_context(|| format!("no vehicle {name}"))?;
    let h = db.handling.get(&def.handling).with_context(|| format!("no handling {}", def.handling))?.clone();
    let clump = dff::parse(world.file(&format!("{}.dff", def.model)).context("vehicle dff")?)?;
    let own_tex = match world.file(&format!("{}.txd", def.txd)) {
        Some(d) => load_txd(d, images)?,
        None => HashMap::new(),
    };
    let tex = |n: &str| own_tex.get(n).or_else(|| db.generic.get(n)).cloned();

    let pair = db.colors.cars.get(&def.model).and_then(|v| (!v.is_empty()).then(|| v[color_seed % v.len()]));
    let pal = |i: usize| db.colors.palette.get(i).map(|c| Color::srgb_u8(c[0], c[1], c[2]));
    let primary = pair.and_then(|p| pal(p.0)).unwrap_or(Color::srgb(0.6, 0.6, 0.6));
    let secondary = pair.and_then(|p| pal(p.1)).unwrap_or(primary);

    let mut make_material = |m: &dff::Material| -> Handle<StandardMaterial> {
        let t = m.texture.as_ref().and_then(|t| tex(&t.name.to_ascii_lowercase()));
        let c = m.color;
        let alpha = c[3] as f32 / 255.0;
        let (base, body) = match paint_of(c) {
            Paint::Primary => (primary, true),
            Paint::Secondary => (secondary, true),
            Paint::FrontLight => (Color::WHITE, false),
            Paint::RearLight => (Color::srgb(0.75, 0.1, 0.1), false),
            Paint::Plain => (Color::srgb_u8(c[0], c[1], c[2]), false),
        };
        materials.add(StandardMaterial {
            base_color: base.with_alpha(alpha),
            base_color_texture: t.as_ref().map(|t| t.0.clone()),
            alpha_mode: if c[3] < 255 {
                AlphaMode::Blend
            } else if t.as_ref().is_some_and(|t| t.1) {
                AlphaMode::Mask(0.5)
            } else {
                AlphaMode::Opaque
            },
            perceptual_roughness: if body { 0.3 } else { 0.7 },
            reflectance: if body { 0.6 } else { 0.3 },
            double_sided: true,
            cull_mode: None,
            ..default()
        })
    };

    // Frame hierarchy in GTA space.
    let model_base = Transform::from_rotation(Quat::from_rotation_x(-FRAC_PI_2));
    let model_root = commands.spawn((model_base, Visibility::default())).id();
    let frames: Vec<Entity> = clump
        .frames
        .iter()
        .map(|f| commands.spawn((frame_transform(f), Visibility::default(), Name::new(f.name.clone()))).id())
        .collect();
    for (i, f) in clump.frames.iter().enumerate() {
        let parent = if f.parent >= 0 { frames[f.parent as usize] } else { model_root };
        commands.entity(parent).add_child(frames[i]);
    }

    let mut wheel_parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)> = Vec::new();
    for a in &clump.atomics {
        let fname = clump.frames[a.frame as usize].name.to_ascii_lowercase();
        if fname.ends_with("_dam") || fname.ends_with("_vlo") {
            continue;
        }
        let geo = &clump.geometries[a.geometry as usize];
        for (mi, mesh) in geometry_meshes(geo) {
            let part = (meshes.add(mesh), make_material(&geo.materials[mi]));
            if fname == "wheel" {
                wheel_parts.push(part);
            } else {
                let e = commands.spawn((Mesh3d(part.0), MeshMaterial3d(part.1))).id();
                commands.entity(frames[a.frame as usize]).add_child(e);
            }
        }
    }
    // The wheel atomic hangs under wheel_rf_dummy; detach it and instance per wheel.
    if let Some(wf) = clump.frames.iter().position(|f| f.name.eq_ignore_ascii_case("wheel")) {
        commands.entity(frames[wf]).despawn();
    }

    let mut wheels = Vec::new();
    for (name, front, left) in [
        ("wheel_lf_dummy", true, true),
        ("wheel_rf_dummy", true, false),
        ("wheel_lb_dummy", false, true),
        ("wheel_rb_dummy", false, false),
    ] {
        let Some(fi) = clump.frames.iter().position(|f| f.name.eq_ignore_ascii_case(name)) else { continue };
        let (_, dummy) = clump.frame_world(fi);
        let scale = if front { def.wheel_scale_front } else { def.wheel_scale_rear };
        let pivot = commands.spawn((Transform::from_translation(dummy.into()), Visibility::default())).id();
        // Wheel mesh faces +X (right side); mirror for the left side.
        let flip = if left { Quat::from_rotation_z(std::f32::consts::PI) } else { Quat::IDENTITY };
        let holder = commands.spawn((Transform::from_rotation(flip), Visibility::default())).id();
        for (m, mat) in &wheel_parts {
            let e = commands.spawn((Mesh3d(m.clone()), MeshMaterial3d(mat.clone()))).id();
            commands.entity(holder).add_child(e);
        }
        commands.entity(pivot).add_child(holder);
        commands.entity(model_root).add_child(pivot);
        let _ = scale;
        let sa_index = match (front, left) {
            (true, true) => 0,
            (false, true) => 1,
            (true, false) => 2,
            (false, false) => 3,
        };
        wheels.push(Wheel { dummy: dummy.into(), sa_index, front, pivot });
    }

    // Rapier proxy (ped / camera only): the COL spheres in Bevy space.
    let raw_col = clump.collision.as_deref().map(col::parse_model).transpose()?;
    let mut shapes: Vec<(Vec3, Quat, Collider)> = Vec::new();
    if let Some(cm) = &raw_col {
        for s in &cm.spheres {
            shapes.push((g2b(s.center), Quat::IDENTITY, Collider::ball(s.radius)));
        }
    }
    if shapes.is_empty() {
        shapes.push((Vec3::new(0.0, 0.3, 0.0), Quat::IDENTITY, Collider::cuboid(1.0, 0.6, 2.3)));
    }

    let seat = clump
        .frames
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case("ped_frontseat"))
        .map(|i| g2b(clump.frame_world(i).1))
        .unwrap_or(Vec3::ZERO);

    // SA physics body.
    let dummy_of = |n: &str| {
        clump
            .frames
            .iter()
            .position(|f| f.name.eq_ignore_ascii_case(n))
            .map(|i| Vec3::from(clump.frame_world(i).1))
            .unwrap_or(Vec3::ZERO)
    };
    let dummies = [dummy_of("wheel_lf_dummy"), dummy_of("wheel_lb_dummy"), dummy_of("wheel_rf_dummy"), dummy_of("wheel_rb_dummy")];
    let mut sa_col = raw_col.as_ref().map(SaColModel::from_col).context("vehicle has no collision")?;
    let vh = VehicleHandling::from_raw(&h);
    let auto = Automobile::new(
        vh,
        def.id as u16,
        def.wheel_scale_front,
        def.wheel_scale_rear,
        dummies,
        &mut sa_col,
        sa.world.surfaces.clone(),
    );
    let tf = Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw));
    let m = gta_matrix(&tf);
    let mut phys = Physical::new(EntityType::Vehicle, m);
    phys.vehicle = Some(VehicleInfo { class: VehicleClass::Automobile, model: def.id as u16, towed_mass: None });
    phys.status = Status::Abandoned;
    auto.setup_physical(&mut phys);
    let id = sa.world.add_body(phys, sa_col, Box::new(auto));

    let car = commands
        .spawn((
            tf,
            Visibility::default(),
            RigidBody::KinematicPositionBased,
            Collider::compound(shapes),
            SaBody::new(id, m),
            Vehicle { name: def.game_name.clone(), sa: id, wheels, speed: 0.0, seat },
        ))
        .add_child(model_root)
        .id();
    Ok(car)
}

// ---------------------------------------------------------------- input

#[allow(clippy::too_many_arguments)]
fn spawn_key(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    world: Res<WorldRes>,
    db: Option<ResMut<VehicleDb>>,
    driving: Res<Driving>,
    mut sa: ResMut<SaPhys>,
    ped: Single<&Transform, With<Ped>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut db) = db else { return };
    if !keys.just_pressed(KeyCode::KeyV) || *mode != Mode::Walk || driving.0.is_some() {
        return;
    }
    let name = SPAWN_LIST[db.next_spawn % SPAWN_LIST.len()];
    db.next_spawn += 1;
    let fwd = ped.rotation * Vec3::NEG_Z;
    let pos = ped.translation + fwd * 5.0 + Vec3::Y * 1.0;
    let yaw = ped.rotation.to_euler(EulerRot::YXZ).0 + FRAC_PI_2;
    let seed = db.next_spawn;
    if let Err(e) = spawn_vehicle(&mut commands, &world.0, &mut sa, &db, &mut meshes, &mut materials, &mut images, name, pos, yaw, seed)
    {
        warn!("spawn {name}: {e:#}");
    }
}

/// `SA_DRIVE=<model>`: once the player can move, spawn that car and get in.
#[allow(clippy::too_many_arguments)]
fn auto_drive(
    mut commands: Commands,
    mut done: Local<bool>,
    world: Res<WorldRes>,
    db: Option<Res<VehicleDb>>,
    mut driving: ResMut<Driving>,
    mut sa: ResMut<SaPhys>,
    ped: Single<(Entity, &Transform, &Ped)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if *done {
        return;
    }
    let (Ok(name), Some(db)) = (std::env::var("SA_DRIVE"), db) else { return };
    let (ped_e, tf, p) = *ped;
    if p.frozen {
        return;
    }
    *done = true;
    let yaw = tf.rotation.to_euler(EulerRot::YXZ).0;
    match spawn_vehicle(&mut commands, &world.0, &mut sa, &db, &mut meshes, &mut materials, &mut images, &name, tf.translation + Vec3::Y, yaw, 0) {
        Ok(car) => {
            info!("SA_DRIVE: spawned {name} as {car:?}");
            driving.0 = Some(car);
            commands.entity(ped_e).insert((ColliderDisabled, Visibility::Hidden)).remove::<CamFollow>();
            commands.entity(car).insert(CamFollow { height: 1.2, dist: 7.0 });
        }
        Err(e) => warn!("SA_DRIVE {name}: {e:#}"),
    }
}

fn enter_exit(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    mut driving: ResMut<Driving>,
    mut ped: Single<(Entity, &mut Transform, &mut Ped), Without<Vehicle>>,
    cars: Query<(Entity, &Transform, &Vehicle)>,
) {
    let (ped_e, ped_tf, ped_c) = &mut *ped;
    if let Some(car) = driving.0 {
        let Ok((_, car_tf, v)) = cars.get(car) else {
            warn!("driven car {car:?} has no Vehicle; leaving it");
            driving.0 = None;
            return;
        };
        // Keep the hidden driver in the seat so the world streams around the car.
        ped_tf.translation = car_tf.transform_point(v.seat);
        if keys.just_pressed(KeyCode::KeyF) && *mode == Mode::Walk {
            let left = car_tf.rotation * Vec3::NEG_X;
            ped_tf.translation = car_tf.translation + left * 2.0 + Vec3::Y * 0.6;
            ped_tf.rotation = Quat::from_rotation_y(car_tf.rotation.to_euler(EulerRot::YXZ).0);
            ped_c.set_velocity_y(0.0);
            commands.entity(*ped_e).remove::<ColliderDisabled>().insert((Visibility::Inherited, CamFollow { height: 0.6, dist: 3.5 }));
            commands.entity(car).remove::<CamFollow>();
            driving.0 = None;
        }
        return;
    }
    if !keys.just_pressed(KeyCode::KeyF) || *mode != Mode::Walk {
        return;
    }
    let nearest = cars
        .iter()
        .map(|(e, tf, _)| (e, tf.translation.distance(ped_tf.translation)))
        .filter(|(_, d)| *d < 5.0)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((car, _)) = nearest {
        driving.0 = Some(car);
        commands.entity(*ped_e).insert((ColliderDisabled, Visibility::Hidden)).remove::<CamFollow>();
        commands.entity(car).insert(CamFollow { height: 1.2, dist: 7.0 });
    }
}

// ---------------------------------------------------------------- SA physics glue

/// Keyboard -> SA control inputs; the driven car is STATUS_PLAYER, others are parked.
fn feed_inputs(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    driving: Res<Driving>,
    mut sa: ResMut<SaPhys>,
    mut cars: Query<(Entity, &mut Vehicle)>,
) {
    let auto = std::env::var("SA_AUTOWALK").is_ok();
    for (e, mut v) in &mut cars {
        let controlled = driving.0 == Some(e) && *mode == Mode::Walk;
        let key = |k: KeyCode| controlled && keys.pressed(k);
        let input = CarInput {
            steer: (key(KeyCode::KeyA) as i32 - key(KeyCode::KeyD) as i32) as f32,
            accelerate: if key(KeyCode::KeyW) || (controlled && auto) { 1.0 } else { 0.0 },
            brake: if key(KeyCode::KeyS) { 1.0 } else { 0.0 },
            handbrake: key(KeyCode::Space),
        };
        if let Some(body) = sa.world.body_mut(v.sa) {
            body.phys.status = if driving.0 == Some(e) { Status::Player } else { Status::Abandoned };
            v.speed = body.phys.move_speed.dot(body.phys.matrix.fwd) * 50.0;
        }
        if let Some(car) = sa.logic_mut::<Automobile>(v.sa) {
            car.input = input;
        }
    }
}

/// Wheel visuals from the SA suspension / wheel spin (CAutomobile::PreRender).
fn update_wheels(sa: Res<SaPhys>, cars: Query<&Vehicle>, mut tfs: Query<&mut Transform, Without<Vehicle>>) {
    for v in &cars {
        let Some(car) = sa.logic::<Automobile>(v.sa) else { continue };
        for w in &v.wheels {
            if let Ok(mut tf) = tfs.get_mut(w.pivot) {
                let i = w.sa_index;
                tf.translation = Vec3::new(w.dummy.x, w.dummy.y, car.hub_z[i]);
                let steer = if w.front { car.steer_angle } else { 0.0 };
                tf.rotation = Quat::from_rotation_z(steer) * Quat::from_rotation_x(car.wheel_rot[i]);
            }
        }
    }
}
