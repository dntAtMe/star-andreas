//! Time-of-day visuals from `CTimeCycle` (timecycle.md):
//! - the gradient sky (`CClouds::RenderSkyPolys`, 5 quads, pushed out to the far plane so
//!   the world occludes it like the original's "drawn first, no z" sky),
//! - far clip and linear fog (sky-bottom colour), the clear colour,
//! - map geometry globals (DN balance, Amb, fog) for the world material,
//! - lights for peds/cars/objects (`SetLightColoursForPedsCarsAndObjects`: Amb_Obj ambient,
//!   a fixed-direction directional light scaled by DirMult, which is 0 on PC),
//! - sky sprites: moon, stars, low clouds, rainbow (the sun is a corona, see coronas.rs).
//!
//! Not ported: CCoronas::Render details (sprites are plain additive quads, no fade or
//! lens flare, no sun line-of-sight dazzle), the fluffy / volumetric cloud layers, SF
//! moving fog, plane trails, the shooting star, the sun reflection on water.

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    core_pipeline::tonemapping::Tonemapping,
    mesh::{Indices, PrimitiveTopology},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::storage::ShaderBuffer,
    transform::TransformSystems,
};
use sa_physics::timecycle::TimeCycle;

use crate::{
    debug::DebugUi,
    player::GameRoot,
    saphys::{SaPhys, SaSync},
    stream::{convert_texture, make_image},
    world::{b2g, g2b},
    world_material::{GlobalsData, WorldGlobals},
};

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init)
            .add_systems(Update, no_tonemapping)
            .add_systems(PostUpdate, update.after(SaSync).after(TransformSystems::Propagate));
    }
}

/// Sky meshes: the gradient box and one additive sprite batch per texture.
#[derive(Resource)]
struct Sky {
    box_mesh: (Entity, Handle<Mesh>),
    sprites: HashMap<&'static str, (Entity, Handle<Mesh>)>,
    rng: u32,
}

fn empty_mesh(color: bool) -> Mesh {
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]));
    if color {
        m.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3]);
    }
    m
}

fn init(
    mut commands: Commands,
    root: Res<GameRoot>,
    mut sa: ResMut<SaPhys>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    match std::fs::read(root.0.join("data/timecyc.dat")) {
        Ok(d) => sa.world.timecycle = Some(TimeCycle::parse(&String::from_utf8_lossy(&d))),
        Err(e) => warn!("timecyc.dat: {e}"),
    }

    let mut tex = HashMap::new();
    if let Ok(d) = std::fs::read(root.0.join("models/particle.txd")) {
        if let Ok(txd) = sa_formats::txd::parse(&d) {
            for t in txd.into_iter().filter_map(|t| convert_texture(t, false)) {
                tex.insert(t.name.clone(), images.add(make_image(t)));
            }
        }
    }
    let mut spawn = |mesh: Mesh, mat: StandardMaterial| {
        let h = meshes.add(mesh);
        let e = commands
            .spawn((Mesh3d(h.clone()), MeshMaterial3d(materials.add(mat)), Transform::default(), NoFrustumCulling))
            .id();
        (e, h)
    };
    let box_mesh = spawn(
        empty_mesh(true),
        StandardMaterial { unlit: true, fog_enabled: false, cull_mode: None, double_sided: true, ..default() },
    );
    let mut sprites = HashMap::new();
    for name in ["coronastar", "coronamoon", "cloud1"] {
        let e = spawn(
            empty_mesh(true),
            StandardMaterial {
                base_color_texture: tex.get(name).cloned(),
                unlit: true,
                fog_enabled: false,
                cull_mode: None,
                double_sided: true,
                // ONE/ONE: premultiplied with alpha 0 adds the colour.
                alpha_mode: AlphaMode::Premultiplied,
                ..default()
            },
        );
        sprites.insert(name, e);
    }
    commands.insert_resource(Sky { box_mesh, sprites, rng: 1 });
}

/// SA has no tonemapping: colours go to the screen as computed.
fn no_tonemapping(mut cams: Query<&mut Tonemapping, (With<Camera3d>, Changed<Tonemapping>)>) {
    for mut t in &mut cams {
        if *t != Tonemapping::None {
            *t = Tonemapping::None;
        }
    }
}

fn rgb(c: [f32; 3]) -> Color {
    Color::srgb_u8(c[0] as u8, c[1] as u8, c[2] as u8)
}

fn lin(c: Color) -> [f32; 4] {
    let l = c.to_linear();
    [l.red, l.green, l.blue, l.alpha]
}

/// One additive sprite: world centre, half sizes (right, up), gamma colour 0..255.
struct Sprite {
    pos: Vec3,
    half: Vec2,
    color: [f32; 3],
    /// Roll about the view axis (radians).
    roll: f32,
}

