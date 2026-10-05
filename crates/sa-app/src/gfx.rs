//! Graphics modes. **Classic** draws SA as the original renderer did (no tonemapping, prelit
//! map, ambient-only peds and cars, SA's own blob shadows). **Enhanced** (the default) keeps
//! SA's timecycle colours and art but adds a modern pipeline on top:
//! - HDR with filmic tonemapping, a little bloom, SMAA, SSAO, colour grading;
//! - the sun as a real light (direction from `CTimeCycle::m_VectorToSun`, colour from the
//!   timecycle's sun core) with cascaded shadow maps, casting from the map, cars and peds and
//!   received by the prelit map (world_material.wgsl darkens the baked light in shadow);
//! - a sky environment map so car paint and glass reflect the timecycle sky.
//!
//! `SA_GFX=classic` starts in classic; F9 toggles.

use bevy::{
    anti_alias::smaa::{Smaa, SmaaPreset},
    camera::Hdr,
    core_pipeline::{
        prepass::{DepthPrepass, NormalPrepass},
        tonemapping::Tonemapping,
    },
    light::CascadeShadowConfigBuilder,
    pbr::ScreenSpaceAmbientOcclusion,
    post_process::bloom::Bloom,
    prelude::*,
    render::view::{ColorGrading, ColorGradingGlobal},
};

pub struct GfxPlugin;

impl Plugin for GfxPlugin {
    fn build(&self, app: &mut App) {
        let enhanced = std::env::var("SA_GFX").map_or(true, |v| v != "classic");
        app.insert_resource(Gfx { enhanced })
            .add_systems(Startup, init_sky_cubemap)
            .add_systems(Update, (toggle, apply_camera, update_sky_cubemap, apply_hdr_boost).chain())
            .add_systems(Update, count_draws)
            .add_systems(PostUpdate, sun_shafts.after(bevy::transform::TransformSystems::Propagate));
    }
}

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub struct Gfx {
    pub enhanced: bool,
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut gfx: ResMut<Gfx>) {
    if keys.just_pressed(KeyCode::F9) {
        gfx.enhanced = !gfx.enhanced;
        info!("graphics: {}", if gfx.enhanced { "enhanced" } else { "classic" });
    }
}

/// The camera's post-processing stack for the mode.
fn apply_camera(
    mut commands: Commands,
    gfx: Res<Gfx>,
    cams: Query<Entity, (With<Camera3d>, Without<GfxApplied>)>,
    applied: Query<(Entity, &GfxApplied), With<Camera3d>>,
    mut lights: Query<&mut DirectionalLight>,
    mut overlays: Query<&mut Camera, (With<Camera2d>, Without<Camera3d>)>,
    sky: Option<Res<SkyCubemap>>,
) {
    let want = gfx.enhanced;
    let mut targets: Vec<Entity> = cams.iter().collect();
    targets.extend(applied.iter().filter(|(_, a)| a.0 != want).map(|(e, _)| e));
    let any_target = !targets.is_empty();
    for e in targets {
        let mut c = commands.entity(e);
        if want {
            let skip = std::env::var("SA_GFXSKIP").unwrap_or_default();
            if skip.contains("all") {
                c.insert(GfxApplied(true));
                continue;
            }
            c.insert((
                Hdr,
                // SA_TONEMAP=agx|tony|aces|filmic (debug; TonyMcMapface by default).
                match std::env::var("SA_TONEMAP").as_deref() {
                    Ok("agx") => Tonemapping::AgX,
                    Ok("aces") => Tonemapping::AcesFitted,
                    Ok("filmic") => Tonemapping::BlenderFilmic,
                    _ => Tonemapping::TonyMcMapface,
                },
                Bloom { intensity: 0.08, ..Bloom::NATURAL },
                Msaa::Off,
                Smaa { preset: SmaaPreset::High },
                DepthPrepass,
                NormalPrepass,
                ScreenSpaceAmbientOcclusion::default(),
                ColorGrading {
                    global: ColorGradingGlobal { exposure: std::env::var("SA_EXPOSURE").ok().and_then(|v| v.parse().ok()).unwrap_or(0.2), post_saturation: 1.1, ..default() },
                    ..default()
                },
                GfxApplied(true),
            ));
            // Volumetric light: the sun's shafts through the haze (FogVolume follows the camera).
            // Opt-in (SA_VOL=1): its in-scattering tints the scene toward the sky colour.
            if std::env::var("SA_VOL").is_ok() && !skip.contains("ssao") {
                c.insert(bevy::light::VolumetricFog { ambient_intensity: 0.0, step_count: 48, ..default() });
            }
            if let Some(sky) = sky.as_ref() {
                c.insert(bevy::light::GeneratedEnvironmentMapLight { environment_map: sky.0.clone(), intensity: std::env::var("SA_ENVI").ok().and_then(|v| v.parse().ok()).unwrap_or(50.0), ..default() });
            }
            // SSAO (and the volumetric light) use the depth + normal prepass, cheap since the map
            // material is bindless.
            if skip.contains("ssao") {
                c.remove::<(ScreenSpaceAmbientOcclusion, NormalPrepass, DepthPrepass)>();
            }
            if skip.contains("smaa") {
                c.remove::<Smaa>();
            }
            if skip.contains("bloom") {
                c.remove::<Bloom>();
            }
            if skip.contains("haze") {
                c.remove::<crate::heat_haze::HeatHaze>();
            }
            if skip.contains("cg") {
                c.remove::<ColorGrading>();
            }
            if skip.contains("tm") {
                c.insert(Tonemapping::None);
            }
            if skip.contains("msaa") {
                c.insert(Msaa::Sample4);
            }
            if skip.contains("hdr") {
                c.remove::<Hdr>();
            }
        } else {
            c.remove::<(Hdr, Bloom, Smaa, DepthPrepass, NormalPrepass, ScreenSpaceAmbientOcclusion, ColorGrading)>();
            c.remove::<bevy::light::GeneratedEnvironmentMapLight>();
            c.remove::<bevy::light::VolumetricFog>();
            c.insert((Tonemapping::None, Msaa::Sample4, GfxApplied(false)));
        }
    }
    // The 2D HUD camera: with an HDR 3D camera they no longer share the intermediate texture,
    // so the HUD clears to transparent and blends over the frame.
    if gfx.is_changed() || any_target {
        for mut cam in &mut overlays {
            let hdr = want && !std::env::var("SA_GFXSKIP").is_ok_and(|v| v.contains("hdr") || v.contains("all"));
            if hdr {
                cam.clear_color = ClearColorConfig::Custom(Color::NONE);
                cam.output_mode = bevy::camera::CameraOutputMode::Write {
                    blend_state: Some(bevy::render::render_resource::BlendState::ALPHA_BLENDING),
                    clear_color: ClearColorConfig::None,
                };
            } else {
                cam.clear_color = ClearColorConfig::None;
                cam.output_mode = bevy::camera::CameraOutputMode::Write { blend_state: None, clear_color: ClearColorConfig::None };
            }
        }
    }
    if gfx.is_changed() {
        for mut l in &mut lights {
            l.shadow_maps_enabled = want;
        }
    }
}

