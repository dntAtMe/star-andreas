//! Particle FX: runs the `sa-fx` port of SA's FxManager and draws its quads.
//!
//! Each frame (PostUpdate, after transforms are interpolated):
//! 1. apply the FX commands the SA world queued (CreateFxSystem, Play, SetConstTime, ...);
//! 2. `FxManager::update` with `dt = min(frame time, 0.06 s)` (CTimer's timestep cap);
//! 3. `FxManager::render` → one mesh per textured batch, drawn after the scene in
//!    the original order (no sorting, z-write off, z-test on).
//!
//! Point lights from explosions and fires become Bevy point lights.

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    camera::primitives::Frustum,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    transform::TransformSystems,
};
use sa_fx::{BlendFactor, Camera as FxCamera, Env, FxManager, PrtMult, SysId};
use sa_physics::{
    effects::{FxCmd, FxHandle},
    world::EntityId,
};

use crate::{
    player::GameRoot,
    saphys::{SaPhys, SaSync},
    stream::{convert_texture, make_image},
    world::b2g,
};

/// CTimer caps the timestep at 3 frames (0.06 s).
const MAX_DT: f32 = 0.06;
/// Point lights kept alive for explosions and fires.
const LIGHT_POOL: usize = 8;

pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init)
            .add_systems(
                PostUpdate,
                (update_fx, draw_fx.in_set(FxDrawn), update_lights).chain().after(SaSync).after(TransformSystems::Propagate),
            );
    }
}

/// After the FX quads are built (heat_haze_needed is known).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FxDrawn;

pub fn fx_camera_of(gt: &GlobalTransform, frustum: &Frustum) -> FxCamera {
    fx_camera(gt, frustum)
}

/// A gameplay handle and what is needed to (re)create its system.
struct Slot {
    sys: Option<SysId>,
    name: &'static str,
    offset: Vec3,
    attach: Option<EntityId>,
    ignore_bb: bool,
    /// Play requested (true = PlayAndKill) before the system could be created.
    played: Option<bool>,
}

/// `Fx_c::InitStaticSystems` (0x49E660): created stopped at the origin, never played;
/// containers for hand-added particles.
const STATIC_SYSTEMS: [&str; 17] = [
    "prt_blood",
    "prt_boatsplash",
    "prt_bubble",
    "prt_cardebris",
    "prt_collisionsmoke",
    "prt_gunshell",
    "prt_sand",
    "prt_sand2",
    "prt_smoke_huge",
    "prt_smokeII_3_expand",
    "prt_spark",
    "prt_spark_2",
    "prt_splash",
    "prt_wake",
    "prt_watersplash",
    "prt_wheeldirt",
    "prt_glass",
];

#[derive(Resource)]
pub struct Fx {
    pub man: FxManager,
    statics: HashMap<&'static str, SysId>,
    slots: HashMap<FxHandle, Slot>,
    textures: HashMap<String, Handle<Image>>,
    /// Pooled batch entities and their meshes / materials.
    batches: Vec<(Entity, Handle<Mesh>, Handle<StandardMaterial>)>,
    lights: Vec<Entity>,
}

fn init(mut commands: Commands, root: Res<GameRoot>, mut images: ResMut<Assets<Image>>) {
    let mut load = || -> anyhow::Result<(FxManager, HashMap<String, Handle<Image>>)> {
        let text = std::fs::read_to_string(root.0.join("models/effects.fxp"))?;
        let man = FxManager::new(&sa_formats::fxp::parse(&text)?)?;
        let txd = sa_formats::txd::parse(&std::fs::read(root.0.join("models/effectsPC.txd"))?)?;
        let textures = txd
            .into_iter()
            .filter_map(|t| convert_texture(t, false))
            .map(|t| (t.name.clone(), images.add(make_image(t))))
            .collect();
        Ok((man, textures))
    };
    match load() {
        Ok((mut man, textures)) => {
            info!("fx: {} systems, {} textures", man.bps.len(), textures.len());
            let statics =
                STATIC_SYSTEMS.iter().filter_map(|&n| man.create(n, Vec3::ZERO, None, true).map(|id| (n, id))).collect();
            commands.insert_resource(Fx {
                man,
                statics,
                slots: HashMap::new(),
                textures,
                batches: Vec::new(),
                lights: Vec::new(),
            });
        }
        Err(e) => warn!("fx disabled: {e:#}"),
    }
}

