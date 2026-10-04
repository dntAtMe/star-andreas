//! `CWaterLevel` rendering (water.md §4): data/water.dat, `RenderWater` (0x6EF650).
//!
//! * Two layers of `waterclear256` (particle.txd), SRCALPHA/INVSRCALPHA, z-write off, fog on,
//!   no culling. Layer 0: uv = pos·0.08 + offset0, alpha A0; layer 1: uv = pos·0.04 + offset1,
//!   alpha A (A = timecyc water alpha / 2, A0 = A·256/(256−A)).
//! * Flat water (`RenderFlatWaterRectangle`): the data heights, colour = timecyc water RGB·0.577.
//! * Inside the detail box (camera ±48, even aligned): a 2-unit lattice with
//!   `CalculateWavesForCoordinate` heights and colour multiplier, the waves fading out from 36
//!   to 48 units (`RenderHighDetailWaterRectangle`).
//! * Beyond the 12×12 grid: 500-unit blocks of sea at z 0 (big waves 1), and the sea bed
//!   `seabd32` at z −70 (colour 80,80,80, 8 repeats per block) under them and under the outer
//!   20-unit strip of the border cells.
//! * Texture scroll from the nearest vertex flow (`FindNearestWaterAndItsFlow`, every 32 frames).
//!
//! Splitting differs from SA: every quad is cut into its two triangles (the §2.1 split) and
//! each triangle is clipped against the detail box and the 2-unit cells, which gives the same
//! surface. Not ported: water fog, boat wakes, water1.dat, the underwater draw order.

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    transform::TransformSystems,
};
use sa_physics::water::{RenPar, WaterLevel, wave_render, wflag};

use crate::{
    player::GameRoot,
    saphys::{SaPhys, SaSync},
    stream::{convert_texture, make_image},
    world::{b2g, g2b},
};

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init).add_systems(PostUpdate, draw.after(SaSync).after(TransformSystems::Propagate));
    }
}

#[derive(Resource)]
struct WaterR {
    layers: [(Entity, Handle<Mesh>); 2],
    seabed: (Entity, Handle<Mesh>),
    /// U1, V1, U2, V2 (0x8D3824..30), start 0.5.
    scroll: [f32; 4],
    flow: Vec2,
    target: Vec2,
    frame: u32,
}

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
    mut sa: ResMut<SaPhys>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    match std::fs::read(root.0.join("data/water.dat")) {
        Ok(d) => {
            let w = WaterLevel::parse(&String::from_utf8_lossy(&d));
            info!("water.dat: {} vertices, {} quads, {} triangles", w.vertices.len(), w.quads.len(), w.tris.len());
            sa.world.water = Some(std::sync::Arc::new(w));
        }
        Err(e) => warn!("water.dat: {e}"),
    }
    let mut water_tex = None;
    let mut bed_tex = None;
    if let Ok(txd) = std::fs::read(root.0.join("models/particle.txd")).map_err(anyhow::Error::from).and_then(|d| sa_formats::txd::parse(&d)) {
        for t in txd.into_iter().filter_map(|t| convert_texture(t, false)) {
            match t.name.as_str() {
                "waterclear256" => water_tex = Some(images.add(make_image(t))),
                "seabd32" => bed_tex = Some(images.add(make_image(t))),
                _ => {}
            }
        }
    }
    let mut spawn = |mat: StandardMaterial| {
        let h = meshes.add(empty_mesh());
        let e = commands.spawn((Mesh3d(h.clone()), MeshMaterial3d(materials.add(mat)), Transform::default(), NoFrustumCulling)).id();
        (e, h)
    };
    let seabed = spawn(StandardMaterial { base_color_texture: bed_tex, unlit: true, ..default() });
    // Layer 0 is drawn first: Bevy sorts transparent items by view z + depth bias, ascending.
    let layer = |bias: f32| StandardMaterial {
        base_color_texture: water_tex.clone(),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        depth_bias: bias,
        ..default()
    };
    let layers = [spawn(layer(0.0)), spawn(layer(1.0))];
    commands.insert_resource(WaterR { layers, seabed, scroll: [0.5; 4], flow: Vec2::ZERO, target: Vec2::ZERO, frame: 0 });
}