#[derive(Component)]
struct GfxApplied(bool);

/// The cascades for the sun: sharp near the player, out to the streaming distance.
pub fn sun_cascades() -> bevy::light::CascadeShadowConfig {
    CascadeShadowConfigBuilder { num_cascades: 2, minimum_distance: 0.3, maximum_distance: 100.0, first_cascade_far_bound: 22.0, overlap_proportion: 0.2 }
        .build()
}

/// The sky environment map source: a small HDR cubemap painted from the timecycle sky
/// (gradient, ground, sun hotspot), filtered on the GPU by `GeneratedEnvironmentMapLight`.
#[derive(Resource)]
pub struct SkyCubemap(pub Handle<Image>);

const CUBE: u32 = 32;

fn init_sky_cubemap(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
    let mut img = Image::new_fill(
        Extent3d { width: CUBE, height: CUBE, depth_or_array_layers: 6 },
        TextureDimension::D2,
        &[0u8; 16],
        TextureFormat::Rgba32Float,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );
    img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    commands.insert_resource(SkyCubemap(images.add(img)));
}

/// Repaint the cubemap from the current timecycle every half second.
fn update_sky_cubemap(
    gfx: Res<Gfx>,
    time: Res<Time>,
    mut next: Local<f32>,
    sa: Res<crate::saphys::SaPhys>,
    sky: Option<Res<SkyCubemap>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !gfx.enhanced || time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 0.5;
    let (Some(sky), Some(tc)) = (sky, sa.world.timecycle.as_ref()) else { return };
    let c = &tc.current;
    let lin = |v: [f32; 3]| Vec3::from(v.map(|x| {
        let x = (x / 255.0).clamp(0.0, 1.0);
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    }));
    let (top, bottom) = (lin(c.sky_top), lin(c.sky_bottom));
    let ground = bottom * 0.25 + Vec3::new(0.06, 0.05, 0.04);
    let dn = sa.world.clock.dn_balance();
    let to_sun = crate::world::g2b(tc.vector_to_sun.to_array()).normalize_or(Vec3::Y);
    let sun_col = lin(c.sun_core) * (1.0 - dn) * 30.0;
    let Some(mut img) = images.get_mut(&sky.0) else { return };
    let n = CUBE as usize;
    let mut data = Vec::with_capacity(n * n * 6 * 16);
    for face in 0..6 {
        for y in 0..n {
            for x in 0..n {
                let u = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                // Cubemap face directions (+X, -X, +Y, -Y, +Z, -Z).
                let d = match face {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                }
                .normalize();
                let mut col = if d.y >= 0.0 { bottom.lerp(top, d.y.powf(0.6)) } else { ground.lerp(bottom, (1.0 + d.y * 4.0).max(0.0)) };
                let s = d.dot(to_sun).max(0.0);
                // Mostly neutral: the env light also lights diffusely, and a saturated blue sky
                // would tint the paint (SA's ambient already carries the sky colour).
                let l = col.dot(Vec3::new(0.2126, 0.7152, 0.0722));
                col = Vec3::splat(l).lerp(col, 0.3);
                col += sun_col * s.powf(400.0) + sun_col * 0.01 * s.powf(8.0);
                for k in [col.x, col.y, col.z, 1.0] {
                    data.extend_from_slice(&k.to_le_bytes());
                }
            }
        }
    }
    img.data = Some(data);
}