/// GTA-space matrix of a Bevy transform (X right, Y up, -Z forward in Bevy).
fn gta_affine(gt: &GlobalTransform) -> bevy::math::Affine3A {
    let (_, rot, t) = gt.to_scale_rotation_translation();
    let g = |v: Vec3| Vec3::from(b2g(v));
    bevy::math::Affine3A::from_cols(
        g(rot * Vec3::X).into(),
        g(rot * Vec3::NEG_Z).into(),
        g(rot * Vec3::Y).into(),
        g(t).into(),
    )
}

fn entity_key(id: EntityId) -> u64 {
    match id {
        EntityId::Body(i) => i as u64,
        EntityId::Building(i) => (1 << 32) | i as u64,
    }
}

fn fx_camera(gt: &GlobalTransform, frustum: &Frustum) -> FxCamera {
    let g = |v: Vec3| Vec3::from(b2g(v));
    let planes = std::array::from_fn(|i| {
        let h = &frustum.half_spaces[i];
        let n: Vec3 = h.normal().into();
        (-g(n), h.d())
    });
    FxCamera {
        pos: g(gt.translation()),
        // RenderWare's camera "right" points to screen-left.
        right: g(-gt.right().as_vec3()),
        up: g(gt.up().as_vec3()),
        at: g(gt.forward().as_vec3()),
        planes,
    }
}

fn update_fx(
    fx: Option<ResMut<Fx>>,
    mut sa: ResMut<SaPhys>,
    time: Res<Time>,
    camera: Single<(&GlobalTransform, &Frustum), With<Camera3d>>,
    bodies: Query<&GlobalTransform>,
) {
    let Some(mut fx) = fx else { return };
    let fx = &mut *fx;
    let (cam_gt, frustum) = *camera;
    let cam = fx_camera(cam_gt, frustum);
    sa.world.camera_pos = cam.pos;
    sa.world.camera_fwd = cam.at;
    sa.world.camera_right = Vec3::from(b2g(cam_gt.right().as_vec3()));
    sa.world.camera_planes = cam.planes;
    sa.world.camera_orientation = cam.at.x.atan2(cam.at.y);

    // Current (interpolated) matrices of the bodies FX are attached to.
    let parent_of = |sa: &SaPhys, id: EntityId| {
        sa.entities.get(&id).and_then(|&e| bodies.get(e).ok()).map(gta_affine)
    };

    let cmds = std::mem::take(&mut sa.world.effects.cmds);
    let sa: &SaPhys = &sa;
    for c in cmds {
        match c {
            FxCmd::Create { h, name, offset, attach, ignore_bounding } => {
                let mut slot = Slot { sys: None, name, offset, attach, ignore_bb: ignore_bounding, played: None };
                try_create(&mut fx.man, &mut slot, sa, &parent_of);
                fx.slots.insert(h, slot);
            }
            FxCmd::Play(h) | FxCmd::PlayAndKill(h) => {
                let and_kill = matches!(c, FxCmd::PlayAndKill(_));
                let Some(slot) = fx.slots.get_mut(&h) else { continue };
                match slot.sys {
                    Some(s) if and_kill => fx.man.play_and_kill(s),
                    Some(s) => fx.man.play(s),
                    None => slot.played = Some(and_kill),
                }
            }
            FxCmd::Kill(h) => {
                if let Some(s) = fx.slots.remove(&h).and_then(|s| s.sys) {
                    fx.man.kill(s);
                }
            }
            FxCmd::SetConstTime(h, on, t) => {
                if let Some(s) = live(fx, h, sa, &parent_of) {
                    fx.man.set_const_time(s, on, t);
                }
            }
            FxCmd::SetVelAdd(h, v) => {
                if let Some(s) = live(fx, h, sa, &parent_of) {
                    fx.man.set_vel_add(s, v);
                }
            }
            FxCmd::AddParticle(a) => {
                let Some(&id) = fx.statics.get(a.system) else { continue };
                if let Some(k) = a.prim {
                    for i in 0..4 {
                        fx.man.enable_prim(id, i, i == k as usize);
                    }
                }
                let m = a.mult;
                let mult = PrtMult { rgba: m.rgba, size: m.size, ang_change: m.ang_change, life: m.life };
                let w = &sa.world.weather;
                let env = Env { wind_dir: w.wind_dir, wind: w.wind, rain: w.rain };
                fx.man.add_particle(
                    id,
                    a.pos,
                    a.vel,
                    a.time_since,
                    &mult,
                    a.z_rot,
                    a.light_mult,
                    a.light_mult_limit,
                    a.local,
                    &env,
                );
            }
            FxCmd::SetOffsetPos(h, p) => {
                if let Some(s) = live(fx, h, sa, &parent_of) {
                    fx.man.set_offset_pos(s, p);
                }
            }
        }
    }
    // Forget handles whose systems finished (PlayAndKill).
    let man = &fx.man;
    fx.slots.retain(|_, s| s.sys.is_none_or(|id| man.is_alive(id)));

    let dt = time.delta_secs().min(MAX_DT);
    let w = &sa.world.weather;
    let env = Env { wind_dir: w.wind_dir, wind: w.wind, rain: w.rain };
    fx.man.update(&cam, dt, &env, |key| {
        let id = if key >> 32 == 0 { EntityId::Body(key as u32) } else { EntityId::Building(key as u32) };
        parent_of(sa, id)
    });
}

