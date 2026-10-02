//! Real SC2 unit models: a loader thread reads `.m3` + `.dds` straight from the
//! CASC storage and hands back renderer-ready buffers; the main thread turns
//! them into meshes/materials and swaps out the placeholder capsules.
//!
//! Unit type -> model is a name heuristic for now (`Marine` ->
//! `.../assets/units/<race>/marine/marine.m3`); the proper chain is
//! UnitData -> ActorData -> ModelData in the game data catalogs.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Mutex,
        mpsc::{Receiver, Sender, channel},
    },
};

use anyhow::{Context, Result};
use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use sc2_formats::{
    casc::{Storage, parse_key},
    dds::{self, Dds},
    m3,
    root::Root,
};

pub(crate) struct ModelsPlugin;

impl Plugin for ModelsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, start_loader).add_systems(Update, receive_models);
    }
}

/// Model state per SC2 unit type.
pub enum ModelState {
    Pending,
    Ready(Vec<(Handle<Mesh>, Handle<StandardMaterial>)>, f32),
    Failed,
}

#[derive(Resource)]
pub struct Models {
    tx: Sender<(u32, String)>,
    rx: Mutex<Receiver<(u32, Result<ModelCpu>)>>,
    pub types: HashMap<u32, ModelState>,
    textures: HashMap<String, Handle<Image>>,
}

impl Models {
    /// Current state for a unit type, queueing a load on first use.
    pub fn get(&mut self, unit_type: u32, name: &str) -> &ModelState {
        self.types.entry(unit_type).or_insert_with(|| {
            let _ = self.tx.send((unit_type, name.to_string()));
            ModelState::Pending
        })
    }
}

/// One drawable region, already in Bevy space (Y-up).
struct MeshCpu {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    texture: Option<String>,
    blend: u32,
    two_sided: bool,
}

pub struct ModelCpu {
    meshes: Vec<MeshCpu>,
    /// Newly decoded textures by path (each sent once).
    textures: Vec<(String, TexCpu)>,
    height: f32,
}

struct TexCpu {
    width: u32,
    height: u32,
    mips: u32,
    format: TextureFormat,
    data: Vec<u8>,
}

fn start_loader(mut commands: Commands, settings: Res<crate::Sc2Settings>) {
    if std::env::var("SC2_MODELS").is_ok_and(|v| v == "0") {
        return;
    }
    let (tx, req_rx) = channel::<(u32, String)>();
    let (res_tx, rx) = channel();
    let dir = settings.game_dir.clone();
    std::thread::Builder::new()
        .name("sc2-models".into())
        .spawn(move || loader(dir, req_rx, res_tx))
        .expect("spawn sc2-models thread");
    commands.insert_resource(Models { tx, rx: Mutex::new(rx), types: HashMap::new(), textures: HashMap::new() });
}

struct Library {
    storage: Storage,
    root: Root,
    /// `marine` -> every `.../assets/{units,buildings}/.../marine.m3`.
    units: HashMap<String, Vec<String>>,
    sent_textures: HashSet<String>,
}

fn loader(dir: PathBuf, rx: Receiver<(u32, String)>, tx: Sender<(u32, Result<ModelCpu>)>) {
    let t = std::time::Instant::now();
    let lib = Storage::open(&dir).and_then(|storage| {
        let root = Root::parse(&storage.read_ckey(&parse_key(storage.config_value("root", 0)?)?)?)?;
        let mut units: HashMap<String, Vec<String>> = HashMap::new();
        for (name, _) in root.names() {
            if name.ends_with(".m3") && (name.contains("/assets/units/") || name.contains("/assets/buildings/")) {
                let stem = name.rsplit('/').next().unwrap().trim_end_matches(".m3");
                units.entry(stem.to_string()).or_default().push(name.to_string());
            }
        }
        Ok(Library { storage, root, units, sent_textures: HashSet::new() })
    });
    let mut lib = match lib {
        Ok(l) => {
            info!("sc2 models: {} unit models indexed in {:.2?}", l.units.len(), t.elapsed());
            l
        }
        Err(e) => {
            error!("sc2 models: storage unavailable: {e:#}");
            for (ty, _) in rx {
                let _ = tx.send((ty, Err(anyhow::anyhow!("no storage"))));
            }
            return;
        }
    };
    for (ty, name) in rx {
        let r = load(&mut lib, &name).with_context(|| format!("{name} ({ty})"));
        if tx.send((ty, r)).is_err() {
            return;
        }
    }
}