/// Additive glows (coronas, light beams) pushed above 1.0 in enhanced mode so they bloom.
#[derive(Component)]
pub struct HdrBoost(pub f32);

fn apply_hdr_boost(
    gfx: Res<Gfx>,
    added: Query<(), Added<HdrBoost>>,
    q: Query<(&HdrBoost, &MeshMaterial3d<StandardMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !gfx.is_changed() && added.is_empty() {
        return;
    }
    for (b, m) in &q {
        if let Some(mut mat) = materials.get_mut(&m.0) {
            let k = if gfx.enhanced { b.0 } else { 1.0 };
            mat.base_color = Color::LinearRgba(LinearRgba::new(k, k, k, 1.0));
        }
    }
}

/// SA_FPSLOG: also log how many meshes are drawn and with how many distinct materials.
fn count_draws(
    time: Res<Time>,
    mut next: Local<f32>,
    world: Query<(&MeshMaterial3d<crate::world_material::WorldMaterial>, &ViewVisibility)>,
    std: Query<(&MeshMaterial3d<StandardMaterial>, &ViewVisibility)>,
) {
    if std::env::var("SA_FPSLOG").is_err() || time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    let vis_w: Vec<_> = world.iter().filter(|(_, v)| v.get()).map(|(m, _)| m.0.id()).collect();
    let mats_w: std::collections::HashSet<_> = vis_w.iter().collect();
    let vis_s = std.iter().filter(|(_, v)| v.get()).count();
    info!("draws: map {} visible of {} ({} materials), standard {} visible of {}", vis_w.len(), world.iter().count(), mats_w.len(), vis_s, std.iter().count());
}

/// Enhanced: the post pass keeps HDR values above 1.0 (SA's colour filter), and the haze volume
/// for the sun's volumetric shafts follows the camera, its density and colour from the
/// timecycle (stronger with a low sun, none at night).
fn sun_shafts(
    mut commands: Commands,
    gfx: Res<Gfx>,
    sa: Res<crate::saphys::SaPhys>,
    mut cams: Query<(&GlobalTransform, &mut crate::heat_haze::HeatHaze, Option<&mut bevy::light::VolumetricFog>), With<Camera3d>>,
    mut vol: Query<(Entity, &mut Transform, &mut bevy::light::FogVolume)>,
    mut sun: Query<(Entity, Has<bevy::light::VolumetricLight>), With<DirectionalLight>>,
) {
    let Some(tc) = sa.world.timecycle.as_ref() else { return };
    let dn = sa.world.clock.dn_balance();
    let to_sun = crate::world::g2b(tc.vector_to_sun.to_array()).normalize_or(Vec3::Y);
    let Ok((gt, mut hh, vf)) = cams.single_mut() else { return };
    hh.rays = Vec4::new(0.0, 0.0, 0.0, if gfx.enhanced { 1.0 } else { 0.0 });
    let vol_on = gfx.enhanced && std::env::var("SA_VOL").is_ok();
    for (e, has) in &mut sun {
        if vol_on && !has {
            commands.entity(e).insert(bevy::light::VolumetricLight);
        } else if !vol_on && has {
            commands.entity(e).remove::<bevy::light::VolumetricLight>();
        }
    }
    let low = 1.0 - (to_sun.y / 0.7).clamp(0.0, 1.0);
    let density = if vol_on { (1.0 - dn) * (0.0008 + 0.0025 * low) * std::env::var("SA_VOLK").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0) } else { 0.0 };
    let fog = Vec3::from(tc.current.sky_bottom) / 255.0;
    let col = Color::srgb(fog.x, fog.y, fog.z);
    // The haze scatters the sky's light too (no darkening).
    if let Some(mut vf) = vf {
        vf.ambient_color = col;
        vf.ambient_intensity = 1.0;
    }
    let tf = Transform::from_translation(gt.translation()).with_scale(Vec3::new(400.0, 160.0, 400.0));
    match vol.single_mut() {
        Ok((_, mut t, mut v)) => {
            *t = tf;
            v.density_factor = density;
            v.fog_color = col;
        }
        Err(_) => {
            commands.spawn((
                tf,
                bevy::light::FogVolume {
                    density_factor: density,
                    absorption: 0.0,
                    scattering: 1.0,
                    scattering_asymmetry: 0.8,
                    fog_color: col,
                    ..default()
                },
            ));
        }
    }
}
