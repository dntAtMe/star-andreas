//! Lights (lights.md, vehicle_lights.md):
//! - `CEntity::ProcessLightsForEntity` (0x6FC7A0) for map objects with 2dfx lights: show
//!   modes, blink flags, coronas, point lights and the light pool on the ground;
//! - `CTrafficLights::DisplayActualLight` (0x49DAB0);
//! - `CPointLights` drawn as Bevy point / spot lights (they light peds and cars; map
//!   geometry is prelit, as in SA);
//! - `DoHeadLightBeam` (0x6E0E20) wedges for lit headlights.
//!
//! Not ported: LA riots, CBrightLights (the traffic-light lens quads), the pedestrian
//! walk sign, RenderFogEffect (fog glow sprites), sun-glare 2dfx effects, the direction
//! test of CHECK_DIRECTION lights.

use std::sync::Arc;

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    transform::TransformSystems,
};
use sa_formats::dff::{Light2d, light_flags as lf};
use sa_physics::{
    automobile::Automobile,
    coronas::{CoronaArgs, CoronaTex},
    effects::MAX_POINT_LIGHTS,
    physical::Matrix as GMatrix,
    shadows::ShadowTex,
};

use crate::{
    saphys::{SaPhys, SaPhysExt, SaStep, SaSync},
    vehicle::Vehicle,
    world::{b2g, g2b},
};

pub struct LightsPlugin;

impl Plugin for LightsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init)
            .add_systems(PostUpdate, (world_lights, draw_point_lights, draw_beams).chain().after(SaSync).after(TransformSystems::Propagate))
            .add_systems(Update, lamp_materials.after(SaStep));
    }
}

/// 2dfx lights of a streamed map instance.
#[derive(Component)]
pub struct EntityLights {
    pub lights: Arc<Vec<Light2d>>,
    pub model: Arc<str>,
    /// `m_nRandomSeed`.
    pub seed: u16,
    /// Stable key for corona / shadow ids.
    pub key: u64,
}

/// Per-effect random table (0x8D5028).
const SEED_TABLE: [u16; 8] = [0, 0x699A, 0xAB55, 0xCC66, 0xFBEF, 0x9625, 0x55AA, 0x98CD];
const TRAFFIC_LIGHTS: [&str; 9] = [
    "trafficlight1",
    "mtraffic4",
    "mtraffic1",
    "vgsstriptlights1",
    "mtraffic2",
    "cj_traffic_light3",
    "cj_traffic_light4",
    "cj_traffic_light5",
    "gay_traffic_light",
];
/// Traffic-light colours by state (0 green, 1 amber, 2 red, 3 off).
const TL_R: [f32; 4] = [0.0, 255.0, 255.0, 0.0];
const TL_G: [f32; 4] = [255.0, 128.0, 0.0, 0.0];

#[derive(Resource)]
struct LightPools {
    points: Vec<Entity>,
    spots: Vec<Entity>,
    beams: (Entity, Handle<Mesh>),
}

fn empty_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn init(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let points = (0..MAX_POINT_LIGHTS)
        .map(|_| commands.spawn((PointLight { shadow_maps_enabled: false, ..default() }, Transform::default(), Visibility::Hidden)).id())
        .collect();
    let spots = (0..MAX_POINT_LIGHTS)
        .map(|_| {
            commands
                .spawn((
                    SpotLight { shadow_maps_enabled: false, outer_angle: std::f32::consts::FRAC_PI_3, inner_angle: 0.0, ..default() },
                    Transform::default(),
                    Visibility::Hidden,
                ))
                .id()
        })
        .collect();
    let h = meshes.add(empty_mesh());
    let mat = materials.add(StandardMaterial {
        unlit: true,
        alpha_mode: AlphaMode::Premultiplied,
        cull_mode: None,
        double_sided: true,
        ..default()
    });
    let e = commands.spawn((Mesh3d(h.clone()), MeshMaterial3d(mat), Transform::default(), NoFrustumCulling)).id();
    commands.insert_resource(LightPools { points, spots, beams: (e, h) });
}

fn gta_matrix(gt: &GlobalTransform) -> GMatrix {
    let (_, rot, t) = gt.to_scale_rotation_translation();
    let g = |v: Vec3| Vec3::from(b2g(v));
    GMatrix { right: g(rot * Vec3::X), fwd: g(rot * Vec3::NEG_Z), up: g(rot * Vec3::Y), pos: g(t) }
}

