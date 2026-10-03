//! Drivable cars: DFF model with carcols paint, simulated by the ported SA
//! physics (`sa_physics::automobile`).
//!
//! The model hierarchy stays in GTA space (Z-up) under a model root rotated
//! -90° about X. The car entity's transform follows its SA body.

use std::{collections::HashMap, f32::consts::FRAC_PI_2};

use anyhow::{Context, Result};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use sa_formats::{
    col, dff, txd,
    vehicle::{self, CarColors, Handling, VehicleDef},
};
use sa_physics::{
    automobile::{Automobile, CarInput, VehicleHandling},
    collision::{ColModel as SaColModel, ColSphere, Surf},
    damage::{DamageEvent, FlyingKind, flying_component_velocity},
    physical::{EntityType, Matrix as GMatrix, Physical, Status, VehicleClass, VehicleInfo},
    world::{EntityId, PlainLogic},
};

use crate::{
    player::{CamFollow, GameRoot, Mode, Ped, frame_transform, ped_set_in_vehicle, ped_teleport},
    saphys::{SaBody, SaPhys, SaPhysExt, SaStep, gta_matrix, transform_from_gta},
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
            .add_systems(Update, (update_wheels, update_damage, expire_flying_parts).after(SaStep));
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
    /// Car health (1000 new, < 250 burning).
    pub health: f32,
    /// Seat offset (Bevy local space) for placing the hidden driver.
    seat: Vec3,
    comps: Vec<CompVisual>,
    /// Body materials (darkened when the car blows up).
    materials: Vec<Handle<StandardMaterial>>,
    wheel_parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    burnt: bool,
    fire_light: Option<Entity>,
}

/// A damageable component: eDoors (bonnet, boot, doors) or ePanels (wings, windscreen, bumpers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comp {
    Door(usize),
    Panel(usize),
}

fn comp_of_dummy(name: &str) -> Option<Comp> {
    Some(match name {
        "bonnet_dummy" => Comp::Door(0),
        "boot_dummy" => Comp::Door(1),
        "door_lf_dummy" => Comp::Door(2),
        "door_rf_dummy" => Comp::Door(3),
        "door_lr_dummy" => Comp::Door(4),
        "door_rr_dummy" => Comp::Door(5),
        "wing_lf_dummy" => Comp::Panel(0),
        "wing_rf_dummy" => Comp::Panel(1),
        "windscreen_dummy" => Comp::Panel(4),
        "bump_front_dummy" => Comp::Panel(5),
        "bump_rear_dummy" => Comp::Panel(6),
        _ => return None,
    })
}

struct CompVisual {
    comp: Comp,
    /// Dummy (hinge) frame entity and its rest transform.
    dummy: Entity,
    rest: Transform,
    /// Dummy frame in GTA model space (for spawning the flying part).
    model: GMatrix,
    ok: Vec<Entity>,
    dam: Vec<Entity>,
    /// Meshes for a flying copy (the `_dam` version if there is one), relative to the dummy.
    parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>, Transform)>,
    /// Part bounds relative to the dummy (GTA space), for its collision spheres.
    bounds: (Vec3, Vec3),
}