/// Package preference when a model exists in several mods.
fn rank(path: &str) -> (u8, usize) {
    const ORDER: [&str; 5] = ["mods/liberty.sc2mod/base", "mods/swarm.sc2mod/base", "mods/void.sc2mod/base", "mods/core.sc2mod/base", "mods/"];
    (ORDER.iter().position(|p| path.starts_with(p)).unwrap_or(ORDER.len()) as u8, path.len())
}

fn find_model<'a>(lib: &'a Library, unit: &str) -> Option<&'a str> {
    const SUFFIXES: [&str; 8] = ["sieged", "burrowed", "flying", "lowered", "uprooted", "phasing", "mp", "morph"];
    let lower = unit.to_ascii_lowercase();
    let mut keys = vec![lower.clone()];
    keys.extend(SUFFIXES.iter().filter_map(|s| lower.strip_suffix(s).map(String::from)));
    keys.iter().find_map(|k| lib.units.get(k)?.iter().min_by_key(|p| rank(p)).map(String::as_str))
}

fn load(lib: &mut Library, unit: &str) -> Result<ModelCpu> {
    let path = find_model(lib, unit).context("no model found")?.to_string();
    let data = lib.storage.read_ckey(&lib.root.get(&path).context("model vanished")?)?;
    let model = m3::parse(&data).with_context(|| path.clone())?;
    let package = path.find(".sc2assets/").map(|i| path[..i + 11].to_string()).unwrap_or_default();

    let mut meshes = Vec::new();
    let mut textures = Vec::new();
    let mut top: f32 = 0.0;
    for m in &model.meshes {
        let mat = m.material.and_then(|i| model.materials.get(i));
        // Only plain standard surfaces; effect layers (additive glows etc.) need their own shading.
        if m.hidden || m.indices.is_empty() || mat.is_some_and(|mt| mt.kind != 1 || mt.blend_mode > 1) {
            continue;
        }
        // SC2 is Z-up like GTA: (x, y, z) -> Bevy (x, z, -y).
        let positions: Vec<[f32; 3]> = m.positions.iter().map(|p| [p[0], p[2], -p[1]]).collect();
        top = positions.iter().fold(top, |t, p| t.max(p[1]));
        let normals = m.normals.iter().map(|n| [n[0], n[2], -n[1]]).collect();
        let texture = mat.and_then(|mt| mt.diffuse.as_deref()).map(|p| p.replace('\\', "/").to_ascii_lowercase());
        if let Some(t) = &texture
            && !lib.sent_textures.contains(t)
        {
            lib.sent_textures.insert(t.clone());
            match read_texture(lib, &package, t) {
                Ok(tex) => textures.push((t.clone(), tex)),
                Err(e) => warn!("sc2 models: {t}: {e:#}"),
            }
        }
        meshes.push(MeshCpu {
            positions,
            normals,
            uvs: m.uvs.clone(),
            indices: m.indices.clone(),
            texture,
            blend: mat.map_or(0, |mt| mt.blend_mode),
            two_sided: mat.is_some_and(|mt| mt.flags & 0x8 != 0),
        });
    }
    anyhow::ensure!(!meshes.is_empty(), "{path}: no drawable regions");
    Ok(ModelCpu { meshes, textures, height: top })
}