/// `ProcessLightsForEntity` + traffic lights, once per game frame.
fn world_lights(mut sa: ResMut<SaPhys>, q: Query<(&EntityLights, &GlobalTransform)>, mut last: Local<u32>) {
    let w = &mut sa.world;
    if w.frame == *last {
        return;
    }
    *last = w.frame;
    let Some(tc) = w.timecycle.as_ref() else { return };
    let (sb, ss, log) = (tc.current.sprite_brightness, tc.current.sprite_size, tc.current.light_on_ground);
    let dn = w.clock.dn_balance();
    let now = w.now_ms;
    let frame = w.frame;
    let (wet, rain, tlb) = (w.weather.wet_roads, w.weather.rain, w.weather.traffic_lights_brightness);
    let cam = w.camera_pos;
    let cam_fwd = w.camera_fwd;
    for (el, gt) in &q {
        let m = gta_matrix(gt);
        if m.up.z < 0.96 {
            continue; // knocked over
        }
        let base = (1u64 << 60) | (el.key << 8);
        if TRAFFIC_LIGHTS.iter().any(|n| n.eq_ignore_ascii_case(&el.model)) {
            traffic_light(w, el, &m, base, now, sb, ss, log, tlb, cam_fwd);
            continue;
        }
        for (i, fx) in el.lights.iter().enumerate() {
            let seed = SEED_TABLE[i & 7] ^ el.seed;
            let mut f = 1.0f32;
            let pos = m.transform(Vec3::from(fx.pos));
            let fl = fx.flags;
            let (mut on, mut off_corona, mut keep) = (false, false, false);
            let mut alpha_f = 1.0;
            let mut by_time = false;
            if fl & lf::AT_DAY != 0 {
                if fl & lf::AT_NIGHT != 0 {
                    by_time = true;
                } else if dn < 1.0 {
                    by_time = true;
                    alpha_f = 1.0 - dn;
                }
            } else if fl & lf::AT_NIGHT != 0 && dn > 0.0 {
                by_time = true;
                alpha_f = dn;
            }
            let t = now;
            let s = seed as u32;
            if (fx.show_mode == 2 && wet > 0.5) || by_time {
                match fx.show_mode {
                    0 => on = true,
                    1 | 2 => {
                        on = ((s ^ t) & 0x60) != 0 || (((t >> 11) ^ s) & 3) != 0;
                        if ((s ^ t) & 0x60) == 0 {
                            off_corona = true;
                        }
                    }
                    3 => {
                        on = ((((i as u32) << 7).wrapping_add(t)) & 0x200) != 0;
                        keep = !on;
                    }
                    4 => {
                        on = ((((i as u32) << 8).wrapping_add(t)) & 0x400) != 0;
                        keep = !on;
                    }
                    5 => {
                        on = ((((i as u32) << 9).wrapping_add(t)) & 0x800) != 0;
                        keep = !on;
                    }
                    6 => {
                        if (s & 0xFF) > 16 {
                            on = true;
                        } else {
                            on = (((s << 3) ^ t) & 0x60) != 0 || (((t >> 11) ^ s) & 3) != 0;
                            if (((s << 3) ^ t) & 0x60) == 0 {
                                off_corona = true;
                            }
                        }
                    }
                    10 => {
                        if rain > 0.0001 {
                            on = true;
                            f = rain;
                        }
                    }
                    11..=13 => {
                        let tt = (pos.y * 10.0) as i32
                            + ((pos.x * 20.0) as i32).wrapping_add((t as i32).wrapping_add((fx.show_mode as i32 - 11) * 3333));
                        let r = tt % 10000;
                        let q = (r * 9) / 10000;
                        let fr = (r - q * 1111) as f32 * 0.000_900_09;
                        match q {
                            0 => {
                                on = true;
                                f = fr;
                            }
                            1 | 2 => on = true,
                            3 => {
                                on = true;
                                f = 1.0 - fr;
                            }
                            _ => {}
                        }
                    }
                    _ => {} // 7 traffic, 8 train crossing (inactive), 9 stub
                }
            }
            let id = base + i as u64;
            let tex = CoronaTex::from_type(0).filter(|_| fx.corona_tex.eq_ignore_ascii_case("coronastar")).or(Some(CoronaTex::Star));
            let intensity;
            if on {
                if fl & lf::BLINKING1 != 0 {
                    f *= 1.0 - (w.rng.next() & 31) as f32 * 0.012;
                }
                if fl & lf::BLINKING2 != 0 && ((s.wrapping_add(frame)) & 3) != 0 {
                    f = 0.0;
                }
                if fl & lf::BLINKING3 != 0 {
                    let c = (frame.wrapping_add(s)) & 63;
                    f = if c == 0 { f } else if c == 1 { f * 0.5 } else { 0.0 };
                }
                intensity = sb * f * 0.1;
                let c = |x: u8| (x as f32 * intensity) as i32 as u8;
                w.register_corona(CoronaArgs {
                    id,
                    rgb: [c(fx.color[0]), c(fx.color[1]), c(fx.color[2])],
                    alpha: (alpha_f * 255.0) as i32 as u8,
                    pos,
                    radius: fx.corona_size,
                    far_clip: fx.corona_far_clip,
                    tex,
                    flare: fx.flare,
                    reflection: fx.reflection,
                    check_obstacles: fl & lf::CHECK_OBSTACLES != 0,
                    long_distance: fl & lf::ONLY_LONG_DISTANCE != 0,
                    near_clip: 0.8,
                    only_from_below: fl & lf::ONLY_FROM_BELOW != 0,
                    ..Default::default()
                });
            } else {
                intensity = sb * f * 0.1;
                if off_corona {
                    w.register_corona(CoronaArgs {
                        id,
                        rgb: [0, 0, 0],
                        pos,
                        radius: fx.corona_size,
                        far_clip: fx.corona_far_clip,
                        tex,
                        flare: fx.flare,
                        reflection: fx.reflection,
                        check_obstacles: fl & lf::CHECK_OBSTACLES != 0,
                        long_distance: fl & lf::ONLY_LONG_DISTANCE != 0,
                        ..Default::default()
                    });
                } else if keep {
                    w.update_corona_coors(id, pos, fx.corona_far_clip);
                }
            }
            // Point light, fog-only lights.
            let rgb = Vec3::new(fx.color[0] as f32, fx.color[1] as f32, fx.color[2] as f32);
            let mut fog_only = true;
            if fx.range != 0.0 && on {
                if rgb == Vec3::ZERO {
                    w.effects.add_point_light(cam, 2, pos, Vec3::ZERO, fx.range, Vec3::ZERO, 0, true);
                } else {
                    let k = alpha_f * intensity / 256.0;
                    w.effects.add_point_light(cam, 0, pos, Vec3::ZERO, fx.range, rgb * k, ((fl >> 1) & 3) as u8, true);
                    fog_only = false;
                }
            }
            if fog_only {
                if fl & lf::FOG_TYPE2 != 0 {
                    w.effects.add_point_light(cam, 3, pos, Vec3::ZERO, 0.0, rgb / 256.0, 2, true);
                } else if fl & lf::FOG_TYPE != 0 && on && fx.range == 0.0 {
                    w.effects.add_point_light(cam, 4, pos, Vec3::ZERO, 0.0, rgb / 256.0, 1, true);
                }
            }
            // The light pool on the ground.
            if fx.shadow_size != 0.0 {
                let zd = if fx.shadow_z_dist != 0 { fx.shadow_z_dist as f32 } else { 15.0 };
                let st = ShadowTex::from_name(&fx.shadow_tex);
                let sz = fx.shadow_size;
                if on {
                    let k = fx.shadow_mult as f32 * intensity / 256.0;
                    let c = |x: u8| (x as f32 * k) as i32 as u8;
                    let rgb = [c(fx.color[0]), c(fx.color[1]), c(fx.color[2])];
                    w.store_static_shadow(id, 2, st, pos, Vec2::new(sz, 0.0), Vec2::new(0.0, -sz), 128, rgb, zd, 1.0, 40.0, false, 0.0);
                } else if off_corona {
                    w.store_static_shadow(id, 2, st, pos, Vec2::new(sz, 0.0), Vec2::new(0.0, -sz), 0, [0; 3], zd, 1.0, 40.0, false, 0.0);
                }
            }
        }
    }
}

