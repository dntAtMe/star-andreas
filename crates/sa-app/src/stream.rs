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
use sa_formats::{col, dff, objdat::ObjectPhysics, txd};
use sa_physics::objects::ObjectLogic;
use sa_physics::{
    collision::ColModel as SaColModel,
    physical::{EntityType, Physical},
};

use crate::{
    world_material::{ATTRIBUTE_NIGHT_COLOR, WorldGlobals, WorldMatUniform, WorldMaterial},
    saphys::{SaBody, SaPhys, gta_matrix},
    world::{WorldRes, g2b},
};

/// Extra distance beyond visibility at which instances are loaded / kept.
const LOAD_MARGIN: f32 = 150.0;
const UNLOAD_MARGIN: f32 = 260.0;
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
            .add_systems(Update, (receive_loaded, finalize_models, stream_instances).chain())
            .add_systems(Update, object_damage_visuals.after(crate::saphys::SaStep))
            .add_systems(Update, pickup_objects.after(crate::saphys::SaStep).after(finalize_models));
    }
}

#[derive(Component)]
pub struct StreamCamera;

// ---------------------------------------------------------------- CPU data

struct PartCpu {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    /// Day and night prelit colours, gamma 0..1.
    colors: Vec<[f32; 4]>,
    night: Vec<[f32; 4]>,
    indices: Vec<u32>,
    texture: Option<String>,
    color: [u8; 4],
    /// rpGEOMETRYLIGHT: the timecyc ambient is added.
    lit: bool,
    /// From the "_dam" atomic (CDamageAtomicModelInfo's damaged version).
    damaged: bool,
}

pub struct TexCpu {
    pub name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    pub(crate) mip_count: u32,
    pub(crate) data: Vec<u8>,
    pub alpha: bool,
}

enum Loaded {
    Model(u32, Result<(Vec<PartCpu>, ColSet)>),
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
    material: Handle<WorldMaterial>,
    damaged: bool,
}

/// A model part of a map object: intact (false) or the damaged version (true).
#[derive(Component)]
pub struct ObjectPart(pub bool);

struct Model {
    parts: Vec<Part>,
    cols: ColSet,
}

/// Collision for one model.
#[derive(Default)]
struct ColSet {
    /// The model's collision for the SA physics world (GTA space).
    sa: Option<Arc<SaColModel>>,
    /// object.dat physics for knockable props.
    prop: Option<ObjectPhysics>,
    /// 2dfx lights (positions in GTA model space) and the model name.
    lights: Arc<Vec<dff::Light2d>>,
    name: Arc<str>,
    /// BreakablePlugin data of the intact atomic.
    breakable: Option<Arc<dff::Breakable>>,
}