/// The slot's system, creating it now if CreateFxSystem failed before (the game
/// retries every frame while its pointer is null).
fn live(
    fx: &mut Fx,
    h: FxHandle,
    sa: &SaPhys,
    parent_of: &impl Fn(&SaPhys, EntityId) -> Option<bevy::math::Affine3A>,
) -> Option<SysId> {
    let slot = fx.slots.get_mut(&h)?;
    if slot.sys.is_none() {
        try_create(&mut fx.man, slot, sa, parent_of);
    }
    slot.sys
}

fn try_create(
    man: &mut FxManager,
    slot: &mut Slot,
    sa: &SaPhys,
    parent_of: &impl Fn(&SaPhys, EntityId) -> Option<bevy::math::Affine3A>,
) {
    let parent = match slot.attach {
        Some(id) => match parent_of(sa, id) {
            Some(m) => Some((entity_key(id), m)),
            None => return,
        },
        None => None,
    };
    slot.sys = man.create(slot.name, slot.offset, parent, slot.ignore_bb);
    if let (Some(s), Some(and_kill)) = (slot.sys, slot.played) {
        if and_kill { man.play_and_kill(s) } else { man.play(s) }
    }
}

fn draw_fx(
    mut commands: Commands,
    fx: Option<ResMut<Fx>>,
    sa: Res<SaPhys>,
    camera: Single<(&GlobalTransform, &Frustum), With<Camera3d>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut vis: Query<(&mut Visibility, &mut Transform)>,
) {
    let Some(mut fx) = fx else { return };
    let fx = &mut *fx;
    let (cam_gt, frustum) = *camera;
    let cam = fx_camera(cam_gt, frustum);
    // FxManager_c::Render: (1 - DNBalance) * 0.6 + 0.4.
    let brightness = (1.0 - sa.world.clock.dn_balance()) * 0.6 + 0.4;
    let batches = fx.man.render(&cam, brightness);
    // Vertices relative to the camera; the entities sit at the camera so every batch
    // sorts after the scene, and increasing depth bias keeps the original order.
    let origin = cam_gt.translation();
    for (i, b) in batches.iter().enumerate() {
        if i == fx.batches.len() {
            let mesh = meshes.add(
                Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                    // One degenerate triangle: empty buffers upset the render slab allocator.
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
                    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; 3])
                    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3])
                    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
                    .with_inserted_indices(Indices::U32(vec![0, 1, 2])),
            );
            let mat = materials.add(StandardMaterial { unlit: true, double_sided: true, cull_mode: None, ..default() });
            let e = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(mat.clone()),
                    Transform::default(),
                    Visibility::Hidden,
                    // The Aabb is not refreshed when the mesh changes.
                    bevy::camera::visibility::NoFrustumCulling,
                ))
                .id();
            fx.batches.push((e, mesh, mat));
            continue; // spawned this frame; filled from the next one
        }
        let (e, mesh_h, mat_h) = &fx.batches[i];
        let Ok((mut v, mut tf)) = vis.get_mut(*e) else { continue };
        *v = Visibility::Visible;
        tf.translation = origin;
        let n = b.verts.len();
        let pos: Vec<[f32; 3]> = b.verts.iter().map(|v| (Vec3::from(crate::world::g2b(v.pos.to_array())) - origin).to_array()).collect();
        let uv: Vec<[f32; 2]> = b.verts.iter().map(|v| v.uv).collect();
        let col: Vec<[f32; 4]> = b
            .verts
            .iter()
            .map(|v| {
                let c = Color::srgba_u8(v.rgba[0], v.rgba[1], v.rgba[2], v.rgba[3]).to_linear();
                [c.red, c.green, c.blue, c.alpha]
            })
            .collect();
        if let Some(mut m) = meshes.get_mut(mesh_h) {
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; n]);
            m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
            m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
            m.insert_indices(Indices::U32((0..n as u32).collect()));
        }
        if let Some(mut m) = materials.get_mut(mat_h) {
            m.base_color_texture = fx.textures.get(&b.texture.to_ascii_lowercase()).cloned();
            m.alpha_mode = match (b.alpha_on, b.src, b.dst) {
                (false, ..) => AlphaMode::Opaque,
                (true, _, BlendFactor::One) => AlphaMode::Add,
                _ => AlphaMode::Blend,
            };
            // After the static shadows (bias 0 / 0.5); positive only (see shadows.rs).
            m.depth_bias = 1.0 + i as f32 * 0.01;
        }
    }
    for (e, ..) in fx.batches.iter().skip(batches.len()) {
        if let Ok((mut v, _)) = vis.get_mut(*e) {
            *v = Visibility::Hidden;
        }
    }
}

/// `CPointLights` from explosions and fires (refreshed every physics step).
fn update_lights(mut commands: Commands, fx: Option<ResMut<Fx>>, sa: Res<SaPhys>, mut q: Query<(&mut PointLight, &mut Transform, &mut Visibility)>) {
    let Some(mut fx) = fx else { return };
    while fx.lights.len() < LIGHT_POOL {
        let e = commands.spawn((PointLight { shadow_maps_enabled: false, ..default() }, Transform::default(), Visibility::Hidden)).id();
        fx.lights.push(e);
    }
    let lights = &sa.world.effects.lights;
    for (i, &e) in fx.lights.iter().enumerate() {
        let Ok((mut pl, mut tf, mut v)) = q.get_mut(e) else { continue };
        match lights.get(i) {
            Some(l) => {
                *v = Visibility::Visible;
                tf.translation = crate::world::g2b(l.pos.to_array());
                pl.range = l.radius;
                pl.color = Color::linear_rgb(l.color.x, l.color.y, l.color.z);
                // SA lights are 0..1 colour strengths over `radius`; scale to lumens.
                pl.intensity = l.color.max_element() * l.radius * l.radius * 4000.0;
            }
            None => *v = Visibility::Hidden,
        }
    }
}