/// A planar triangle with its `CRenPar`s.
#[derive(Clone, Copy)]
struct Tri {
    p: [Vec2; 3],
    rp: [RenPar; 3],
}

impl Tri {
    /// The plane through the three `CRenPar`s at `q`.
    fn at(&self, q: Vec2) -> RenPar {
        let [a, b, c] = self.p;
        let d = (b - a).perp_dot(c - a);
        if d.abs() < 1e-9 {
            return self.rp[0];
        }
        let u = (q - a).perp_dot(c - a) / d;
        let v = (b - a).perp_dot(q - a) / d;
        let l = |f: fn(&RenPar) -> f32| f(&self.rp[0]) + (f(&self.rp[1]) - f(&self.rp[0])) * u + (f(&self.rp[2]) - f(&self.rp[0])) * v;
        RenPar { z: l(|r| r.z), big: l(|r| r.big), small: l(|r| r.small) }
    }
}

/// Clip a convex polygon to `n·p <= d`.
fn clip(poly: &[Vec2], n: Vec2, d: f32) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (da, db) = (n.dot(a) - d, n.dot(b) - d);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(a + (b - a) * (da / (da - db)));
        }
    }
    out
}

fn clip_rect(poly: &[Vec2], min: Vec2, max: Vec2) -> Vec<Vec2> {
    let mut p = clip(poly, Vec2::X, max.x);
    p = clip(&p, -Vec2::X, -min.x);
    p = clip(&p, Vec2::Y, max.y);
    clip(&p, -Vec2::Y, -min.y)
}

/// Vertex buffers shared by both layers (positions in Bevy space relative to the camera).
#[derive(Default)]
struct Geo {
    pos: Vec<[f32; 3]>,
    /// GTA x, y (for the layer UVs).
    xy: Vec<Vec2>,
    /// Colour multiplier per vertex (0.577 flat, the wave multiplier detailed).
    cm: Vec<f32>,
    idx: Vec<u32>,
}

impl Geo {
    fn fan(&mut self, verts: &[(Vec2, f32, f32)], origin: Vec3) {
        if verts.len() < 3 {
            return;
        }
        let base = self.pos.len() as u32;
        for &(q, z, cm) in verts {
            self.pos.push((g2b([q.x, q.y, z]) - origin).to_array());
            self.xy.push(q);
            self.cm.push(cm);
        }
        for k in 1..verts.len() as u32 - 1 {
            self.idx.extend([base, base + k, base + k + 1]);
        }
    }
}

struct Waves {
    cam: Vec2,
    wavy: f32,
    now: u32,
}

impl Waves {
    /// Height offset and colour multiplier at a lattice point (`RenderHighDetailWaterRectangle`).
    fn at(&self, x: i32, y: i32, rp: RenPar) -> (f32, f32) {
        let d = Vec2::new(x as f32, y as f32).distance(self.cam) / 48.0;
        let fade = if d <= 0.75 { 1.0 } else { ((1.0 - d) * 4.0).max(0.0) };
        wave_render(x, y, rp.big * fade, rp.small * fade, self.wavy, self.now)
    }
}