#[allow(clippy::too_many_arguments)]
fn update(
    mut sa: ResMut<SaPhys>,
    mut sky: ResMut<Sky>,
    dbg: Res<DebugUi>,
    globals: Option<Res<WorldGlobals>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut clear: ResMut<ClearColor>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut dir_light: Query<(&mut DirectionalLight, &mut Transform), Without<Camera3d>>,
    mut tfs: Query<&mut Transform, (Without<Camera3d>, Without<DirectionalLight>)>,
    mut cam: Query<(&GlobalTransform, &mut Projection, Option<&mut DistanceFog>), With<Camera3d>>,
) {
    let Ok((cam_gt, mut proj, fog)) = cam.single_mut() else { return };
    let Some(tc) = sa.world.timecycle.as_mut() else { return };
    tc.brightness = dbg.brightness;
    let c = tc.current;
    let bhg = tc.below_horizon_grey;
    let lights_mult = tc.lights_mult;
    let w = &sa.world.weather;
    let lightning = w.lightning_flash;
    let (fogginess, clouds, rainbow) = (w.foggyness, w.cloud_coverage, w.rainbow);
    let extra_sunny = w.extra_sunnyness;
    let clock = sa.world.clock.clone();
    let dn = clock.dn_balance();

    let cam_pos_b = cam_gt.translation();
    let cam_pos = Vec3::from(b2g(cam_pos_b));
    let fwd = Vec3::from(b2g(cam_gt.forward().as_vec3()));
    let right_cam = Vec3::from(b2g(cam_gt.right().as_vec3()));
    let up_cam = Vec3::from(b2g(cam_gt.up().as_vec3()));

    // ---------------------------------------------------------------- far clip, fog, clear
    let far = c.far_clip.max(50.0);
    let mut sky_bottom = c.sky_bottom;
    if lightning {
        sky_bottom = [255.0; 3];
    }
    if let Projection::Perspective(p) = &mut *proj {
        p.far = far;
    }
    let fog_col = rgb(sky_bottom);
    if let Some(mut f) = fog {
        f.color = fog_col;
        f.falloff = FogFalloff::Linear { start: c.fog_start, end: far };
        if !dbg.fog {
            f.falloff = FogFalloff::Linear { start: 1e6, end: 1e6 + 1.0 };
        }
    }
    clear.0 = fog_col;
    if let Some(g) = globals {
        let amb = if lightning { Vec3::ONE } else { c.ambient * lights_mult };
        let data = GlobalsData {
            dn,
            ambient: amb,
            fog: dbg.fog.then(|| (c.fog_start, far, Vec3::from(sky_bottom) / 255.0)),
        };
        if let Some(mut b) = buffers.get_mut(&g.0) {
            b.set_data(data.pack());
        }
    }

    // ---------------------------------------------------------------- lights for peds / cars
    // Bevy's ambient term is albedo * colour * brightness * exposure (~1/980 by default).
    let amb_obj = if lightning { Vec3::ONE } else { c.ambient_obj * lights_mult };
    ambient.color = Color::srgb(amb_obj.x.min(1.0), amb_obj.y.min(1.0), amb_obj.z.min(1.0));
    // The ambient term is per entity (SetupLighting's m): applied as emissive by dynlight.rs.
    ambient.brightness = 0.0;
    let dir_mult = dbg.dir_mult_override.unwrap_or(c.dir_mult);
    for (mut l, mut tf) in &mut dir_light {
        let d = dir_mult * 0.996_093_75 * lights_mult;
        l.illuminance = d * std::f32::consts::PI * 980.0;
        // Fixed: from (-0.5, -0.5, 0.707) toward (0.5, 0.5, -0.707) (m_vecDirnLightToSun).
        *tf = Transform::default().looking_to(g2b([0.5, 0.5, -0.707_106_8]), Vec3::Y);
    }

    // ---------------------------------------------------------------- gradient sky
    let f2 = Vec3::new(fwd.x, fwd.y, 0.0).normalize_or(Vec3::Y);
    let right = Vec3::new(f2.y, -f2.x, 0.0);
    let t = ((cam_pos.z - 25.0) * 0.0125).clamp(0.0, 1.0).max(fogginess);
    let grey: [f32; 3] = std::array::from_fn(|k| ((bhg[k] * (1.0 - t) + sky_bottom[k] * t) as i32) as f32);
    let (top, bot) = (rgb(c.sky_top), rgb(sky_bottom));
    let grey_c = rgb(grey);
    // Scale the 30 m box out to just inside the far plane (projection unchanged).
    let s = 30.0 * (0.97 * far / (30.0 * 1.79));
    let v = |fw: f32, r: f32, u: f32| cam_pos + (f2 * fw + right * r + Vec3::Z * u) * s;
    let quads: [([Vec3; 4], Color, Color); 5] = [
        ([v(1., -1.4, 0.5), v(1., 1.4, 0.5), v(1., -1.4, 0.), v(1., 1.4, 0.)], top, bot),
        ([v(1., -1.4, 0.), v(1., 1.4, 0.), v(1., -1.4, -0.1), v(1., 1.4, -0.1)], bot, bot),
        ([v(1., -1.4, -0.1), v(1., 1.4, -0.1), v(1., -1.4, -0.3), v(1., 1.4, -0.3)], bot, grey_c),
        ([v(1., -1.4, 0.5), v(1., 1.4, 0.5), v(-1., -1.4, 0.5), v(-1., 1.4, 0.5)], top, top),
        ([v(1., -1.4, -0.3), v(1., 1.4, -0.3), v(-1., -1.4, -0.3), v(-1., 1.4, -0.3)], grey_c, grey_c),
    ];
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    if dbg.sky {
        for (q, c1, c2) in quads {
            let b = pos.len() as u32;
            for (k, p) in q.iter().enumerate() {
                pos.push((g2b(p.to_array()) - cam_pos_b).to_array());
                col.push(lin(if k < 2 { c1 } else { c2 }));
            }
            idx.extend([b, b + 2, b + 1, b + 1, b + 2, b + 3]);
        }
    }
    set_mesh(&mut meshes, &sky.box_mesh.1, pos, None, col, idx);
    if let Ok(mut tf) = tfs.get_mut(sky.box_mesh.0) {
        tf.translation = cam_pos_b;
    }

    // ---------------------------------------------------------------- sprites
    let mut star: Vec<Sprite> = Vec::new();
    let mut moon: Vec<Sprite> = Vec::new();
    let mut cloud: Vec<Sprite> = Vec::new();
    if dbg.sky {
        // The sun is two coronas (CCoronas::DoSunAndMoon, coronas.rs).
        let cover = 1.0 - clouds.max(fogginess);
        // Moon: visible 00:00..07:20, brightest at 03:40, fixed direction.
        let m = (clock.hours as u32 * 60 + clock.minutes as u32) as f32 + clock.seconds as f32 / 60.0 - 220.0;
        let d = m.abs() as i32;
        if d < 220 {
            let a = ((220 - d) as f32 * cover) as i32;
            if a > 0 {
                let s = 3.0 * 2.0 + 4.0;
                let a = a as f32;
                moon.push(Sprite {
                    pos: cam_pos + Vec3::new(0.0, -100.0, 15.0),
                    half: Vec2::splat(s),
                    color: [a, a, ((a * 0.85) as i32) as f32],
                    roll: 0.0,
                });
            }
        }
        // Stars (22:00..05:59).
        let (h, mi) = (clock.hours as i32, clock.minutes as i32);
        let mut a = match h {
            23 | 0..=4 => 255,
            22 => mi * 255 / 60,
            5 => (60 - mi) * 255 / 60,
            _ => 0,
        };
        if a != 0 {
            a = (a as f32 * cover) as i32;
            const S1: [f32; 9] = [0.0, 0.05, 0.13, 0.4, 0.7, 0.6, 0.27, 0.55, 0.75];
            const S2: [f32; 9] = [0.0, 0.45, 0.9, 1.0, 0.85, 0.52, 0.48, 0.35, 0.2];
            const S3: [f32; 9] = [1.0, 1.4, 0.9, 1.0, 0.6, 1.5, 1.3, 1.0, 0.8];
            for i in 0..12 {
                let k = i % 9;
                let p = cam_pos + Vec3::new(if i < 9 { 100.0 } else { -100.0 }, -90.0 * S1[k], 10.0 + 80.0 * S2[k]);
                let r = next_rand(&mut sky.rng);
                let b = ((1.0 - (r & 31) as f32 * 0.015) * a as f32) as i32 as f32;
                star.push(Sprite { pos: p, half: Vec2::splat(S3[k] * 0.8), color: [b; 3], roll: 0.0 });
            }
            let r = next_rand(&mut sky.rng);
            let b = (((r & 127) as f32 * 0.001_562_5 + 0.5) * a as f32) as i32 as f32;
            star.push(Sprite { pos: cam_pos + Vec3::new(100.0, -90.0, 10.0), half: Vec2::splat(5.0), color: [b; 3], roll: 0.0 });
        }
        // Low clouds (only in plain SUNNY weathers).
        let q = extra_sunny.max(fogginess).max(clouds);
        let lc: [f32; 3] = std::array::from_fn(|k| ((c.low_clouds[k] * (1.0 - q)) as i32) as f32);
        if lc != [0.0; 3] {
            let mut sr = (right_cam.x * right_cam.x + right_cam.y * right_cam.y).sqrt();
            if up_cam.z < 0.0 {
                sr = -sr;
            }
            let roll = right_cam.z.atan2(sr);
            const X: [f32; 12] = [1.0, 0.7, 0.0, -0.7, -1.0, -0.7, 0.0, 0.7, 0.8, -0.8, 0.4, -0.4];
            const Y: [f32; 12] = [0.0, -0.7, -1.0, -0.7, 0.0, 0.7, 1.0, 0.7, 0.4, 0.4, -0.8, -0.8];
            const Z: [f32; 12] = [0.0, 1.0, 0.5, 0.0, 1.0, 0.3, 0.9, 0.4, 1.3, 1.4, 1.2, 1.7];
            for i in 0..12 {
                let p = Vec3::new(cam_pos.x + 800.0 * X[i], cam_pos.y + 800.0 * Y[i], 40.0 + 60.0 * Z[i]);
                cloud.push(Sprite { pos: p, half: Vec2::new(320.0, 40.0), color: lc, roll });
            }
        }
        // Rainbow: six coronastar strips to the north.
        if rainbow > 0.0 {
            const R: [f32; 6] = [30.0, 30.0, 30.0, 10.0, 0.0, 15.0];
            const G: [f32; 6] = [0.0, 15.0, 30.0, 30.0, 0.0, 0.0];
            const B: [f32; 6] = [0.0, 0.0, 0.0, 10.0, 30.0, 30.0];
            for i in 0..6 {
                let f = |x: f32| ((x * rainbow) as i32) as f32;
                star.push(Sprite {
                    pos: cam_pos + Vec3::new(1.5 * i as f32, 100.0, 5.0),
                    half: Vec2::new(2.0, 50.0),
                    color: [f(R[i]), f(G[i]), f(B[i])],
                    roll: 0.0,
                });
            }
        }
    }

    let push_far = |sp: &mut Vec<Sprite>| {
        // Sky sprites are drawn before the world with no depth: move them behind it.
        for s in sp.iter_mut() {
            let d = s.pos - cam_pos;
            let len = d.length().max(1e-3);
            let k = (0.96 * far / len).max(1.0);
            s.pos = cam_pos + d * k;
            s.half *= k;
        }
    };
    push_far(&mut star);
    push_far(&mut moon);
    push_far(&mut cloud);
    for (name, list) in [("coronastar", &star), ("coronamoon", &moon), ("cloud1", &cloud)] {
        let Some((e, h)) = sky.sprites.get(name) else { continue };
        let (mut pos, mut uv, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for s in list {
            let (sn, cs) = s.roll.sin_cos();
            let r = right_cam * cs + up_cam * sn;
            let u = up_cam * cs - right_cam * sn;
            let b = pos.len() as u32;
            for (kx, ky, tu, tv) in [(-1.0, 1.0, 0.0, 0.0), (1.0, 1.0, 1.0, 0.0), (-1.0, -1.0, 0.0, 1.0), (1.0, -1.0, 1.0, 1.0)] {
                let p = s.pos + r * (kx * s.half.x) + u * (ky * s.half.y);
                pos.push((g2b(p.to_array()) - cam_pos_b).to_array());
                uv.push([tu, tv]);
                let cl = rgb(s.color).to_linear();
                col.push([cl.red, cl.green, cl.blue, 0.0]);
            }
            idx.extend([b, b + 2, b + 1, b + 1, b + 2, b + 3]);
        }
        set_mesh(&mut meshes, h, pos, Some(uv), col, idx);
        if let Ok(mut tf) = tfs.get_mut(*e) {
            tf.translation = cam_pos_b;
        }
    }
}

fn next_rand(s: &mut u32) -> u32 {
    *s = s.wrapping_mul(214_013).wrapping_add(2_531_011);
    (*s >> 16) & 0x7FFF
}

fn set_mesh(
    meshes: &mut Assets<Mesh>,
    h: &Handle<Mesh>,
    pos: Vec<[f32; 3]>,
    uv: Option<Vec<[f32; 2]>>,
    col: Vec<[f32; 4]>,
    idx: Vec<u32>,
) {
    let Some(mut m) = meshes.get_mut(h) else { return };
    if idx.is_empty() {
        *m = empty_mesh(true);
        return;
    }
    let n = pos.len();
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv.unwrap_or_else(|| vec![[0.0; 2]; n]));
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    m.insert_indices(Indices::U32(idx));
}
