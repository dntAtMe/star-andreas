//! `CCoronas::Render` (0x6FAEC0) and `RenderReflections` (0x6FB630), see coronas.md:
//! - main sprite at `pos - nearClip * viewDir`, depth tested, additive, axis-aligned, half
//!   extents `size * W * 70 / (FOV * z)` x `size * H * 70 / (FOV * z) * fogScale` (so the
//!   world size is independent of depth), colour `rgb / fogScale * faded * distFade / 256`
//!   with the squared near fade between 1.3 and 2.3 m;
//! - lens flares (sun / headlight tables): 2D sprites on the line from the screen centre,
//!   raw pixel sizes, no depth test (drawn just in front of the camera);
//! - wet-road reflections: `coronareflect` mirrored below the ground, no depth test.
//!
//! Not ported: the per-element flare line-of-sight test against vehicles and peds, the
//! chromatic headlight ghosts in fog/rain.

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    transform::TransformSystems,
};
use sa_physics::coronas::MAX_CORONAS;

use crate::{
    player::GameRoot,
    saphys::{SaPhys, SaSync},
    stream::{convert_texture, make_image},
    world::{b2g, g2b},
};

/// SA's CDraw::ms_fFOV.
const SA_FOV: f32 = 70.0;
/// Distance at which "no depth test" sprites are drawn (in front of all geometry).
const OVERLAY_DIST: f32 = 0.25;

pub struct CoronasPlugin;

impl Plugin for CoronasPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init).add_systems(PostUpdate, render.after(SaSync).after(TransformSystems::Propagate));
    }
}

#[derive(Resource)]
struct Batches {
    meshes: HashMap<&'static str, (Entity, Handle<Mesh>)>,
    rng: u32,
}

const TEXTURES: [&str; 5] = ["coronastar", "coronamoon", "coronareflect", "coronaheadlightline", "coronaringb"];

fn empty_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn init(
    mut commands: Commands,
    root: Res<GameRoot>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut tex = HashMap::new();
    if let Ok(txd) = std::fs::read(root.0.join("models/particle.txd")).map_err(anyhow::Error::from).and_then(|d| sa_formats::txd::parse(&d)) {
        for t in txd.into_iter().filter_map(|t| convert_texture(t, false)) {
            tex.insert(t.name.clone(), images.add(make_image(t)));
        }
    }
    let mut out = HashMap::new();
    for name in TEXTURES {
        let h = meshes.add(empty_mesh());
        let mat = materials.add(StandardMaterial {
            base_color_texture: tex.get(name).cloned(),
            unlit: true,
            fog_enabled: false,
            cull_mode: None,
            double_sided: true,
            // ONE/ONE: premultiplied with vertex alpha 0.
            alpha_mode: AlphaMode::Premultiplied,
            // After the static shadows and before the FX quads (positive biases only).
            depth_bias: 0.75,
            ..default()
        });
        let e = commands.spawn((Mesh3d(h.clone()), MeshMaterial3d(mat), Transform::default(), NoFrustumCulling)).id();
        out.insert(name, (e, h));
    }
    commands.insert_resource(Batches { meshes: out, rng: 1 });
}