/// One planar triangle: flat outside the detail box, the 2-unit wave lattice inside.
fn emit(t: &Tri, bmin: Vec2, bmax: Vec2, w: &Waves, g: &mut Geo, origin: Vec3) {
    const BIG: f32 = 1.0e6;
    let poly = t.p.to_vec();
    // Outside strips: left, right, below, above.
    for (min, max) in [
        (Vec2::new(-BIG, -BIG), Vec2::new(bmin.x, BIG)),
        (Vec2::new(bmax.x, -BIG), Vec2::new(BIG, BIG)),
        (Vec2::new(bmin.x, -BIG), Vec2::new(bmax.x, bmin.y)),
        (Vec2::new(bmin.x, bmax.y), Vec2::new(bmax.x, BIG)),
    ] {
        let p = clip_rect(&poly, min, max);
        let v: Vec<_> = p.iter().map(|&q| (q, t.at(q).z, 0.577)).collect();
        g.fan(&v, origin);
    }
    let inner = clip_rect(&poly, bmin, bmax);
    if inner.len() < 3 {
        return;
    }
    let (mut lo, mut hi) = (Vec2::splat(BIG), Vec2::splat(-BIG));
    for q in &inner {
        lo = lo.min(*q);
        hi = hi.max(*q);
    }
    let (x0, x1) = ((lo.x / 2.0).floor() as i32 * 2, (hi.x / 2.0).ceil() as i32 * 2);
    let (y0, y1) = ((lo.y / 2.0).floor() as i32 * 2, (hi.y / 2.0).ceil() as i32 * 2);
    let mut cx = x0;
    while cx < x1 {
        let mut cy = y0;
        while cy < y1 {
            let c0 = Vec2::new(cx as f32, cy as f32);
            let cell = clip_rect(&inner, c0, c0 + 2.0);
            if cell.len() >= 3 {
                // Wave offsets at the 4 corners, interpolated over the cell's two triangles
                // (00,20,02) / (22,02,20), as the vertex grid and GetWaterLevel do.
                let h = |dx: i32, dy: i32| {
                    let q = c0 + Vec2::new(dx as f32, dy as f32);
                    let r = t.at(q);
                    w.at(cx + dx, cy + dy, RenPar { big: r.big.max(0.0), small: r.small.max(0.0), ..r })
                };
                let (h00, h20, h02, h22) = (h(0, 0), h(2, 0), h(0, 2), h(2, 2));
                // Split cells along the lattice diagonal so the fan follows the wave triangles.
                let diag = Vec2::new(1.0, 1.0).normalize();
                let d = diag.dot(c0) + 2.0 * std::f32::consts::FRAC_1_SQRT_2;
                for part in [clip(&cell, diag, d), clip(&cell, -diag, -d)] {
                    let pv: Vec<_> = part
                        .iter()
                        .map(|&q| {
                            let f = (q - c0) / 2.0;
                            let (wh, cm) = if f.x + f.y <= 1.0 + 1e-5 {
                                (
                                    h00.0 + f.x * (h20.0 - h00.0) + f.y * (h02.0 - h00.0),
                                    h00.1 + f.x * (h20.1 - h00.1) + f.y * (h02.1 - h00.1),
                                )
                            } else {
                                (
                                    h22.0 + (1.0 - f.x) * (h02.0 - h22.0) + (1.0 - f.y) * (h20.0 - h22.0),
                                    h22.1 + (1.0 - f.x) * (h02.1 - h22.1) + (1.0 - f.y) * (h20.1 - h22.1),
                                )
                            };
                            (q, t.at(q).z + wh, cm)
                        })
                        .collect();
                    g.fan(&pv, origin);
                }
            }
            cy += 2;
        }
        cx += 2;
    }
}

fn rp(v: &sa_physics::water::WaterVertex) -> RenPar {
    RenPar { z: v.z, big: v.big_waves, small: v.small_waves }
}

fn quad_tris(x1: f32, x2: f32, y1: f32, y2: f32, r: [RenPar; 4]) -> [Tri; 2] {
    // (minX,minY), (maxX,minY), (minX,maxY), (maxX,maxY): triangles (0,1,2), (3,2,1).
    let p = [Vec2::new(x1, y1), Vec2::new(x2, y1), Vec2::new(x1, y2), Vec2::new(x2, y2)];
    [Tri { p: [p[0], p[1], p[2]], rp: [r[0], r[1], r[2]] }, Tri { p: [p[3], p[2], p[1]], rp: [r[3], r[2], r[1]] }]
}