fn read_texture(lib: &Library, package: &str, path: &str) -> Result<TexCpu> {
    let read = |pkg: &str, p: &str| lib.root.get(&format!("{pkg}{p}")).and_then(|k| lib.storage.read_ckey(&k).ok());
    for pkg in [package, "mods/liberty.sc2mod/base.sc2assets/", "mods/core.sc2mod/base.sc2assets/"] {
        let (full, low) = (read(pkg, path), read(pkg, &dds::lvl0_name(path)));
        if full.is_none() && low.is_none() {
            continue;
        }
        let d = dds::best(full.as_deref(), low.as_deref())?;
        return to_gpu(&d);
    }
    anyhow::bail!("not found in any package")
}

fn to_gpu(d: &Dds) -> Result<TexCpu> {
    use sc2_formats::dds::Format;
    anyhow::ensure!(!d.cube, "cube map");
    let format = match d.format {
        Format::Bc1 => TextureFormat::Bc1RgbaUnormSrgb,
        Format::Bc2 => TextureFormat::Bc2RgbaUnormSrgb,
        Format::Bc3 => TextureFormat::Bc3RgbaUnormSrgb,
        Format::Bc7 => TextureFormat::Bc7RgbaUnormSrgb,
        Format::Rgba8 => TextureFormat::Rgba8UnormSrgb,
        Format::Bgra8 => TextureFormat::Bgra8UnormSrgb,
        f => anyhow::bail!("unsupported format {f:?}"),
    };
    anyhow::ensure!(!d.format.is_compressed() || (d.width % 4 == 0 && d.height % 4 == 0), "BC texture not block aligned");
    // Keep mips down to 4x4: wgpu wants every BC mip block-aligned in practice.
    let mips: Vec<_> = d.mips().into_iter().take_while(|(w, h, _)| !d.format.is_compressed() || (*w >= 4 && *h >= 4)).collect();
    anyhow::ensure!(!mips.is_empty(), "no mips");
    Ok(TexCpu {
        width: d.width,
        height: d.height,
        mips: mips.len() as u32,
        format,
        data: mips.iter().flat_map(|(_, _, m)| m.iter().copied()).collect(),
    })
}

fn make_image(t: TexCpu) -> Image {
    let mut img = Image::new_uninit(
        Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        t.format,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.data = Some(t.data);
    img.texture_descriptor.mip_level_count = t.mips;
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

fn receive_models(
    models: Option<ResMut<Models>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut models) = models else { return };
    let results: Vec<_> = models.rx.lock().map(|rx| rx.try_iter().collect()).unwrap_or_default();
    for (ty, r) in results {
        let state = match r {
            Ok(cpu) => {
                for (path, tex) in cpu.textures {
                    let h = images.add(make_image(tex));
                    models.textures.insert(path, h);
                }
                let parts = cpu
                    .meshes
                    .into_iter()
                    .map(|m| {
                        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
                        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, m.positions);
                        if !m.normals.is_empty() {
                            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals);
                        }
                        if !m.uvs.is_empty() {
                            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, m.uvs);
                        }
                        mesh.insert_indices(Indices::U32(m.indices));
                        let texture = m.texture.and_then(|p| models.textures.get(&p).cloned());
                        let mat = mats.add(StandardMaterial {
                            base_color: if texture.is_some() { Color::WHITE } else { Color::srgb(0.6, 0.6, 0.6) },
                            base_color_texture: texture,
                            perceptual_roughness: 0.7,
                            alpha_mode: if m.blend == 1 { AlphaMode::Blend } else { AlphaMode::Opaque },
                            double_sided: m.two_sided,
                            cull_mode: if m.two_sided { None } else { Some(bevy::render::render_resource::Face::Back) },
                            ..default()
                        });
                        (meshes.add(mesh), mat)
                    })
                    .collect();
                ModelState::Ready(parts, cpu.height)
            }
            Err(e) => {
                warn!("sc2 models: {e:#}");
                ModelState::Failed
            }
        };
        models.types.insert(ty, state);
    }
}
