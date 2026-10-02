//! Distance-based streaming of map instances.
//!
//! Worker threads parse DFF/TXD into CPU-side data; the main thread turns
//! finished results into Bevy assets and spawns/despawns instance entities
//! as the camera moves.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
};

use anyhow::Result;
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::VisibilityRange,
    image::{
        CompressedImageFormatSupport, CompressedImageFormats, ImageAddressMode, ImageFilterMode,
        ImageSampler, ImageSamplerDescriptor,
    },
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    tasks::AsyncComputeTaskPool,
};
use bevy_rapier3d::prelude::{Collider, RigidBody};
use sa_formats::{col, dff, txd};

use crate::world::{WorldRes, g2b};

/// Extra distance beyond visibility at which instances are loaded / kept.
const LOAD_MARGIN: f32 = 60.0;
const UNLOAD_MARGIN: f32 = 200.0;
const SCAN_INTERVAL: f32 = 0.2;
const MAX_RESULTS_PER_FRAME: usize = 96;
const MAX_SPAWNS_PER_FRAME: usize = 3000;

pub struct StreamPlugin;

impl Plugin for StreamPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = channel();
        app.insert_resource(Loader { tx, rx: Mutex::new(rx) })
            .init_resource::<Cache>()
            .init_resource::<Streamer>()
            .add_systems(Update, (receive_loaded, finalize_models, stream_instances).chain());
    }
}

#[derive(Component)]
pub struct StreamCamera;

// ---------------------------------------------------------------- CPU data

struct PartCpu {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    texture: Option<String>,
    color: [u8; 4],
}

pub struct TexCpu {
    pub name: String,
    width: u32,
    height: u32,
    format: TextureFormat,
    mip_count: u32,
    data: Vec<u8>,
    pub alpha: bool,
}

enum Loaded {
    Model(u32, Result<(Vec<PartCpu>, Vec<Collider>)>),
    Txd(String, Result<Vec<TexCpu>>),
}

#[derive(Resource)]
struct Loader {
    tx: Sender<Loaded>,
    rx: Mutex<Receiver<Loaded>>,
}

// ---------------------------------------------------------------- caches