/// Flare tables (0x8D4B68 sun, 0x8D4D88 headlight): (pos, size, r, g, b).
#[rustfmt::skip]
const FLARE_SUN: [(f32, f32, u8, u8, u8); 26] = [
    (4.00, 8.00, 36, 30, 24), (3.00, 11.20, 24, 18, 15), (2.00, 8.00, 24, 12, 12), (1.50, 9.60, 54, 54, 48),
    (1.25, 8.00, 24, 24, 18), (0.80, 22.40, 36, 30, 24), (0.60, 6.40, 24, 15, 12), (0.25, 16.00, 36, 30, 30),
    (0.10, 9.60, 18, 18, 18), (0.05, 22.40, 36, 30, 24), (-0.03, 4.80, 18, 18, 18), (-0.10, 9.60, 42, 42, 42),
    (-0.30, 8.00, 18, 6, 6), (-0.40, 96.00, 18, 12, 9), (-0.55, 6.40, 18, 18, 12), (-0.75, 22.40, 42, 24, 18),
    (-0.90, 8.32, 21, 12, 18), (-1.00, 17.60, 42, 13, 18), (-1.20, 5.60, 21, 12, 12), (-1.35, 14.40, 42, 42, 24),
    (-1.70, 86.88, 21, 15, 15), (-2.00, 8.00, 48, 30, 30), (-2.50, 7.20, 21, 15, 12), (-3.00, 22.40, 42, 30, 24),
    (-6.00, 38.40, 42, 42, 30), (-9.00, 22.40, 42, 30, 36),
];
#[rustfmt::skip]
const FLARE_HEADLIGHT: [(f32, f32, u8, u8, u8); 26] = [
    (4.00, 5.0, 60, 60, 60), (3.00, 7.0, 40, 40, 40), (2.00, 5.0, 40, 40, 40), (1.50, 6.0, 90, 90, 90),
    (1.25, 5.0, 40, 40, 40), (0.80, 14.0, 60, 60, 60), (0.60, 4.0, 40, 40, 40), (0.25, 10.0, 60, 60, 60),
    (0.10, 6.0, 30, 30, 30), (0.05, 14.0, 50, 50, 50), (-0.03, 3.0, 30, 30, 30), (-0.10, 6.0, 60, 60, 60),
    (-0.30, 5.0, 30, 30, 30), (-0.40, 60.0, 30, 30, 30), (-0.55, 4.0, 40, 40, 40), (-0.75, 14.0, 50, 50, 50),
    (-0.90, 5.2, 35, 35, 35), (-1.00, 11.0, 55, 55, 55), (-1.20, 3.5, 35, 35, 35), (-1.35, 9.0, 50, 50, 50),
    (-1.70, 54.3, 35, 35, 35), (-2.00, 5.0, 50, 50, 50), (-2.50, 4.5, 35, 35, 35), (-3.00, 14.0, 50, 50, 50),
    (-6.00, 24.0, 70, 70, 70), (-9.00, 14.0, 70, 50, 70),
];

/// A camera-aligned quad in GTA space: centre, half extents along camera right / up, linear colour.
struct Quad {
    centre: Vec3,
    hx: f32,
    hy: f32,
    color: [f32; 3],
}