/// `CTrafficLights::DisplayActualLight` (0x49DAB0).
#[allow(clippy::too_many_arguments)]
fn traffic_light(
    w: &mut sa_physics::world::World,
    el: &EntityLights,
    m: &GMatrix,
    base: u64,
    now: u32,
    sb: f32,
    ss: f32,
    log: f32,
    tlb: f32,
    cam_fwd: Vec3,
) {
    // FindTrafficLightType from the light's heading (RW "up" = GTA forward).
    let mut ang = m.fwd.y.atan2(m.fwd.x).to_degrees();
    if ang < 0.0 {
        ang += 360.0;
    }
    let ty1 = (60.0 < ang && ang < 150.0) || (240.0 < ang && ang < 330.0);
    let t = (now >> 1) & 0x3FFF;
    let state = if ty1 {
        if t < 5000 { 0 } else if t < 6000 { 1 } else { 2 }
    } else if t < 6000 {
        2
    } else if t < 11000 {
        0
    } else if t < 12000 {
        1
    } else {
        2
    };
    let facing = cam_fwd.dot(m.fwd) > 0.0;
    let mut sum = Vec3::ZERO;
    for (i, fx) in el.lights.iter().enumerate() {
        let p = m.transform(Vec3::from(fx.pos));
        sum += p;
        let lamp = if fx.color[0] <= 200 { 0 } else if fx.color[1] > 100 { 1 } else { 2 };
        if (fx.pos[1] > 0.0) != facing && lamp == state {
            let k = sb * 0.07;
            w.register_corona(CoronaArgs {
                id: base + i as u64,
                rgb: [(TL_R[state] * k) as i32 as u8, (TL_G[state] * k) as i32 as u8, 0],
                pos: p,
                radius: ss * 0.175,
                far_clip: 50.0,
                reflection: true,
                ..Default::default()
            });
        }
    }
    let n = el.lights.len().max(1) as f32;
    let avg = sum / n;
    let cam = w.camera_pos;
    if tlb > 0.5 {
        let c = |x: f32| x.max(50.0) / 637.5;
        w.effects.add_point_light(cam, 0, avg, Vec3::ZERO, 14.0, Vec3::new(c(TL_R[state]), c(TL_G[state]), c(0.0)), 1, true);
    }
    if tlb > 0.05 {
        let c = log * tlb * 0.0125;
        let rgb = [(TL_R[state] * c) as i32 as u8, (TL_G[state] * c) as i32 as u8, 0];
        w.store_static_shadow(base | 0xFF, 2, ShadowTex::Exp, avg, Vec2::new(8.0, 0.0), Vec2::new(0.0, -8.0), 128, rgb, 12.0, 1.0, 40.0, false, 0.0);
    }
}

