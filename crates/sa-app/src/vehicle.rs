//! Drivable cars: DFF model with carcols paint, embedded COL collider, and a
//! raycast-suspension / tire model driven by handling.cfg.
//!
//! Like the ped, the model hierarchy stays in GTA space (Z-up) under a model
//! root rotated -90° about X; physics runs on the Bevy-space rigid body.

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

use crate::{
    player::{CamFollow, GameRoot, Mode, Ped, frame_transform},
    stream::{convert_texture, make_image},
    world::{WorldRes, g2b},
};

const GRAVITY: f32 = 9.81;
/// Cars cycled by the spawn key.
const SPAWN_LIST: &[&str] = &["greenwoo", "sabre", "infernus", "bobcat", "savanna", "elegy", "banshee", "sultan"];

pub struct VehiclePlugin;

impl Plugin for VehiclePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Driving>()
            .add_systems(Startup, load_vehicle_db)
            .add_systems(Update, (spawn_key, auto_drive, enter_exit, drive_vehicles, update_wheels).chain());
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
    /// Wheel centre at rest, GTA model space.
    dummy: Vec3,
    front: bool,
    driven: bool,
    radius: f32,
    pivot: Entity,
    /// Current offset of the wheel centre along model Z (GTA), for visuals.
    offset: f32,
    spin: f32,
    grounded: bool,
}

/// Velocity at the start of the frame, i.e. before this frame's physics step.
#[derive(Component, Default, Clone, Copy)]
pub struct PrevVelocity {
    pub linear: Vec3,
    pub angular: Vec3,
}

#[derive(Component)]
pub struct Vehicle {
    pub name: String,
    h: Handling,
    wheels: Vec<Wheel>,
    steer: f32,
    pub speed: f32,
    throttle: f32,
    brake: f32,
    handbrake: bool,
    /// Seat offset (Bevy local space) for placing the hidden driver.
    seat: Vec3,
}