#[allow(clippy::too_many_arguments)]
fn render(
    mut sa: ResMut<SaPhys>,
    batches: Option<ResMut<Batches>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tfs: Query<&mut Transform, Without<Camera3d>>,
    cam: Single<(&Camera, &GlobalTransform, &Projection), With<Camera3d>>,
) {
    let Some(mut batches) = batches else { return };
    let (camera, cam_gt, proj) = *cam;
    let Projection::Perspective(p) = proj else { return };
    let Some(size) = camera.physical_viewport_size() else { return };
    let (sw, sh) = (size.x as f32, size.y as f32);
    let tan_v = (p.fov * 0.5).tan();
    let tan_h = tan_v * p.aspect_ratio;
    // World half extent per unit corona size (z cancels out).
    let kx = 70.0 / SA_FOV * 2.0 * tan_h;
    let ky = 70.0 / SA_FOV * 2.0 * tan_v;
    let g = |v: Vec3| Vec3::from(b2g(v));
    let cam_pos = g(cam_gt.translation());
    let fwd = g(cam_gt.forward().as_vec3());
    let right = g(cam_gt.right().as_vec3());
    let up = g(cam_gt.up().as_vec3());
    let far = p.far;
    // Screen pixel position of a GTA point, with its view depth.
    let screen = |w: Vec3| -> Option<(Vec2, f32)> {
        let d = w - cam_pos;
        let z = d.dot(fwd);
        if z <= p.near + 1.0 || z >= far {
            return None;
        }
        let x = d.dot(right) / (z * tan_h);
        let y = d.dot(up) / (z * tan_v);
        Some((Vec2::new((x + 1.0) * 0.5 * sw, (1.0 - y) * 0.5 * sh), z))
    };
    // A point at OVERLAY_DIST along the ray through screen pixel s, and pixel -> world scale there.
    let overlay = |s: Vec2| -> (Vec3, f32, f32) {
        let x = s.x / sw * 2.0 - 1.0;
        let y = 1.0 - s.y / sh * 2.0;
        let pnt = cam_pos + (fwd + right * (x * tan_h) + up * (y * tan_v)) * OVERLAY_DIST;
        (pnt, OVERLAY_DIST * 2.0 * tan_h / sw, OVERLAY_DIST * 2.0 * tan_v / sh)
    };
    let fog = sa.world.weather.foggyness;
    let wet = sa.world.weather.wet_roads;
    let frame = sa.world.frame;
    let mut quads: HashMap<&'static str, Vec<Quad>> = HashMap::new();
    let lin = |c: [f32; 3]| {
        let l = Color::srgb(c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0)).to_linear();
        [l.red, l.green, l.blue]
    };

    for i in 0..MAX_CORONAS {
        let Some(c) = sa.world.coronas.slots[i] else { continue };
        if c.faded == 0 && c.intensity == 0 {
            continue;
        }
        let Some(p0) = sa.world.corona_world_pos(&c) else { continue };
        let Some((s0, z)) = screen(p0) else {
            sa.world.coronas.slots[i].as_mut().unwrap().off_screen = true;
            continue;
        };
        sa.world.coronas.slots[i].as_mut().unwrap().off_screen = s0.x < 0.0 || s0.y < 0.0 || s0.x > sw || s0.y > sh;
        if c.faded != 0 && z < c.far_clip {
            let t = c.far_clip * 0.5;
            let dist_fade = if z < t { 1.0 } else { 1.0 - (z - t) / t };
            let intensity = (c.faded as f32 * dist_fade) as i32 as f32;
            let mut s = s0;
            if let Some(tex) = c.tex {
                let fog_scale = 1.0 + z.min(40.0) * fog * 0.025;
                let pp = p0 - (p0 - cam_pos).normalize_or_zero() * c.near_clip;
                if let Some((sp, zp)) = screen(pp) {
                    s = sp;
                    if zp >= 1.3 {
                        let k = if zp < 2.3 { ((zp - 1.3) * 255.0) as i32 as f32 / 256.0 } else { 1.0 };
                        let col: [f32; 3] = std::array::from_fn(|n| {
                            let v = (c.rgb[n] as f32 / fog_scale) as i32 as f32;
                            v * k * (intensity * k) / 256.0 / 255.0
                        });
                        quads.entry(tex.name()).or_default().push(Quad {
                            centre: pp,
                            hx: c.size * kx,
                            hy: c.size * ky * fog_scale,
                            color: lin(col),
                        });
                    }
                }
            }
            if c.flare != 0 {
                let table = if c.flare == 1 { &FLARE_SUN } else { &FLARE_HEADLIGHT };
                batches.rng = batches.rng.wrapping_mul(214_013).wrapping_add(2_531_011);
                let r = ((batches.rng >> 16) & 0x7FFF) as f32;
                let k = (r * (1.0 / 32767.0) * 0.3 + 0.7) * c.faded as f32 * (1.0 / 65536.0);
                let centre = Vec2::new((sw as i32 / 2) as f32, (sh as i32 / 2) as f32);
                for &(pos, size, er, eg, eb) in table {
                    let col = [er, eg, eb];
                    let col: [f32; 3] = std::array::from_fn(|n| {
                        ((c.rgb[n] as f32 * col[n] as f32 * k) as i32 as f32) * 255.0 / 256.0 / 255.0
                    });
                    let sp = centre + (s - centre) * pos;
                    let (wp, px, py) = overlay(sp);
                    quads.entry("coronastar").or_default().push(Quad {
                        centre: wp,
                        hx: 4.0 * size * px,
                        hy: 4.0 * size * py,
                        color: lin(col),
                    });
                }
            }
        }
    }

    // Wet-road reflections.
    if wet > 0.0 {
        for i in 0..MAX_CORONAS {
            let Some(c) = sa.world.coronas.slots[i] else { continue };
            if (c.faded == 0 && c.intensity == 0) || !c.reflection {
                continue;
            }
            let Some(p0) = sa.world.corona_world_pos(&c) else { continue };
            let probe = !c.valid_ground_height || (frame.wrapping_add(i as u32) & 15) == 0;
            if probe {
                if let Some((_, _, cp)) = sa.world.line_of_sight(p0, p0 - Vec3::Z * 1000.0, true, None) {
                    let s = sa.world.coronas.slots[i].as_mut().unwrap();
                    s.valid_ground_height = true;
                    s.height_above_ground = p0.z - cp.point.z;
                }
            }
            let c = sa.world.coronas.slots[i].unwrap();
            let h = c.height_above_ground;
            if !c.valid_ground_height || !(h < 20.0) || !(p0.z - h <= cam_pos.z) {
                continue;
            }
            let m = Vec3::new(p0.x, p0.y, p0.z - 2.0 * h);
            let Some((s, z)) = screen(m) else { continue };
            let max_d = (c.far_clip * 0.75).min(55.0);
            if !(z < max_d) {
                continue;
            }
            let t = max_d * 0.5;
            let f = if z < t { 1.0 } else { (1.0 - (z - t) / t).clamp(0.0, 1.0) };
            let intensity = (wet * f * (20.0 - h) * 230.0 * 0.05) as i32 as f32;
            let col: [f32; 3] = std::array::from_fn(|n| ((c.rgb[n] as f32 * intensity / 256.0) as i32 as f32) * 128.0 / 256.0 / 255.0);
            let (wp, px, py) = overlay(s);
            // Pixel half sizes w*size*0.75 and h*size*2 at the mirror depth.
            let w_px = sw / z * 70.0 / SA_FOV;
            let h_px = sh / z * 70.0 / SA_FOV;
            quads.entry("coronareflect").or_default().push(Quad {
                centre: wp,
                hx: w_px * c.size * 0.75 * px,
                hy: h_px * c.size * 2.0 * py,
                color: lin(col),
            });
        }
    } else {
        for s in sa.world.coronas.slots.iter_mut().flatten() {
            s.valid_ground_height = false;
        }
    }

    let cam_b = cam_gt.translation();
    for name in TEXTURES {
        let Some((e, h)) = batches.meshes.get(name) else { continue };
        if let Ok(mut tf) = tfs.get_mut(*e) {
            tf.translation = cam_b;
        }
        let Some(mut m) = meshes.get_mut(h) else { continue };
        let list = quads.get(name).map(Vec::as_slice).unwrap_or(&[]);
        if list.is_empty() {
            *m = empty_mesh();
            continue;
        }
        let (mut pos, mut uv, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for q in list {
            let b = pos.len() as u32;
            for (sx, sy, tu, tv) in [(-1.0, 1.0, 0.0, 0.0), (1.0, 1.0, 1.0, 0.0), (-1.0, -1.0, 0.0, 1.0), (1.0, -1.0, 1.0, 1.0)] {
                let w = q.centre + right * (sx * q.hx) + up * (sy * q.hy);
                pos.push((g2b(w.to_array()) - cam_b).to_array());
                uv.push([tu, tv]);
                col.push([q.color[0], q.color[1], q.color[2], 0.0]);
            }
            idx.extend([b, b + 2, b + 1, b + 1, b + 2, b + 3]);
        }
        let n = pos.len();
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
        m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
        m.insert_indices(Indices::U32(idx));
    }
}