/// A component that came off a car; despawns after its lifetime.
#[derive(Component)]
struct FlyingPart {
    until: f32,
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
    let mut body_materials = Vec::new();
    let mut comps: Vec<CompVisual> = Vec::new();
    // Component dummy ancestor of a frame.
    let comp_dummy = |mut fi: usize| -> Option<(Comp, usize)> {
        for _ in 0..8 {
            let f = &clump.frames[fi];
            if let Some(c) = comp_of_dummy(&f.name.to_ascii_lowercase()) {
                return Some((c, fi));
            }
            if f.parent < 0 {
                return None;
            }
            fi = f.parent as usize;
        }
        None
    };
    for a in &clump.atomics {
        let fi = a.frame as usize;
        let fname = clump.frames[fi].name.to_ascii_lowercase();
        if fname.ends_with("_vlo") {
            continue;
        }
        let damaged = fname.ends_with("_dam");
        let geo = &clump.geometries[a.geometry as usize];
        let comp = comp_dummy(fi);
        for (mi, mesh) in geometry_meshes(geo) {
            let material = make_material(&geo.materials[mi]);
            body_materials.push(material.clone());
            let part = (meshes.add(mesh), material);
            if fname == "wheel" {
                wheel_parts.push(part);
                continue;
            }
            // `_dam` models start hidden; damage swaps them in.
            let vis = if damaged { Visibility::Hidden } else { Visibility::Inherited };
            let e = commands.spawn((Mesh3d(part.0.clone()), MeshMaterial3d(part.1.clone()), vis)).id();
            commands.entity(frames[fi]).add_child(e);
            if let Some((c, di)) = comp {
                let idx = match comps.iter().position(|x| x.comp == c) {
                    Some(i) => i,
                    None => {
                        let (rot, pos) = clump.frame_world(di);
                        comps.push(CompVisual {
                            comp: c,
                            dummy: frames[di],
                            rest: frame_transform(&clump.frames[di]),
                            model: GMatrix { right: rot[0].into(), fwd: rot[1].into(), up: rot[2].into(), pos: pos.into() },
                            ok: Vec::new(),
                            dam: Vec::new(),
                            parts: Vec::new(),
                            bounds: (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                        });
                        comps.len() - 1
                    }
                };
                let cv = &mut comps[idx];
                if damaged { cv.dam.push(e) } else { cv.ok.push(e) }
                // Flying copy: prefer the damaged model.
                let local = if fi == di { Transform::IDENTITY } else { frame_transform(&clump.frames[fi]) };
                if damaged || cv.dam.is_empty() {
                    if damaged && cv.dam.len() == 1 {
                        cv.parts.clear();
                    }
                    cv.parts.push((part.0.clone(), part.1.clone(), local));
                }
                for v in &geo.positions {
                    let q = local.transform_point(Vec3::from(*v));
                    cv.bounds.0 = cv.bounds.0.min(q);
                    cv.bounds.1 = cv.bounds.1.max(q);
                }
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

    let raw_col = clump.collision.as_deref().map(col::parse_model).transpose()?;

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
    let mut auto = Automobile::new(
        vh,
        def.id as u16,
        def.wheel_scale_front,
        def.wheel_scale_rear,
        dummies,
        &mut sa_col,
        sa.world.surfaces.clone(),
    );
    for c in &comps {
        if let Comp::Door(d) = c.comp {
            auto.door_hinges[d] = Some(c.model.pos);
        }
    }
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
            SaBody::new(id, m),
            Vehicle {
                name: def.game_name.clone(),
                sa: id,
                wheels,
                speed: 0.0,
                health: 1000.0,
                seat,
                comps,
                materials: body_materials,
                wheel_parts,
                burnt: false,
                fire_light: None,
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
    let pos = tf.translation + Vec3::Y;
    match spawn_vehicle(&mut commands, &world.0, &mut sa, &db, &mut meshes, &mut materials, &mut images, &name, pos, yaw, 0) {
        Ok(car) => {
            info!("SA_DRIVE: spawned {name} as {car:?}");
            driving.0 = Some(car);
            ped_set_in_vehicle(&mut sa, p.sa, true);
            commands.entity(ped_e).insert(Visibility::Hidden).remove::<CamFollow>();
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
    mut sa: ResMut<SaPhys>,
    ped: Single<(Entity, &Transform, &Ped), Without<Vehicle>>,
    cars: Query<(Entity, &Transform, &Vehicle)>,
) {
    let (ped_e, ped_tf, ped) = *ped;
    if let Some(car) = driving.0 {
        let Ok((_, car_tf, v)) = cars.get(car) else {
            warn!("driven car {car:?} has no Vehicle; leaving it");
            driving.0 = None;
            ped_set_in_vehicle(&mut sa, ped.sa, false);
            return;
        };
        // Keep the hidden driver in the seat so the world streams around the car.
        ped_teleport(&mut sa, ped.sa, car_tf.transform_point(v.seat), None);
        if keys.just_pressed(KeyCode::KeyF) && *mode == Mode::Walk {
            let left = car_tf.rotation * Vec3::NEG_X;
            let yaw = car_tf.rotation.to_euler(EulerRot::YXZ).0;
            ped_set_in_vehicle(&mut sa, ped.sa, false);
            ped_teleport(&mut sa, ped.sa, car_tf.translation + left * 2.0 + Vec3::Y * 0.6, Some(yaw));
            commands.entity(ped_e).insert((Visibility::Inherited, CamFollow { height: 0.6, dist: 3.5 }));
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
        ped_set_in_vehicle(&mut sa, ped.sa, true);
        commands.entity(ped_e).insert(Visibility::Hidden).remove::<CamFollow>();
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

// ---------------------------------------------------------------- damage visuals

/// Component visibility, door hinges, flying parts, burnt look and fire light.
#[allow(clippy::too_many_arguments)]
fn update_damage(
    mut commands: Commands,
    time: Res<Time>,
    mut sa: ResMut<SaPhys>,
    mut cars: Query<(Entity, &mut Vehicle)>,
    mut vis: Query<&mut Visibility>,
    mut tfs: Query<&mut Transform, Without<Vehicle>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (car_e, mut v) in &mut cars {
        let Some(car_phys) = sa.world.body(v.sa).map(|b| b.phys.clone()) else { continue };
        let Some(auto) = sa.logic_mut::<Automobile>(v.sa) else { continue };
        let events = std::mem::take(&mut auto.damage.events);
        let dm = auto.damage.dm.clone();
        let doors = auto.damage.doors;
        let on_fire = auto.damage.on_fire;
        v.health = auto.damage.health;

        for c in &v.comps {
            // 0 = ok model, 1 = damaged model, 2 = gone.
            let state = match c.comp {
                Comp::Door(d) => match dm.doors[d] {
                    0 | 1 => 0,
                    2 | 3 => 1,
                    _ => 2,
                },
                Comp::Panel(p) => match dm.panels[p] {
                    0 => 0,
                    1 | 2 => 1,
                    _ => 2,
                },
            };
            let show_dam = state == 1 && !c.dam.is_empty();
            for &e in &c.ok {
                if let Ok(mut x) = vis.get_mut(e) {
                    *x = if state == 0 || (state == 1 && !show_dam) { Visibility::Inherited } else { Visibility::Hidden };
                }
            }
            for &e in &c.dam {
                if let Ok(mut x) = vis.get_mut(e) {
                    *x = if show_dam { Visibility::Inherited } else { Visibility::Hidden };
                }
            }
            if let (Comp::Door(d), Ok(mut tf)) = (c.comp, tfs.get_mut(c.dummy)) {
                let door = doors[d];
                let axis = if door.axis == 0 { Vec3::X } else { Vec3::Z };
                tf.rotation = c.rest.rotation * Quat::from_axis_angle(axis, door.angle);
            }
        }
        for w in &v.wheels {
            if let Ok(mut x) = vis.get_mut(w.pivot) {
                *x = if dm.wheels[w.sa_index] == 2 { Visibility::Hidden } else { Visibility::Inherited };
            }
        }

        for ev in events {
            match ev {
                DamageEvent::DoorOff(d, kind) => {
                    if let Some(c) = v.comps.iter().find(|c| c.comp == Comp::Door(d)) {
                        spawn_flying_part(&mut commands, &mut sa, &time, &car_phys, c, kind, false);
                    }
                }
                DamageEvent::PanelOff(p, kind) => {
                    if let Some(c) = v.comps.iter().find(|c| c.comp == Comp::Panel(p)) {
                        spawn_flying_part(&mut commands, &mut sa, &time, &car_phys, c, kind, p == 4);
                    }
                }
                DamageEvent::WheelOff(w) => {
                    if let Some(wh) = v.wheels.iter().find(|x| x.sa_index == w) {
                        let model = GMatrix { pos: wh.dummy, ..GMatrix::IDENTITY };
                        let parts = v.wheel_parts.iter().map(|(m, mat)| (m.clone(), mat.clone(), Transform::IDENTITY)).collect();
                        let c = CompVisual {
                            comp: Comp::Panel(99),
                            dummy: wh.pivot,
                            rest: Transform::IDENTITY,
                            model,
                            ok: Vec::new(),
                            dam: Vec::new(),
                            parts,
                            bounds: (Vec3::splat(-0.35), Vec3::splat(0.35)),
                        };
                        spawn_flying_part(&mut commands, &mut sa, &time, &car_phys, &c, FlyingKind::Wheel, false);
                    }
                }
                DamageEvent::Exploded => {
                    if !v.burnt {
                        v.burnt = true;
                        for h in &v.materials {
                            if let Some(mut m) = materials.get_mut(h) {
                                let c = m.base_color.to_srgba();
                                m.base_color = Color::srgba(c.red * 0.15, c.green * 0.14, c.blue * 0.13, c.alpha);
                                m.reflectance = 0.1;
                                m.perceptual_roughness = 0.9;
                            }
                        }
                    }
                }
                DamageEvent::OnFire => {}
            }
        }

        // A flickering orange light while the car burns.
        match (on_fire, v.fire_light) {
            (true, None) => {
                let light = commands
                    .spawn((
                        PointLight { color: Color::srgb(1.0, 0.45, 0.1), intensity: 400_000.0, range: 12.0, ..default() },
                        Transform::from_xyz(0.0, 1.2, -1.2),
                    ))
                    .id();
                commands.entity(car_e).add_child(light);
                v.fire_light = Some(light);
            }
            (false, Some(l)) => {
                commands.entity(l).despawn();
                v.fire_light = None;
            }
            _ => {}
        }
    }
}

/// SpawnFlyingComponent (0x6A8580): the part becomes a 10 kg SA object.
#[allow(clippy::too_many_arguments)]
fn spawn_flying_part(
    commands: &mut Commands,
    sa: &mut SaPhys,
    time: &Time,
    car: &Physical,
    c: &CompVisual,
    kind: FlyingKind,
    windscreen: bool,
) {
    if c.parts.is_empty() {
        return;
    }
    let m = car.matrix.mul(&c.model);
    let (v, w) = flying_component_velocity(car, m.pos, kind, windscreen);
    let mut phys = Physical::new(EntityType::Object, m);
    phys.mass = 10.0;
    phys.turn_mass = if kind == FlyingKind::Wheel { 5.0 } else { 25.0 };
    phys.air_resistance = if kind == FlyingKind::Wheel { 0.99 } else { 0.97 };
    phys.elasticity = 0.1;
    phys.move_speed = v;
    phys.turn_speed = w;
    // Collision: spheres along the part's longest axis.
    let (lo, hi) = c.bounds;
    let (lo, hi) = if lo.x > hi.x { (Vec3::splat(-0.3), Vec3::splat(0.3)) } else { (lo, hi) };
    let size = hi - lo;
    let centre = (lo + hi) * 0.5;
    let axis = if size.x >= size.y && size.x >= size.z {
        Vec3::X
    } else if size.y >= size.z {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let long = size.dot(axis);
    let r = ((size - axis * long).max_element() * 0.5).clamp(0.08, 0.4);
    let spheres = [-0.33f32, 0.0, 0.33]
        .iter()
        .map(|k| ColSphere { center: centre + axis * (long * k), radius: r, surf: Surf { material: 63, piece: 0, lighting: 0 } })
        .collect();
    let col = SaColModel {
        bbox_min: lo - Vec3::splat(r),
        bbox_max: hi + Vec3::splat(r),
        bound_center: centre,
        bound_radius: long * 0.5 + r,
        spheres,
        ..Default::default()
    };
    let id = sa.world.add_body(phys, col, Box::new(PlainLogic));
    let tf = transform_from_gta(&m);
    let root = commands.spawn((Transform::from_rotation(Quat::from_rotation_x(-FRAC_PI_2)), Visibility::default())).id();
    for (mesh, mat, local) in &c.parts {
        let e = commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), *local)).id();
        commands.entity(root).add_child(e);
    }
    commands
        .spawn((tf, Visibility::default(), SaBody::new(id, m), FlyingPart { until: time.elapsed_secs() + 20.0 }))
        .add_child(root);
}

fn expire_flying_parts(mut commands: Commands, time: Res<Time>, parts: Query<(Entity, &FlyingPart)>) {
    for (e, p) in &parts {
        if time.elapsed_secs() > p.until {
            commands.entity(e).despawn();
        }
    }
}