/// CPointLights → Bevy lights. Point lights (type 0) and spots (type 1) light peds and
/// cars; darkness and fog-only lights are not drawn. SA turns each light into a per-object
/// directional light at full strength in the inner half of its range; the Bevy intensity
/// is set so a white diffuse surface gets the light's colour at half range.
fn draw_point_lights(
    sa: Res<SaPhys>,
    pools: Option<Res<LightPools>>,
    mut points: Query<(&mut PointLight, &mut Transform, &mut Visibility), Without<SpotLight>>,
    mut spots: Query<(&mut SpotLight, &mut Transform, &mut Visibility), Without<PointLight>>,
) {
    let Some(pools) = pools else { return };
    let (mut pi, mut si) = (0, 0);
    for l in &sa.world.effects.lights {
        let mx = l.color.max_element();
        if mx <= 0.0 || l.radius <= 0.0 {
            continue;
        }
        let half = l.radius * 0.5;
        let lumens = mx * 4.0 * std::f32::consts::PI * std::f32::consts::PI * 980.0 * half * half;
        let col = Color::linear_rgb(l.color.x / mx, l.color.y / mx, l.color.z / mx);
        match l.ty {
            0 if pi < pools.points.len() => {
                if let Ok((mut pl, mut tf, mut v)) = points.get_mut(pools.points[pi]) {
                    pl.color = col;
                    pl.intensity = lumens;
                    pl.range = l.radius;
                    tf.translation = g2b(l.pos.to_array());
                    *v = Visibility::Visible;
                }
                pi += 1;
            }
            1 if si < pools.spots.len() => {
                if let Ok((mut sl, mut tf, mut v)) = spots.get_mut(pools.spots[si]) {
                    sl.color = col;
                    sl.intensity = lumens;
                    sl.range = l.radius;
                    let dir = g2b(l.dir.to_array()).normalize_or(Vec3::NEG_Z);
                    // SA lights each object from the light's direction and never lights the
                    // car a headlight belongs to: start the Bevy spot ahead of its source.
                    let p = g2b(l.pos.to_array()) + dir * 2.5;
                    *tf = Transform::from_translation(p).looking_to(dir, Vec3::Y);
                    *v = Visibility::Visible;
                }
                si += 1;
            }
            _ => {}
        }
    }
    for &e in &pools.points[pi..] {
        if let Ok((_, _, mut v)) = points.get_mut(e) {
            *v = Visibility::Hidden;
        }
    }
    for &e in &pools.spots[si..] {
        if let Ok((_, _, mut v)) = spots.get_mut(e) {
            *v = Visibility::Hidden;
        }
    }
}

