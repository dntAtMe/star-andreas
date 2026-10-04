//! `CPostEffects::HeatHazeFX` (0x701780, settings 0x701450, type 0): 180 rising tiles
//! that magnify the screen copy x50/47 at alpha `80 * intensity / 255`.
//!
//! When it runs (`CPostEffects::Render` 0x705099): in heat-haze weather
//! (`CWeather` 0xC812DC > 0) full screen at the weather intensity, else masked to the
//! fires' heat-haze sprites (mask not occluded by the world, as the original clears depth).
//! Not ported: the underwater variant (no water yet) and the script switch.

use bevy::{
    asset::embedded_asset,
    camera::primitives::Frustum,
    core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin},
    prelude::*,
    render::{extract_component::ExtractComponent, render_resource::ShaderType},
    shader::ShaderRef,
};

use crate::{
    fx::{Fx, fx_camera_of},
    saphys::SaPhys,
    world::g2b,
};

const TILES: usize = 180;
const MASKS: usize = 64;

pub struct HeatHazePlugin;

impl Plugin for HeatHazePlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "heat_haze.wgsl");
        app.add_plugins(FullscreenMaterialPlugin::<HeatHaze>::default())
            .add_systems(Startup, attach)
            .add_systems(PostUpdate, update.after(crate::fx::FxDrawn));
    }
}

#[derive(Component, ExtractComponent, Clone, Copy, ShaderType)]
pub struct HeatHaze {
    params: Vec4,
    tiles: [Vec4; TILES * 2],
    masks: [Vec4; MASKS * 2],
}

impl Default for HeatHaze {
    fn default() -> Self {
        Self { params: Vec4::ZERO, tiles: [Vec4::ZERO; TILES * 2], masks: [Vec4::ZERO; MASKS * 2] }
    }
}

impl FullscreenMaterial for HeatHaze {
    fn fragment_shader() -> ShaderRef {
        "embedded://sa_app/heat_haze.wgsl".into()
    }
}

/// HeatHazeFXInit state: tile positions in raster pixels, seeded per screen size.
#[derive(Default)]
struct Tiles {
    size: UVec2,
    x: Vec<i32>,
    y: Vec<i32>,
    speed: Vec<i32>,
    rng: u32,
}

impl Tiles {
    /// `(rand() & 0xFFFF) * (1/32768)` with its own CRT LCG.
    fn rnd01(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(214_013).wrapping_add(2_531_011);
        (((self.rng >> 16) & 0x7FFF) & 0xFFFF) as f32 * (1.0 / 32768.0)
    }
}

/// pRasterFrontBuffer size: the power of two strictly above floor(log2(n)).
fn raster(n: u32) -> i32 {
    1 << (32 - n.max(1).leading_zeros())
}

fn attach(mut commands: Commands, cam: Single<Entity, With<Camera3d>>) {
    commands.entity(*cam).insert(HeatHaze::default());
}

