//! Weather presentation: rain streaks (`RenderRainStreaks`) and
//! debug controls. The simulation (CClock / CWeather) lives in sa-physics' World.
//!
//! `SA_TIME=hh:mm` sets the clock, `SA_WEATHER=<0..22>` forces a weather type now;
//! N cycles forced weather in game, M releases it.

use bevy::{asset::RenderAssetUsages, mesh::PrimitiveTopology, prelude::*};
use sa_physics::weather::WEATHER_NAMES;

use crate::{
    saphys::{SaPhys, SaStep},
    world::g2b,
};

pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init)
            .add_systems(Update, (debug_keys.before(SaStep), draw_streaks.after(SaStep)));
    }
}

#[derive(Resource)]
struct Streaks(Handle<Mesh>);


fn init(
    mut commands: Commands,
    mut sa: ResMut<SaPhys>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if let Some((h, m)) = std::env::var("SA_TIME").ok().and_then(|v| {
        let (h, m) = v.split_once(':')?;
        Some((h.trim().parse().ok()?, m.trim().parse().ok()?))
    }) {
        let now = sa.world.now_ms;
        sa.world.clock.set(now, h, m);
    }
    if let Some(t) = std::env::var("SA_WEATHER").ok().and_then(|v| v.parse::<i16>().ok()) {
        sa.world.weather.force_now(t.clamp(0, 22));
    }
    // Untextured lines, alpha blended, no fog (FOGENABLE off), z-write off.
    let mesh = meshes.add(
        Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 2])
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 2]),
    );
    let mat = materials.add(StandardMaterial {
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        fog_enabled: false,
        ..default()
    });
    commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), Transform::default(), bevy::camera::visibility::NoFrustumCulling));
    commands.insert_resource(Streaks(mesh));
}

fn debug_keys(keys: Res<ButtonInput<KeyCode>>, mut sa: ResMut<SaPhys>) {
    let w = &mut sa.world.weather;
    if keys.just_pressed(KeyCode::KeyN) {
        let t = if w.forced_type < 0 { w.new_type + 1 } else { w.forced_type + 1 };
        w.force_now(t % 20);
        info!("weather forced: {}", WEATHER_NAMES[w.new_type as usize]);
    }
    if keys.just_pressed(KeyCode::KeyM) {
        w.release();
        info!("weather released");
    }
}

fn draw_streaks(sa: Res<SaPhys>, streaks: Res<Streaks>, mut meshes: ResMut<Assets<Mesh>>, dbg: Res<crate::debug::DebugUi>) {
    let Some(mut m) = meshes.get_mut(&streaks.0) else { return };
    let none = Vec::new();
    let s = if dbg.rain_streaks { &sa.world.weather.streaks } else { &none };
    let mut pos = Vec::with_capacity(s.len() * 2 + 2);
    let mut col = Vec::with_capacity(s.len() * 2 + 2);
    // RGB 210,210,230; bottom alpha A, top alpha A/2.
    let rgb = Color::srgb_u8(210, 210, 230).to_linear();
    for st in s {
        pos.push(g2b(st.bottom.to_array()).to_array());
        col.push([rgb.red, rgb.green, rgb.blue, st.alpha as f32 / 255.0]);
        pos.push(g2b(st.top.to_array()).to_array());
        col.push([rgb.red, rgb.green, rgb.blue, (st.alpha / 2) as f32 / 255.0]);
    }
    if pos.is_empty() {
        pos = vec![[0.0; 3]; 2];
        col = vec![[0.0; 4]; 2];
    }
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
}