#[derive(Clone)]
struct Part {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

struct Model {
    parts: Vec<Part>,
    /// Primitive compound and/or triangle mesh (parry can't nest a trimesh in a compound).
    colliders: Vec<Collider>,
}

enum ModelState {
    Loading,
    /// Parsed; waiting for its TXD chain before materials can be built.
    Parsed(Vec<PartCpu>, Vec<Collider>),
    Ready(Arc<Model>),
    Failed,
}

#[derive(Clone)]
struct TexEntry {
    image: Handle<Image>,
    alpha: bool,
}

enum TxdState {
    Loading,
    Ready(HashMap<String, TexEntry>),
    Failed,
}

#[derive(Resource, Default)]
struct Cache {
    models: HashMap<u32, ModelState>,
    txds: HashMap<String, TxdState>,
    materials: HashMap<(String, String, [u8; 4]), Handle<StandardMaterial>>,
}

#[derive(Resource, Default)]
pub struct Streamer {
    spawned: HashMap<usize, Entity>,
    /// Instances waiting for their model to become Ready.
    pending: HashSet<usize>,
    timer: f32,
    pub stats: Stats,
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub spawned: usize,
    pub pending: usize,
    pub models_ready: usize,
    pub models_loading: usize,
    pub txds: usize,
}

// ---------------------------------------------------------------- workers

fn request_model(world: &WorldRes, loader: &Loader, id: u32) {
    let (world, tx) = (world.0.clone(), loader.tx.clone());
    AsyncComputeTaskPool::get()
        .spawn(async move {
            let res = (|| {
                let obj = world.objects.get(&id).ok_or_else(|| anyhow::anyhow!("no def"))?;
                let data = world
                    .file(&format!("{}.dff", obj.model))
                    .ok_or_else(|| anyhow::anyhow!("{}.dff missing", obj.model))?;
                let parts = build_parts(&dff::parse(data)?)?;
                let colliders = match world.col(&obj.model) {
                    Some(c) => build_colliders(&col::parse_model(c)?),
                    None => Vec::new(),
                };
                Ok((parts, colliders))
            })();
            let _ = tx.send(Loaded::Model(id, res));
        })
        .detach();
}

fn request_txd(world: &WorldRes, loader: &Loader, name: String, bc: bool) {
    let (world, tx) = (world.0.clone(), loader.tx.clone());
    AsyncComputeTaskPool::get()
        .spawn(async move {
            let res = (|| {
                let data = world
                    .file(&format!("{name}.txd"))
                    .ok_or_else(|| anyhow::anyhow!("{name}.txd missing"))?;
                Ok(txd::parse(data)?.into_iter().filter_map(|t| convert_texture(t, bc)).collect())
            })();
            let _ = tx.send(Loaded::Txd(name, res));
        })
        .detach();
}

/// Split a clump into one mesh per material, baked into model space (Y-up).
fn build_parts(clump: &dff::Clump) -> Result<Vec<PartCpu>> {
    let mut parts = Vec::new();
    for atomic in &clump.atomics {
        let frame = atomic.frame as usize;
        let name = clump.frames.get(frame).map(|f| f.name.to_ascii_lowercase()).unwrap_or_default();
        // Damage / very-low variants are not part of the intact model.
        if name.ends_with("_dam") || name.ends_with("_vlo") {
            continue;
        }
        let Some(geo) = clump.geometries.get(atomic.geometry as usize) else { continue };
        if geo.positions.is_empty() {
            continue;
        }
        let (rot, pos) = clump.frame_world(frame);
        let lit = geo.flags & dff::geo_flags::LIGHT != 0;
        let xf = |p: [f32; 3]| {
            let r = dff::apply(&rot, p);
            g2b([r[0] + pos[0], r[1] + pos[1], r[2] + pos[2]])
        };

        for (mi, mat) in geo.materials.iter().enumerate() {
            let tris: Vec<_> = geo.triangles.iter().filter(|t| t.material as usize == mi).collect();
            if tris.is_empty() {
                continue;
            }
            // Compact vertices used by this material.
            let mut remap = vec![u32::MAX; geo.positions.len()];
            let mut part = PartCpu {
                positions: Vec::new(),
                normals: Vec::new(),
                uvs: Vec::new(),
                colors: Vec::new(),
                indices: Vec::with_capacity(tris.len() * 3),
                texture: mat.texture.as_ref().map(|t| t.name.to_ascii_lowercase()),
                color: mat.color,
            };
            for t in tris {
                for &v in &t.v {
                    let v = v as usize;
                    if v >= remap.len() {
                        continue;
                    }
                    if remap[v] == u32::MAX {
                        remap[v] = part.positions.len() as u32;
                        part.positions.push(xf(geo.positions[v]).to_array());
                        if let Some(n) = geo.normals.get(v) {
                            part.normals.push(g2b(dff::apply(&rot, *n)).normalize_or_zero().to_array());
                        }
                        part.uvs.push(geo.uvs.first().and_then(|u| u.get(v)).copied().unwrap_or([0.0; 2]));
                        let c = geo.prelit.get(v).copied().unwrap_or([255; 4]);
                        part.colors.push(prelit_to_linear(c, lit));
                    }
                    part.indices.push(remap[v]);
                }
            }
            if part.indices.len() % 3 == 0 && !part.indices.is_empty() {
                parts.push(part);
            }
        }
    }
    Ok(parts)
}

/// Collision shapes in Bevy space: spheres, boxes and the triangle mesh.
fn build_colliders(m: &col::ColModel) -> Vec<Collider> {
    let mut out = Vec::new();
    let mut shapes: Vec<(Vec3, Quat, Collider)> = Vec::new();
    for s in &m.spheres {
        shapes.push((g2b(s.center), Quat::IDENTITY, Collider::ball(s.radius)));
    }
    for b in &m.boxes {
        let (lo, hi) = (g2b(b.min), g2b(b.max));
        let half = ((hi - lo).abs() * 0.5).max(Vec3::splat(0.01));
        shapes.push(((lo + hi) * 0.5, Quat::IDENTITY, Collider::cuboid(half.x, half.y, half.z)));
    }
    if !m.faces.is_empty() {
        let verts: Vec<Vec3> = m.vertices.iter().map(|&v| g2b(v)).collect();
        let tris: Vec<[u32; 3]> = m
            .faces
            .iter()
            .map(|f| f.v)
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .collect();
        if let Ok(c) = Collider::trimesh(verts, tris) {
            out.push(c);
        }
    }
    if !shapes.is_empty() {
        out.push(Collider::compound(shapes));
    }
    out
}

/// Noon ambient added to lit geometry (stand-in for timecyc `AmbientObj`).
const NOON_AMBIENT: f32 = 0.3;

fn prelit_to_linear(c: [u8; 4], lit: bool) -> [f32; 4] {
    let amb = if lit { NOON_AMBIENT } else { 0.0 };
    let f = |x: u8| (x as f32 / 255.0 + amb).min(1.0).powf(2.2);
    [f(c[0]), f(c[1]), f(c[2]), c[3] as f32 / 255.0]
}

pub fn convert_texture(t: txd::Texture, bc_supported: bool) -> Option<TexCpu> {
    use txd::Format;
    if t.mips.is_empty() || t.width == 0 || t.height == 0 {
        return None;
    }
    let name = t.name.to_ascii_lowercase();
    let block_aligned = t.width % 4 == 0 && t.height % 4 == 0;
    let (format, mips): (TextureFormat, Vec<Vec<u8>>) = match t.format {
        Format::Dxt1 | Format::Dxt3 | Format::Dxt5 if bc_supported && block_aligned => {
            let f = match t.format {
                Format::Dxt1 => TextureFormat::Bc1RgbaUnormSrgb,
                Format::Dxt3 => TextureFormat::Bc2RgbaUnormSrgb,
                _ => TextureFormat::Bc3RgbaUnormSrgb,
            };
            let block = if t.format == Format::Dxt1 { 8 } else { 16 };
            // Keep only mips whose payload matches the expected block count.
            let mut mips = Vec::new();
            for (l, m) in t.mips.into_iter().enumerate() {
                let (w, h) = ((t.width >> l).max(1), (t.height >> l).max(1));
                let need = (w.div_ceil(4) * h.div_ceil(4)) as usize * block;
                if m.len() < need {
                    break;
                }
                mips.push(m[..need].to_vec());
            }
            (f, mips)
        }
        Format::Dxt1 | Format::Dxt3 | Format::Dxt5 => {
            let mut mips = Vec::new();
            for (l, m) in t.mips.iter().enumerate() {
                let (w, h) = ((t.width >> l).max(1), (t.height >> l).max(1));
                mips.push(txd::decode_dxt(t.format, m, w, h));
                // Non-block-aligned sizes: keep the base level only.
                if !block_aligned {
                    break;
                }
            }
            (TextureFormat::Rgba8UnormSrgb, mips)
        }
        Format::Rgba8 => {
            let mut mips = Vec::new();
            for (l, m) in t.mips.into_iter().enumerate() {
                let (w, h) = ((t.width >> l).max(1), (t.height >> l).max(1));
                if m.len() != (w * h * 4) as usize {
                    break;
                }
                mips.push(m);
            }
            (TextureFormat::Rgba8UnormSrgb, mips)
        }
    };
    if mips.is_empty() {
        return None;
    }
    Some(TexCpu {
        name,
        width: t.width,
        height: t.height,
        format,
        mip_count: mips.len() as u32,
        data: mips.concat(),
        alpha: t.has_alpha,
    })
}

// ---------------------------------------------------------------- systems

pub fn make_image(t: TexCpu) -> Image {
    let mut img = Image::new_uninit(
        Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        t.format,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.data = Some(t.data);
    img.texture_descriptor.mip_level_count = t.mip_count;
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    img
}

fn receive_loaded(
    loader: Res<Loader>,
    mut cache: ResMut<Cache>,
    mut images: ResMut<Assets<Image>>,
) {
    let rx = loader.rx.lock().unwrap();
    for msg in rx.try_iter().take(MAX_RESULTS_PER_FRAME) {
        match msg {
            Loaded::Model(id, Ok((parts, colliders))) => {
                cache.models.insert(id, ModelState::Parsed(parts, colliders));
            }
            Loaded::Model(id, Err(e)) => {
                warn!("model {id}: {e:#}");
                cache.models.insert(id, ModelState::Failed);
            }
            Loaded::Txd(name, Ok(texs)) => {
                let map = texs
                    .into_iter()
                    .map(|t| {
                        let (name, alpha, img) = (t.name.clone(), t.alpha, make_image(t));
                        (name, TexEntry { image: images.add(img), alpha })
                    })
                    .collect();
                cache.txds.insert(name, TxdState::Ready(map));
            }
            Loaded::Txd(name, Err(e)) => {
                warn!("txd {name}: {e:#}");
                cache.txds.insert(name, TxdState::Failed);
            }
        }
    }
}

/// Turn parsed models into meshes + materials once their TXD chain is loaded.
fn finalize_models(
    world: Res<WorldRes>,
    loader: Res<Loader>,
    formats: Option<Res<CompressedImageFormatSupport>>,
    mut cache: ResMut<Cache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let bc = formats.is_some_and(|f| f.0.contains(CompressedImageFormats::BC));
    let parsed: Vec<u32> = cache
        .models
        .iter()
        .filter(|(_, s)| matches!(s, ModelState::Parsed(..)))
        .map(|(&id, _)| id)
        .collect();

    for id in parsed {
        let Some(obj) = world.0.objects.get(&id) else { continue };
        // TXD chain: own txd, then parents.
        let mut chain = vec![obj.txd.clone()];
        while let Some(p) = world.0.txd_parent.get(chain.last().unwrap()) {
            if chain.contains(p) || chain.len() > 8 {
                break;
            }
            chain.push(p.clone());
        }
        let mut waiting = false;
        for t in &chain {
            match cache.txds.get(t) {
                None => {
                    cache.txds.insert(t.clone(), TxdState::Loading);
                    request_txd(&world, &loader, t.clone(), bc);
                    waiting = true;
                }
                Some(TxdState::Loading) => waiting = true,
                _ => {}
            }
        }
        if waiting {
            continue;
        }

        let Some(ModelState::Parsed(cpu, colliders)) = cache.models.remove(&id) else { continue };
        let mut parts = Vec::with_capacity(cpu.len());
        for p in cpu {
            let tex = p.texture.as_ref().and_then(|name| {
                chain.iter().find_map(|t| match cache.txds.get(t) {
                    Some(TxdState::Ready(m)) => m.get(name).cloned(),
                    _ => None,
                })
            });
            let key = (obj.txd.clone(), p.texture.clone().unwrap_or_default(), p.color);
            let material = cache
                .materials
                .entry(key)
                .or_insert_with(|| {
                    let alpha = tex.as_ref().is_some_and(|t| t.alpha);
                    let c = p.color;
                    materials.add(StandardMaterial {
                        base_color: Color::srgba_u8(c[0], c[1], c[2], c[3]),
                        base_color_texture: tex.map(|t| t.image),
                        unlit: true,
                        double_sided: true,
                        cull_mode: None,
                        alpha_mode: if c[3] < 255 {
                            AlphaMode::Blend
                        } else if alpha {
                            AlphaMode::Mask(0.5)
                        } else {
                            AlphaMode::Opaque
                        },
                        ..default()
                    })
                })
                .clone();

            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            let has_normals = p.normals.len() == p.positions.len();
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, p.positions);
            if has_normals {
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, p.normals);
            }
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, p.uvs);
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, p.colors);
            mesh.insert_indices(Indices::U32(p.indices));
            if !has_normals {
                mesh.duplicate_vertices();
                mesh.compute_flat_normals();
            }
            parts.push(Part { mesh: meshes.add(mesh), material });
        }
        cache.models.insert(id, ModelState::Ready(Arc::new(Model { parts, colliders })));
    }
}