fn draw(
    time: Res<Time>,
    sa: Res<SaPhys>,
    wr: Option<ResMut<WaterR>>,
    camera: Single<&GlobalTransform, With<Camera3d>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tfs: Query<&mut Transform>,
) {
    let Some(mut wr) = wr else { return };
    let Some(water) = sa.world.water.clone() else { return };
    let origin = camera.translation();
    let cg = b2g(origin);
    let cam = Vec2::new(cg[0], cg[1]);
    let ts = time.delta_secs() * 50.0;
    let now = sa.world.now_ms;
    let wavy = sa.world.wavyness();

    // FindNearestWaterAndItsFlow every 32 frames; the flow follows at ts·0.001 per axis.
    wr.frame = wr.frame.wrapping_add(1);
    if wr.frame & 31 == 29 {
        wr.target = if cam.x.abs() > 3000.0 || cam.y.abs() > 3000.0 {
            Vec2::ZERO
        } else {
            let mut best = (1.0e7f32, Vec2::ZERO);
            for q in &water.quads {
                for &vi in &q.v {
                    let v = &water.vertices[vi as usize];
                    let d = Vec2::new(v.x as f32, v.y as f32).distance(cam);
                    if d < best.0 {
                        best = (d, Vec2::new(v.flow[0] as f32, v.flow[1] as f32) / 64.0);
                    }
                }
            }
            best.1
        };
    }
    let step = ts * 0.001;
    let (f, t) = (wr.flow, wr.target);
    wr.flow = f + (t - f).clamp(Vec2::splat(-step), Vec2::splat(step));

    // §4.4: scroll and colours.
    let (dx, dy) = (ts * wr.flow.x * 0.04, ts * wr.flow.y * 0.04);
    let s = &mut wr.scroll;
    s[0] += dx * 0.08;
    s[1] += dy * 0.08;
    s[2] += dx * 0.04;
    s[3] += dy * 0.04;
    for v in s.iter_mut() {
        if *v > 1.0 {
            *v -= 1.0;
        }
    }
    let a1 = (now & 0xFFF) as f32 * 0.001_533_980_8;
    let a2 = (now & 0x1FFF) as f32 * 0.000_766_990_4;
    let off0 = Vec2::new(a1.sin() * wavy * 0.08 + s[0], a1.cos() * wavy * 0.08 + s[1]);
    let off1 = Vec2::new(s[2], a2.cos() * 0.024 + s[3]);
    let Some(tc) = sa.world.timecycle.as_ref() else { return };
    let wc = tc.current.water;
    let far = tc.current.far_clip.max(300.0);
    let a = (wc[3] * 0.5) as i32;
    let a0 = ((a * 256) / (256 - a).max(1)).min(255);

    // SetCameraRange: the detail box.
    let bmin = Vec2::new(((cam.x - 48.0) / 2.0).floor() * 2.0, ((cam.y - 48.0) / 2.0).floor() * 2.0);
    let bmax = Vec2::new(((cam.x + 48.0) / 2.0).ceil() * 2.0, ((cam.y + 48.0) / 2.0).ceil() * 2.0);
    let w = Waves { cam, wavy, now };
    let mut g = Geo::default();
    let rough = |x1: f32, x2: f32, y1: f32, y2: f32| {
        // Rectangle vs. the far-clip circle around the camera (the ScanThroughBlocks frustum).
        let n = Vec2::new(cam.x.clamp(x1, x2), cam.y.clamp(y1, y2));
        n.distance(cam) < far
    };
    for q in &water.quads {
        let v = q.v.map(|i| water.vertices[i as usize]);
        if q.flags & wflag::INVISIBLE != 0 || v[0].z > 950.0 {
            continue;
        }
        let (x1, x2, y1, y2) = (v[0].x as f32, v[1].x as f32, v[0].y as f32, v[2].y as f32);
        if !rough(x1.min(x2), x1.max(x2), y1.min(y2), y1.max(y2)) {
            continue;
        }
        for t in quad_tris(x1, x2, y1, y2, v.map(|v| rp(&v))) {
            emit(&t, bmin, bmax, &w, &mut g, origin);
        }
    }
    for tr in &water.tris {
        let v = tr.v.map(|i| water.vertices[i as usize]);
        if tr.flags & wflag::INVISIBLE != 0 || v[0].z > 950.0 {
            continue;
        }
        let p = v.map(|v| Vec2::new(v.x as f32, v.y as f32));
        let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
        if !rough(lo.x, hi.x, lo.y, hi.y) {
            continue;
        }
        emit(&Tri { p, rp: v.map(|v| rp(&v)) }, bmin, bmax, &w, &mut g, origin);
    }

    // Outside the grid: 500-unit sea blocks and the sea bed.
    let mut bed = Geo::default();
    let mut bed_uv = Vec::new();
    let mut bed_seg = |bx: i32, by: i32, fx0: f32, fx1: f32, fy0: f32, fy1: f32| {
        let mut c = |fx: f32, fy: f32| {
            let x = (bx as f32 + fx) * 500.0 - 3000.0;
            let y = (by as f32 + fy) * 500.0 - 3000.0;
            bed_uv.push([fx * 8.0, fy * 8.0]);
            (Vec2::new(x, y), -70.0, 1.0)
        };
        let quad = [c(fx0, fy0), c(fx1, fy0), c(fx1, fy1), c(fx0, fy1)];
        bed.fan(&quad, origin);
    };
    let r = (far / 500.0).ceil() as i32 + 1;
    let (gcx, gcy) = ((cam.x / 500.0 + 6.0).floor() as i32, (cam.y / 500.0 + 6.0).floor() as i32);
    for bx in gcx - r..=gcx + r {
        for by in gcy - r..=gcy + r {
            let (x1, y1) = (bx as f32 * 500.0 - 3000.0, by as f32 * 500.0 - 3000.0);
            if !rough(x1, x1 + 500.0, y1, y1 + 500.0) {
                continue;
            }
            let inside = (0..12).contains(&bx) && (0..12).contains(&by);
            if !inside {
                bed_seg(bx, by, 0.0, 1.0, 0.0, 1.0);
                let sea = RenPar { z: 0.0, big: 1.0, small: 0.0 };
                for t in quad_tris(x1, x1 + 500.0, y1, y1 + 500.0, [sea; 4]) {
                    emit(&t, bmin, bmax, &w, &mut g, origin);
                }
                continue;
            }
            match bx {
                0 => bed_seg(bx, by, 0.0, 0.04, 0.0, 1.0),
                11 => bed_seg(bx, by, 0.96, 1.0, 0.0, 1.0),
                _ => {}
            }
            match by {
                0 => bed_seg(bx, by, 0.0, 1.0, 0.0, 0.04),
                11 => bed_seg(bx, by, 0.0, 1.0, 0.96, 1.0),
                _ => {}
            }
        }
    }

    for (e, _) in wr.layers.iter().chain([&wr.seabed]) {
        if let Ok(mut tf) = tfs.get_mut(*e) {
            tf.translation = origin;
        }
    }
    let rgb = [wc[0], wc[1], wc[2]];
    for (li, (_, h)) in wr.layers.iter().enumerate() {
        let Some(mut m) = meshes.get_mut(h) else { continue };
        if g.idx.is_empty() {
            *m = empty_mesh();
            continue;
        }
        let (scale, off, alpha) = if li == 0 { (0.08, off0, a0) } else { (0.04, off1, a) };
        let uv: Vec<[f32; 2]> = g.xy.iter().map(|q| (*q * scale + off).to_array()).collect();
        let col: Vec<[f32; 4]> = g
            .cm
            .iter()
            .map(|&cm| {
                let c = |k: usize| (rgb[k] * cm).clamp(0.0, 255.0) as u8;
                let l = Color::srgba_u8(c(0), c(1), c(2), alpha.clamp(0, 255) as u8).to_linear();
                [l.red, l.green, l.blue, l.alpha]
            })
            .collect();
        let n = g.pos.len();
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, g.pos.clone());
        m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
        m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
        m.insert_indices(Indices::U32(g.idx.clone()));
    }
    if let Some(mut m) = meshes.get_mut(&wr.seabed.1) {
        if bed.idx.is_empty() {
            *m = empty_mesh();
        } else {
            let n = bed.pos.len();
            let l = Color::srgb_u8(80, 80, 80).to_linear();
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, bed.pos);
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
            m.insert_attribute(Mesh::ATTRIBUTE_UV_0, bed_uv);
            m.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[l.red, l.green, l.blue, 1.0]; n]);
            m.insert_indices(Indices::U32(bed.idx));
        }
    }
}