impl Vehicle {
    pub fn mass(&self) -> f32 {
        self.h.mass
    }
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
    let model_root = commands.spawn((Transform::from_rotation(Quat::from_rotation_x(-FRAC_PI_2)), Visibility::default())).id();
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
        let driven = match h.drive_type {
            '4' => true,
            'F' => front,
            _ => !front,
        };
        wheels.push(Wheel {
            dummy: dummy.into(),
            front,
            driven,
            radius: scale * 0.5,
            pivot,
            offset: 0.0,
            spin: 0.0,
            grounded: false,
        });
    }

    // Collider: the COL spheres, lifted so the body clears the wheels. Rounded
    // contacts let the suspension ride over curbs instead of the flat-bottomed
    // COL mesh hitting them like a wall. The mesh hull is only a fallback.
    let lowest_wheel = wheels.iter().map(|w| w.dummy.z).fold(0.0f32, f32::min);
    let min_bottom = lowest_wheel - 0.1;
    let mut shapes: Vec<(Vec3, Quat, Collider)> = Vec::new();
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    if let Some(raw) = &clump.collision {
        if let Ok(cm) = col::parse_model(raw) {
            for s in &cm.spheres {
                let mut c = s.center;
                c[2] = c[2].max(min_bottom + s.radius);
                shapes.push((g2b(c), Quat::IDENTITY, Collider::ball(s.radius)));
            }
            for b in &cm.boxes {
                let (a, c) = (g2b(b.min), g2b(b.max));
                let half = ((c - a).abs() * 0.5).max(Vec3::splat(0.02));
                shapes.push(((a + c) * 0.5, Quat::IDENTITY, Collider::cuboid(half.x, half.y, half.z)));
            }
            let pts: Vec<Vec3> = cm.vertices.iter().map(|&v| g2b(v)).collect();
            if shapes.is_empty() {
                if let Some(hull) = (pts.len() >= 4).then(|| Collider::convex_hull(&pts)).flatten() {
                    shapes.push((Vec3::ZERO, Quat::IDENTITY, hull));
                }
            }
            let (a, c) = (g2b(cm.min), g2b(cm.max));
            lo = a.min(c);
            hi = a.max(c);
        }
    }
    if shapes.is_empty() {
        lo = Vec3::new(-1.0, -0.4, -2.3);
        hi = Vec3::new(1.0, 0.8, 2.3);
        let half = (hi - lo) * 0.5;
        shapes.push(((lo + hi) * 0.5, Quat::IDENTITY, Collider::cuboid(half.x, half.y, half.z)));
    }
    let size = hi - lo;
    // Box inertia, scaled by handling turn mass.
    let m_rot = h.turn_mass.max(h.mass) * 0.6;
    let inertia = Vec3::new(
        m_rot / 12.0 * (size.y * size.y + size.z * size.z),
        m_rot / 12.0 * (size.x * size.x + size.z * size.z),
        m_rot / 12.0 * (size.x * size.x + size.y * size.y),
    );
    let com = g2b(h.centre_of_mass);

    let seat = clump
        .frames
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case("ped_frontseat"))
        .map(|i| g2b(clump.frame_world(i).1))
        .unwrap_or(Vec3::ZERO);

    let car = commands
        .spawn((
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::default(),
            RigidBody::Dynamic,
            Collider::compound(shapes),
            ColliderMassProperties::MassProperties(MassProperties {
                local_center_of_mass: com,
                mass: h.mass,
                principal_inertia_local_frame: Quat::IDENTITY,
                principal_inertia: inertia,
            }),
            Velocity::default(),
            PrevVelocity::default(),
            // Slide along walls and bounce a little, instead of sticking to them.
            Friction { coefficient: 0.3, combine_rule: CoefficientCombineRule::Min },
            Restitution { coefficient: 0.2, combine_rule: CoefficientCombineRule::Max },
            ExternalForce::default(),
            ReadMassProperties::default(),
            Damping { linear_damping: 0.02, angular_damping: 0.3 },
            Ccd::enabled(),
            Sleeping::disabled(),
            Vehicle {
                name: def.game_name.clone(),
                h,
                wheels,
                steer: 0.0,
                speed: 0.0,
                throttle: 0.0,
                brake: 0.0,
                handbrake: false,
                seat,
            },
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
    if let Err(e) = spawn_vehicle(&mut commands, &world.0, &db, &mut meshes, &mut materials, &mut images, name, pos, yaw, seed)
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
    match spawn_vehicle(&mut commands, &world.0, &db, &mut meshes, &mut materials, &mut images, &name, tf.translation + Vec3::Y, yaw, 0) {
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

// ---------------------------------------------------------------- physics

pub fn drive_vehicles(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    driving: Res<Driving>,
    rapier: ReadRapierContext,
    mut cars: Query<(
        Entity,
        &Transform,
        &Velocity,
        &mut PrevVelocity,
        &ReadMassProperties,
        &mut ExternalForce,
        &mut Vehicle,
    )>,
) {
    let dt = time.delta_secs().max(1e-4);
    let Ok(ctx) = rapier.single() else { return };
    let auto = std::env::var("SA_AUTOWALK").is_ok();
    for (e, tf, vel, mut prev, mp, mut ext, mut v) in &mut cars {
        *prev = PrevVelocity { linear: vel.linear, angular: vel.angular };
        let v = &mut *v;
        let controlled = driving.0 == Some(e) && *mode == Mode::Walk;
        let key = |k: KeyCode| controlled && keys.pressed(k);
        let h = v.h.clone();

        let fwd = tf.rotation * Vec3::NEG_Z;
        let up = tf.rotation * Vec3::Y;
        let speed = vel.linear.dot(fwd);
        v.speed = speed;

        // Driver input.
        let accel_in = key(KeyCode::KeyW) || (controlled && auto);
        let brake_in = key(KeyCode::KeyS);
        v.handbrake = key(KeyCode::Space);
        (v.throttle, v.brake) = match (accel_in, brake_in) {
            (true, _) => (1.0, 0.0),
            (false, true) if speed > 1.0 => (0.0, 1.0),
            (false, true) => (-0.6, 0.0), // reverse
            _ => (0.0, 0.0),
        };
        let steer_in = (key(KeyCode::KeyA) as i32 - key(KeyCode::KeyD) as i32) as f32;
        let lock = h.steering_lock.to_radians() / (1.0 + speed.abs() / 25.0);
        let target = steer_in * lock;
        v.steer += (target - v.steer) * (1.0 - (-10.0 * dt).exp());

        let mass = h.mass;
        let com = tf.transform_point(mp.get().local_center_of_mass);
        let travel = (h.susp_upper - h.susp_lower).max(0.05);
        let n_wheels = v.wheels.len().max(1) as f32;
        let k = h.susp_force * mass * GRAVITY / n_wheels * 2.0 / travel;
        let c_damp = 2.0 * (k * mass / n_wheels).sqrt() * (h.susp_damping * 3.0).clamp(0.2, 1.2);
        let mu_base = h.traction_mult * 1.7;
        let driven = v.wheels.iter().filter(|w| w.driven).count().max(1) as f32;
        let vmax = h.max_velocity / 3.6;
        let drive_total = mass * h.engine_accel * 0.25 * v.throttle * (1.0 - (speed / vmax).clamp(0.0, 1.0).powi(2));
        let brake_total = mass * h.brake_decel * v.brake;

        let mut force = Vec3::ZERO;
        let mut torque = Vec3::ZERO;
        let filter = QueryFilter::default().exclude_rigid_body(e).exclude_sensors();

        for w in v.wheels.iter_mut() {
            let ray_len = travel + w.radius;
            let top = tf.transform_point(g2b((w.dummy + Vec3::Z * h.susp_upper).into()));
            let down = -up;
            w.grounded = false;
            w.offset = h.susp_lower;
            let Some((_, hit)) = ctx.cast_ray_and_get_normal(top, down, ray_len, true, filter) else { continue };
            w.grounded = true;
            let d = hit.time_of_impact;
            w.offset = h.susp_upper - (d - w.radius);
            let compression = ray_len - d;
            let p = hit.point;
            let pv = vel.linear + vel.angular.cross(p - com);

            // Suspension.
            let fz = (k * compression + c_damp * pv.dot(down)).max(0.0);

            // Tire frame on the ground plane.
            let n = hit.normal;
            let steer = if w.front { v.steer } else { 0.0 };
            let heading = Quat::from_axis_angle(up, steer) * fwd;
            let f = (heading - n * heading.dot(n)).normalize_or_zero();
            let s = n.cross(f).normalize_or_zero();
            let v_long = pv.dot(f);
            let v_lat = pv.dot(s);

            let front_share = h.traction_bias * 2.0;
            let mut mu = mu_base * if w.front { front_share } else { 2.0 - front_share };
            if v.handbrake && !w.front {
                mu *= 0.45;
            }
            let mut f_long = if w.driven { drive_total / driven } else { 0.0 };
            let brake_share = if w.front { h.brake_bias } else { 1.0 - h.brake_bias } * 2.0 / n_wheels;
            let mut brake = brake_total * brake_share;
            if v.handbrake && !w.front {
                brake += mass * 6.0 / n_wheels;
            }
            // Brakes oppose rolling, never reversing it within a step.
            let max_stop = v_long.abs() * mass / n_wheels / dt;
            f_long -= v_long.signum() * brake.min(max_stop);
            f_long -= v_long * mass / n_wheels * 0.05; // rolling resistance
            let f_lat = -v_lat * mass / n_wheels * 12.0;
            // Friction circle.
            let mut horiz = Vec2::new(f_long, f_lat);
            let limit = mu * fz;
            if horiz.length() > limit {
                horiz = horiz.normalize() * limit;
            }

            let wf = up * fz + f * horiz.x + s * horiz.y;
            // Apply horizontal forces a little above the contact to reduce rollover.
            let app = p + up * (w.radius * 0.6);
            force += wf;
            torque += (app - com).cross(f * horiz.x + s * horiz.y) + (p - com).cross(up * fz);
            w.spin -= v_long / w.radius * dt;
        }
        // Air drag.
        force -= vel.linear * vel.linear.length() * h.drag_mult * 0.3;
        ext.force = force;
        ext.torque = torque;
    }
}

fn update_wheels(cars: Query<&Vehicle>, mut tfs: Query<&mut Transform, Without<Vehicle>>) {
    for v in &cars {
        for w in &v.wheels {
            if let Ok(mut tf) = tfs.get_mut(w.pivot) {
                tf.translation = w.dummy + Vec3::Z * w.offset;
                let steer = if w.front { v.steer } else { 0.0 };
                tf.rotation = Quat::from_rotation_z(steer) * Quat::from_rotation_x(w.spin);
            }
        }
    }
}