fn stream_instances(
    mut commands: Commands,
    time: Res<Time>,
    world: Res<WorldRes>,
    loader: Res<Loader>,
    mut cache: ResMut<Cache>,
    mut st: ResMut<Streamer>,
    cam: Single<&GlobalTransform, With<StreamCamera>>,
) {
    let cam_pos = cam.translation();
    let st = &mut *st;

    st.timer -= time.delta_secs();
    if st.timer <= 0.0 {
        st.timer = SCAN_INTERVAL;
        for (i, inst) in world.0.instances.iter().enumerate() {
            let d = inst.pos.distance(cam_pos);
            let spawned = st.spawned.contains_key(&i);
            let want = d < inst.far + LOAD_MARGIN && d + LOAD_MARGIN >= inst.near;
            let drop = d > inst.far + UNLOAD_MARGIN || d + UNLOAD_MARGIN < inst.near;
            if want && !spawned {
                st.pending.insert(i);
                if !cache.models.contains_key(&inst.id) {
                    cache.models.insert(inst.id, ModelState::Loading);
                    request_model(&world, &loader, inst.id);
                }
            } else if drop {
                st.pending.remove(&i);
                if let Some(e) = st.spawned.remove(&i) {
                    commands.entity(e).despawn();
                }
            }
        }
    }

    // Spawn pending instances whose models are ready.
    let mut done = Vec::new();
    for &i in st.pending.iter() {
        if done.len() >= MAX_SPAWNS_PER_FRAME {
            break;
        }
        let inst = &world.0.instances[i];
        match cache.models.get(&inst.id) {
            Some(ModelState::Ready(model)) => {
                let range = VisibilityRange {
                    start_margin: inst.near..inst.near,
                    end_margin: inst.far..inst.far,
                    use_aabb: false,
                };
                let model = model.clone();
                let mut ec = commands
                    .spawn((Transform::from_translation(inst.pos).with_rotation(inst.rot), Visibility::default()));
                // Only full-detail instances collide; LODs are visual only.
                let collide = inst.near == 0.0 && !model.colliders.is_empty();
                if collide {
                    ec.insert(RigidBody::Fixed);
                }
                let e = ec
                    .with_children(|c| {
                        if collide {
                            for col in &model.colliders {
                                c.spawn((Transform::default(), col.clone()));
                            }
                        }
                        for p in model.parts.iter() {
                            c.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(p.material.clone()), range.clone()));
                        }
                    })
                    .id();
                st.spawned.insert(i, e);
                done.push(i);
            }
            Some(ModelState::Failed) => done.push(i),
            _ => {}
        }
    }
    for i in done {
        st.pending.remove(&i);
    }

    let (mut ready, mut loading) = (0, 0);
    for s in cache.models.values() {
        match s {
            ModelState::Ready(_) => ready += 1,
            ModelState::Loading | ModelState::Parsed(..) => loading += 1,
            ModelState::Failed => {}
        }
    }
    st.stats = Stats {
        spawned: st.spawned.len(),
        pending: st.pending.len(),
        models_ready: ready,
        models_loading: loading,
        txds: cache.txds.len(),
    };
}