fn update(
    sa: Res<SaPhys>,
    fx: Option<ResMut<Fx>>,
    time: Res<Time>,
    mut tiles: Local<Tiles>,
    mut cam: Single<(&Camera, &GlobalTransform, &Frustum, &mut HeatHaze)>,
    dbg: Res<crate::debug::DebugUi>,
) {
    let (camera, gt, frustum, ref mut hh) = *cam;
    let Some(mut fx) = fx else { return };
    if !dbg.heat_haze {
        hh.params.y = 0.0;
        return;
    }
    let w = &sa.world.weather;
    let needed = fx.man.heat_haze_needed;
    let (intensity, mode) = if w.heat_haze > 0.0 {
        (w.heat_haze_fx_control, 1.0)
    } else if needed {
        (1.0, 2.0)
    } else {
        hh.params.y = 0.0;
        return;
    };
    let Some(size) = camera.physical_target_size() else { return };
    let (sw, sh) = (size.x as f32, size.y as f32);
    let (ras_w, ras_h) = (raster(size.x), raster(size.y));
    let (sx, sy) = (sw * (1.0 / 640.0), sh * (1.0 / 448.0));
    let (src_w, src_h) = ((47.0 * sx) as i32, (47.0 * sy) as i32);
    let (dst_w, dst_h) = ((50.0 * sx) as i32, (50.0 * sy) as i32);
    let (speed_min, speed_max) = (12, 18);
    if tiles.size != size {
        // HeatHazeFXInit / DoScreenModeDependentInitializations.
        tiles.size = size;
        tiles.rng = tiles.rng.max(1);
        tiles.x.clear();
        tiles.y.clear();
        tiles.speed.clear();
        for _ in 0..TILES {
            let x = (tiles.rnd01() * (ras_w - src_w) as f32) as i32;
            let y = (tiles.rnd01() * (ras_h - src_h) as f32) as i32;
            let s = (tiles.rnd01() * (speed_max - speed_min) as f32) as i32 + speed_min;
            tiles.x.push(x);
            tiles.y.push(y);
            tiles.speed.push(s);
        }
    }
    let ts = (time.delta_secs() * 50.0).clamp(0.01, 3.0);
    let (hdx, hdy) = ((dst_w - src_w) / 2, (dst_h - src_h) / 2);
    for i in 0..TILES {
        let (mut tx, mut ty) = (tiles.x[i], tiles.y[i]);
        let (mut dx, mut dy) = (tx - hdx, ty - hdy);
        if dx < 0 {
            tx += hdx;
            dx = 0;
        }
        if dx > ras_w - dst_w {
            dx = ras_w - dst_w;
            tx -= hdx;
        }
        if dy < 0 {
            ty += hdy;
            dy = 0;
        }
        if dy > ras_h - dst_h {
            dy = ras_h - dst_h;
            ty -= hdy;
        }
        hh.tiles[2 * i] = Vec4::new(dx as f32, dy as f32, dst_w as f32, dst_h as f32);
        hh.tiles[2 * i + 1] = Vec4::new(tx as f32, ty as f32, src_w as f32, src_h as f32);
        // Rise after the quad is built; respawn at the bottom.
        tiles.y[i] -= (ts * 0.5 * tiles.speed[i] as f32) as i32;
        if tiles.y[i] < 0 {
            tiles.x[i] = (tiles.rnd01() * (ras_w - src_w) as f32) as i32;
            tiles.y[i] = ras_h - src_h;
            tiles.speed[i] = (tiles.rnd01() * (speed_max - speed_min) as f32) as i32 + speed_min;
        }
    }
    let mut n = 0;
    if mode == 2.0 {
        let fcam = fx_camera_of(gt, frustum);
        let batches = fx.man.render_heat_haze(&fcam);
        let to_px = |p: Vec3| {
            let ndc = camera.world_to_ndc(gt, g2b(p.to_array()))?;
            (ndc.z > 0.0).then(|| Vec2::new((ndc.x + 1.0) * 0.5 * sw, (1.0 - ndc.y) * 0.5 * sh))
        };
        'outer: for b in &batches {
            for q in b.verts.chunks_exact(6) {
                if n == MASKS {
                    break 'outer;
                }
                // Quad corners: v0 = uv(0,0), v2 = uv(1,0), v5 = uv(0,1).
                if q[0].rgba[3] == 0 {
                    continue;
                }
                let (Some(o), Some(u), Some(v)) = (to_px(q[0].pos), to_px(q[2].pos), to_px(q[5].pos)) else { continue };
                hh.masks[2 * n] = Vec4::new(o.x, o.y, u.x - o.x, u.y - o.y);
                hh.masks[2 * n + 1] = Vec4::new(v.x - o.x, v.y - o.y, 0.0, 0.0);
                n += 1;
            }
        }
    }
    let alpha = (80.0 * intensity.clamp(0.0, 1.0)) as i32 as f32 / 255.0;
    hh.params = Vec4::new(alpha, mode, n as f32, 0.0);
}