pub(crate) enum ModelState {
    Loading,
    /// Parsed; waiting for its TXD chain before materials can be built.
    Parsed(Vec<PartCpu>, ColSet),
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
pub(crate) struct Cache {
    pub(crate) models: HashMap<u32, ModelState>,
    txds: HashMap<String, TxdState>,
    materials: HashMap<(String, String, [u8; 4], bool), Handle<WorldMaterial>>,
}

#[derive(Resource, Default)]
pub struct Streamer {
    spawned: HashMap<usize, Entity>,
    /// Instances waiting for their model to become Ready.
    pending: HashSet<usize>,
    timer: f32,
    /// The area and game hour of the last scan (a change rescans at once).
    last_area_hour: (u8, u8),
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

pub(crate) fn request_model(world: &WorldRes, loader: &Loader, id: u32) {
    let (world, tx) = (world.0.clone(), loader.tx.clone());
    AsyncComputeTaskPool::get()
        .spawn(async move {
            let res = (|| {
                let obj = world.objects.get(&id).ok_or_else(|| anyhow::anyhow!("no def"))?;
                let data = world
                    .file(&format!("{}.dff", obj.model))
                    .ok_or_else(|| anyhow::anyhow!("{}.dff missing", obj.model))?;
                let clump = dff::parse(data)?;
                let parts = build_parts(&clump)?;
                let mut cols = ColSet { lights: Arc::new(model_lights(&clump)), name: obj.model.as_str().into(), ..Default::default() };
                cols.breakable = clump.geometries.iter().find_map(|g| g.breakable.clone()).map(Arc::new);
                if let Some(c) = world.col(&obj.model) {
                    let m = col::parse_model(c)?;
                    cols.sa = Some(Arc::new(SaColModel::from_col(&m)));
                    // Every object.dat model is a CObject (doors' hinge physics is not ported).
                    cols.prop = world.physics.get(&obj.model).filter(|p| !matches!(p.special, 6 | 7)).copied();
                }
                Ok((parts, cols))
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

/// The clump's 2dfx lights, moved into GTA model space by their atomic's frame.
fn model_lights(clump: &dff::Clump) -> Vec<dff::Light2d> {
    let mut out = Vec::new();
    for atomic in &clump.atomics {
        let frame = atomic.frame as usize;
        let name = clump.frames.get(frame).map(|f| f.name.to_ascii_lowercase()).unwrap_or_default();
        if name.ends_with("_dam") || name.ends_with("_vlo") {
            continue;
        }
        let Some(geo) = clump.geometries.get(atomic.geometry as usize) else { continue };
        let (rot, pos) = clump.frame_world(frame);
        for l in &geo.lights {
            let r = dff::apply(&rot, l.pos);
            let mut l = l.clone();
            l.pos = [r[0] + pos[0], r[1] + pos[1], r[2] + pos[2]];
            out.push(l);
        }
    }
    out
}

/// Split a clump into one mesh per material, baked into model space (Y-up).
fn build_parts(clump: &dff::Clump) -> Result<Vec<PartCpu>> {
    let mut parts = Vec::new();
    for atomic in &clump.atomics {
        let frame = atomic.frame as usize;
        let name = clump.frames.get(frame).map(|f| f.name.to_ascii_lowercase()).unwrap_or_default();
        // Very-low variants are not used; "_dam" atomics are the damaged version.
        // The weapon models' muzzle flash is drawn only while firing (pickups never fire).
        if name.ends_with("_vlo") || name == "gunflash" {
            continue;
        }
        let damaged = name.ends_with("_dam");
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
                night: Vec::new(),
                indices: Vec::with_capacity(tris.len() * 3),
                texture: mat.texture.as_ref().map(|t| t.name.to_ascii_lowercase()),
                color: mat.color,
                lit,
                damaged,
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
                        let n = geo.extra_colors.get(v).copied().unwrap_or(c);
                        part.colors.push(gamma01(c));
                        part.night.push(gamma01(n));
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

/// Prelit colour as gamma 0..1 (the world material blends and lights in gamma space).
fn gamma01(c: [u8; 4]) -> [f32; 4] {
    c.map(|x| x as f32 / 255.0)
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
            Loaded::Model(id, Ok((parts, cols))) => {
                cache.models.insert(id, ModelState::Parsed(parts, cols));
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
    mut materials: ResMut<Assets<WorldMaterial>>,
    globals: Res<WorldGlobals>,
    mut break_tex: ResMut<crate::breaks::BreakTextures>,
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

        let Some(ModelState::Parsed(cpu, cols)) = cache.models.remove(&id) else { continue };
        if let Some(b) = &cols.breakable {
            let texs = b
                .tex_names
                .iter()
                .map(|name| {
                    chain.iter().find_map(|t| match cache.txds.get(t) {
                        Some(TxdState::Ready(m)) => m.get(&name.to_ascii_lowercase()).map(|e| e.image.clone()),
                        _ => None,
                    })
                })
                .collect();
            break_tex.0.insert(Arc::as_ptr(b) as usize, texs);
        }
        let mut parts = Vec::with_capacity(cpu.len());
        for p in cpu {
            let tex = p.texture.as_ref().and_then(|name| {
                chain.iter().find_map(|t| match cache.txds.get(t) {
                    Some(TxdState::Ready(m)) => m.get(name).cloned(),
                    _ => None,
                })
            });
            let key = (obj.txd.clone(), p.texture.clone().unwrap_or_default(), p.color, p.lit);
            let material = cache
                .materials
                .entry(key)
                .or_insert_with(|| {
                    let alpha = tex.as_ref().is_some_and(|t| t.alpha);
                    let c = p.color;
                    let alpha_mode = if c[3] < 255 {
                        AlphaMode::Blend
                    } else if alpha {
                        AlphaMode::Mask(0.5)
                    } else {
                        AlphaMode::Opaque
                    };
                    let cutoff = if let AlphaMode::Mask(x) = alpha_mode { x } else { -1.0 };
                    materials.add(WorldMaterial {
                        uniform: WorldMatUniform {
                            color: Vec4::from(gamma01(c)),
                            params: Vec4::new(if p.lit { 1.0 } else { 0.0 }, cutoff, 0.0, 0.0),
                        },
                        texture: tex.map(|t| t.image),
                        globals: globals.0.clone(),
                        alpha_mode,
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
            mesh.insert_attribute(ATTRIBUTE_NIGHT_COLOR, p.night);
            mesh.insert_indices(Indices::U32(p.indices));
            if !has_normals {
                mesh.duplicate_vertices();
                mesh.compute_flat_normals();
            }
            parts.push(Part { mesh: meshes.add(mesh), material, damaged: p.damaged });
        }
        cache.models.insert(id, ModelState::Ready(Arc::new(Model { parts, cols })));
    }
}

fn stream_instances(
    mut commands: Commands,
    time: Res<Time>,
    world: Res<WorldRes>,
    loader: Res<Loader>,
    mut cache: ResMut<Cache>,
    mut st: ResMut<Streamer>,
    mut sa: ResMut<SaPhys>,
    cam: Single<&GlobalTransform, With<StreamCamera>>,
) {
    let cam_pos = cam.translation();
    let st = &mut *st;

    let area = sa.world.curr_area;
    let hour = sa.world.clock.hours;
    st.timer -= time.delta_secs();
    if (area, hour) != st.last_area_hour {
        st.last_area_hour = (area, hour);
        st.timer = 0.0;
    }
    if st.timer <= 0.0 {
        st.timer = SCAN_INTERVAL;
        let clock = &sa.world.clock;
        for (i, inst) in world.0.instances.iter().enumerate() {
            let d = inst.pos.distance(cam_pos);
            let spawned = st.spawned.contains_key(&i);
            let shown = inst.in_area(area) && inst.time.is_none_or(|(on, off)| clock.is_time_in_range(on, off));
            let want = shown && d < inst.far + LOAD_MARGIN && d + LOAD_MARGIN >= inst.near;
            let drop = !shown || d > inst.far + UNLOAD_MARGIN || d + UNLOAD_MARGIN < inst.near;
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
                let tf = Transform::from_translation(inst.pos).with_rotation(inst.rot);
                let mut ec = commands.spawn((tf, Visibility::default()));
                // Only full-detail instances collide; LODs are visual only.
                let collide = inst.near == 0.0;
                // SA physics: props are static bodies that can be knocked loose, the rest is geometry.
                if let (true, Some(sa_col)) = (collide, &model.cols.sa) {
                    let m = gta_matrix(&tf);
                    match model.cols.prop {
                        Some(op) => {
                            let mut p = Physical::new(EntityType::Object, m);
                            let mut logic = ObjectLogic::new(op, &model.cols.name.to_ascii_lowercase());
                            logic.breakable = model.cols.breakable.clone();
                            logic.setup_physical(&mut p);
                            let id = sa.world.add_body(p, (**sa_col).clone(), Box::new(logic));
                            ec.insert(SaBody::new(id, m));
                        }
                        // Buildings collide through their COL slot (colstore.rs), streamed
                        // around the player.
                        None => {}
                    }
                }
                if !model.cols.lights.is_empty() {
                    ec.insert(crate::lights::EntityLights {
                        lights: model.cols.lights.clone(),
                        model: model.cols.name.clone(),
                        // m_nRandomSeed: any stable per-instance u16.
                        seed: (i as u32).wrapping_mul(2_654_435_761).rotate_right(16) as u16,
                        key: i as u64,
                    });
                }
                let e = ec
                    .with_children(|c| {
                        for p in model.parts.iter() {
                            let vis = if p.damaged { Visibility::Hidden } else { Visibility::Inherited };
                            c.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(p.material.clone()), range.clone(), vis, ObjectPart(p.damaged)));
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


/// `CObject` visuals: hidden when smashed, the "_dam" parts once damaged.
pub fn object_damage_visuals(
    sa: Res<SaPhys>,
    objects: Query<(&SaBody, &Children)>,
    mut vis: Query<&mut Visibility>,
    parts: Query<&ObjectPart>,
) {
    for (body, children) in &objects {
        let Some(o) = sa.world.body(body.id).and_then(|b| b.logic.as_any().downcast_ref::<ObjectLogic>()) else { continue };
        if !(o.hidden || o.render_damaged) {
            continue;
        }
        for c in children.iter() {
            let Ok(part) = parts.get(c) else { continue };
            let show = !o.hidden && part.0 == o.render_damaged;
            if let Ok(mut v) = vis.get_mut(c) {
                let want = if show { Visibility::Inherited } else { Visibility::Hidden };
                if *v != want {
                    *v = want;
                }
            }
        }
    }
}

/// The pickup CObjects (pickups.md §4): one per visible, enabled pickup (CPickups::Update's
/// 100 m camera gate), spinning once per 2.048 s and scaled up when small (DoPickUpEffects);
/// hidden in widescreen.
fn pickup_objects(
    mut commands: Commands,
    world: Res<WorldRes>,
    loader: Res<Loader>,
    mut cache: ResMut<Cache>,
    mut sa: ResMut<SaPhys>,
    overlay: Res<crate::hud::Overlay>,
    mut objs: Local<HashMap<usize, (u16, Entity)>>,
    mut test_done: Local<bool>,
    mut tfs: Query<&mut Transform>,
) {
    // SA_TESTPICKUP=<model>[,type]: one pickup 3 m north of the player (debug).
    if !*test_done {
        if let Some(v) = std::env::var("SA_TESTPICKUP").ok() {
            let mut it = v.split(',').filter_map(|t| t.trim().parse::<u32>().ok());
            let (m, t) = (it.next().unwrap_or(1240), it.next().unwrap_or(2));
            if let Some(p) = sa.world.player_id().and_then(|p| sa.world.body(p)).map(|b| b.phys.matrix.pos) {
                if sa.world.now_ms > 5000 {
                    sa.world.generate_pickup(p + Vec3::new(2.0, 4.0, 0.0), m as u16, t as u8, 0, 0, false, 0);
                    *test_done = true;
                }
            }
        }
    }
    let w = &sa.world;
    let now = w.now_ms;
    let a = (now & 0x7FF) as f32 * 0.003_056_640_7;
    for (i, p) in w.pickups.slots.iter().enumerate() {
        let want = p.ty != sa_physics::pickups::ty::NONE && p.visible && !p.disabled && !overlay.widescreen;
        match objs.get(&i).copied() {
            Some((r, e)) if !want || r != p.ref_index => {
                commands.entity(e).despawn();
                objs.remove(&i);
            }
            _ => {}
        }
        if !want {
            continue;
        }
        let id = p.model as u32;
        let model = match cache.models.get(&id) {
            Some(ModelState::Ready(m)) => m.clone(),
            None => {
                cache.models.insert(id, ModelState::Loading);
                request_model(&world, &loader, id);
                continue;
            }
            _ => continue,
        };
        // The scale from the collision box: s = max(1.2 / max dimension, 1), (s - 1) * 0.6 + 1.
        let s = if p.model == 362 {
            1.2
        } else {
            let d = model.cols.sa.as_ref().map_or(1.2, |c| (c.bbox_max - c.bbox_min).max_element());
            (1.2 / d.max(1e-3)).max(1.0) * 0.6 + 0.4
        };
        let tf = Transform::from_translation(crate::world::g2b(p.position().to_array())).with_rotation(Quat::from_rotation_y(a)).with_scale(Vec3::splat(s));
        match objs.get(&i) {
            Some(&(_, e)) => {
                if let Ok(mut t) = tfs.get_mut(e) {
                    *t = tf;
                }
            }
            None => {
                let e = commands
                    .spawn((tf, Visibility::default()))
                    .with_children(|c| {
                        for part in model.parts.iter().filter(|p| !p.damaged) {
                            c.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone())));
                        }
                    })
                    .id();
                objs.insert(i, (p.ref_index, e));
            }
        }
    }
}