/// `DoHeadLightBeam` (0x6E0E20): a camera-facing additive wedge per lit headlight.
fn draw_beams(
    sa: Res<SaPhys>,
    pools: Option<Res<LightPools>>,
    cars: Query<&Vehicle>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tfs: Query<&mut Transform, With<Mesh3d>>,
    cam: Single<&GlobalTransform, With<Camera3d>>,
) {
    let Some(pools) = pools else { return };
    let cam_b = cam.translation();
    let cam_pos = Vec3::from(b2g(cam_b));
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::<u32>::new());
    for v in &cars {
        let Some(car) = sa.logic::<Automobile>(v.sa) else { continue };
        let Some(body) = sa.world.body(v.sa) else { continue };
        if matches!(car.model, 441 | 432) {
            continue;
        }
        let m = &body.phys.matrix;
        for (bit, right) in [(1u8, true), (2u8, false)] {
            if car.lights.render & bit == 0 {
                continue;
            }
            let d = car.lights.dummies[0];
            let mut p = m.transform(d);
            if !right {
                p -= m.right * (2.0 * d.x);
            }
            let view = (cam_pos - p).normalize_or_zero();
            let dot = view.dot(m.fwd);
            let a = ((1.0 - dot.abs()) * 32.0) as i32 as f32 / 255.0;
            let k = if car.model == 530 { 0.5 } else { 0.15 };
            let dd = (m.fwd - m.up * k).normalize_or_zero();
            let s = dd.cross(view).normalize_or_zero();
            let p = p - m.fwd * 0.1;
            let verts = [
                (p - s * 0.05, a),
                (p + s * 0.05, a),
                (p + dd * 3.0 - s * 0.5, 0.0),
                (p + dd * 3.0 + s * 0.5, 0.0),
                (p + dd * 0.2, a),
            ];
            let b = pos.len() as u32;
            // SRCALPHA/ONE: premultiplied colour, alpha 0 (pure add).
            for (vp, al) in verts {
                pos.push((g2b(vp.to_array()) - cam_b).to_array());
                let c = Color::srgb(al, al, al).to_linear();
                col.push([c.red, c.green, c.blue, 0.0]);
            }
            idx.extend([0, 1, 4, 1, 3, 4, 2, 3, 4, 0, 2, 4].map(|i| b + i));
        }
    }
    let (e, h) = &pools.beams;
    if let Ok(mut tf) = tfs.get_mut(*e) {
        tf.translation = cam_b;
    }
    let Some(mut mesh) = meshes.get_mut(h) else { return };
    if idx.is_empty() {
        *mesh = empty_mesh();
        return;
    }
    let n = pos.len();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    mesh.insert_indices(Indices::U32(idx));
}

/// `SetupLightFlags` / `SetEditableMaterialsCB`: lit lamps show `vehiclelightson128` at
/// full brightness (surface ambient 16, no diffuse), unlit ones `vehiclelights128` lit
/// normally.
fn lamp_materials(sa: Res<SaPhys>, mut cars: Query<&mut Vehicle>, mut materials: ResMut<Assets<StandardMaterial>>) {
    for mut v in &mut cars {
        let Some(car) = sa.logic::<Automobile>(v.sa) else { continue };
        let r = car.lights.render;
        let lit = [r & 2 != 0, r & 1 != 0, r & 8 != 0, r & 4 != 0];
        for lamp in &mut v.lamps {
            let on = lit[lamp.index as usize];
            if lamp.on == on {
                continue;
            }
            lamp.on = on;
            if let Some(mut m) = materials.get_mut(&lamp.material) {
                m.base_color_texture = if on { lamp.tex_on.clone() } else { lamp.tex_off.clone() };
                m.unlit = on;
            }
        }
    }
}
