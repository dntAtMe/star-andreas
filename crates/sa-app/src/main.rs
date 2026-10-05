mod audio;
mod breaks;
mod camera;
mod debug;
mod colour_filter;
mod coronas;
mod cutscene;
mod fx;
mod gfx;
mod heat_haze;
mod hud;
mod lights;
mod peds;
mod player;
mod saphys;
mod script;
mod shadows;
mod sky;
mod target_tri;
mod stream;
mod vehicle;
mod dynlight;
mod wasted;
mod water;
mod weapons;
mod weather;
mod world;
mod world_material;

use std::{path::PathBuf, sync::Arc};

use bevy::{
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    window::{CursorGrabMode, CursorOptions},
};
use player::{GameRoot, Mode, OrbitCam, Ped, PlayerPlugin};
use stream::{StreamCamera, StreamPlugin, Streamer};
use vehicle::{Driving, Vehicle, VehiclePlugin};
use world::{World as SaWorld, WorldRes, b2g, g2b};

const DEFAULT_GAME_DIR: &str = r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas";
const SKY: Color = Color::srgb(0.62, 0.72, 0.85);

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .or_else(|| std::env::var("SA_DIR").ok())
            .unwrap_or_else(|| DEFAULT_GAME_DIR.into()),
    );
    let t = std::time::Instant::now();
    let world = SaWorld::load(&root)?;
    // SA_LISTPROPS=x,y,radius: print knockable props near a GTA position (debug).
    if let Ok(q) = std::env::var("SA_LISTPROPS") {
        let v: Vec<f32> = q.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if let [x, y, r] = v[..] {
            for inst in &world.instances {
                let g = b2g(inst.pos);
                let d = ((g[0] - x).powi(2) + (g[1] - y).powi(2)).sqrt();
                let model = &world.objects[&inst.id].model;
                if let Some(p) = world.physics.get(model).filter(|p| !p.is_static() && d < r) {
                    println!("prop {model} at {:.1},{:.1},{:.1} d={d:.0} mass {} uproot {}", g[0], g[1], g[2], p.mass, p.uproot);
                }
            }
        }
    }
    println!(
        "world: {} objects, {} instances, loaded in {:.2?}",
        world.objects.len(),
        world.instances.len(),
        t.elapsed()
    );

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        // SA_NOVSYNC=1: uncapped frame rate (measuring).
        primary_window: Some(Window {
            title: "sa-rs".into(),
            present_mode: if std::env::var("SA_NOVSYNC").is_ok() { bevy::window::PresentMode::AutoNoVsync } else { bevy::window::PresentMode::AutoVsync },
            ..default()
        }),
        ..default()
    }))
    .insert_resource(WorldRes(Arc::new(world)))
    .insert_resource(ClearColor(SKY))
    .insert_resource(GameRoot(root))
    .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 600.0, ..default() })
    .add_plugins((
        StreamPlugin,
        PlayerPlugin,
        VehiclePlugin,
        saphys::SaPhysPlugin,
        fx::FxPlugin,
        weather::WeatherPlugin,
        shadows::ShadowsPlugin,
        heat_haze::HeatHazePlugin,
        debug::DebugPlugin,
        world_material::WorldMaterialPlugin,
        sky::SkyPlugin,
        coronas::CoronasPlugin,
        lights::LightsPlugin,
        colour_filter::ColourFilterPlugin,
    ))
    .add_plugins((camera::CameraPlugin, weapons::WeaponsPlugin, wasted::WastedPlugin, water::WaterPlugin, dynlight::DynLightPlugin, peds::NpcPlugin, breaks::BreaksPlugin, hud::HudPlugin, target_tri::TargetTrianglePlugin, gfx::GfxPlugin, audio::AudioPlugin, cutscene::CutscenePlugin, script::ScriptPlugin))
    .add_systems(Startup, setup)
    .add_systems(Update, (fly_camera.run_if(resource_equals(Mode::Fly)), update_hud, auto_screenshot))
    .add_systems(Last, fps_cap);
    #[cfg(feature = "sc2")]
    app.add_plugins(sc2_bevy::Sc2Plugin::default());
    app.run();
    Ok(())
}

#[derive(Component)]
pub struct FlyCam {
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
}

#[derive(Component)]
struct Hud;

fn setup(mut commands: Commands) {
    // Grove Street, looking north-west.
    let v: Vec<f32> = std::env::var("SA_POS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    let (start, yaw, pitch) = match v[..] {
        [x, y, z, yaw, pitch] => (g2b([x, y, z]), yaw, pitch),
        _ => (g2b([2495.0, -1670.0, 40.0]), 0.8, -0.25),
    };
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { far: 6000.0, fov: 70f32.to_radians(), ..default() }),
        Transform::from_translation(start).with_rotation(Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0)),
        DistanceFog { color: SKY, falloff: FogFalloff::Linear { start: 900.0, end: 3200.0 }, ..default() },
        FlyCam { yaw, pitch, speed: 60.0 },
        // SA_ORBIT=<yaw>: the starting orbit yaw (debug).
        OrbitCam { yaw: std::env::var("SA_ORBIT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0), pitch: -0.15, dist: 3.5 },
        StreamCamera,
    ));

    // Noon sun for dynamic (lit) objects like peds; the map itself is prelit.
    commands.spawn((
        DirectionalLight { illuminance: 9000.0, ..default() },
        gfx::sun_cascades(),
        Transform::from_xyz(0.0, 0.0, 0.0).looking_to(Vec3::new(-0.4, -1.0, -0.3), Vec3::Y),
    ));

    commands.spawn((
        Text::default(),
        TextFont { font_size: bevy::text::FontSize::Px(15.0), ..default() },
        TextColor(Color::WHITE),
        Node { position_type: PositionType::Absolute, top: px(8), left: px(8), ..default() },
        Hud,
    ));
}

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut cursor: Single<&mut CursorOptions>,
    cam: Single<(&mut Transform, &mut FlyCam)>,
    dbg: Res<debug::DebugUi>,
) {
    let (mut tf, mut fc) = cam.into_inner();
    let looking = buttons.pressed(MouseButton::Right) && !dbg.capture_mouse;
    cursor.grab_mode = if looking { CursorGrabMode::Locked } else { CursorGrabMode::None };
    cursor.visible = !looking;

    if looking {
        fc.yaw -= motion.delta.x * 0.003;
        fc.pitch = (fc.pitch - motion.delta.y * 0.003).clamp(-1.54, 1.54);
    }
    if scroll.delta.y != 0.0 {
        fc.speed = (fc.speed * 1.2f32.powf(scroll.delta.y)).clamp(2.0, 2000.0);
    }
    tf.rotation = Quat::from_euler(EulerRot::YXZ, fc.yaw, fc.pitch, 0.0);

    let mut dir = Vec3::ZERO;
    let (fwd, right) = (*tf.forward(), *tf.right());
    for (key, v) in [
        (KeyCode::KeyW, fwd),
        (KeyCode::KeyS, -fwd),
        (KeyCode::KeyD, right),
        (KeyCode::KeyA, -right),
        (KeyCode::KeyE, Vec3::Y),
        (KeyCode::KeyQ, -Vec3::Y),
    ] {
        if keys.pressed(key) {
            dir += v;
        }
    }
    let boost = if keys.pressed(KeyCode::ShiftLeft) { 5.0 } else { 1.0 };
    tf.translation += dir.normalize_or_zero() * fc.speed * boost * time.delta_secs();
}

fn update_hud(
    time: Res<Time>,
    st: Res<Streamer>,
    mode: Res<Mode>,
    driving: Res<Driving>,
    sa: Res<saphys::SaPhys>,
    cars: Query<&Vehicle>,
    cam: Single<(&Transform, &FlyCam)>,
    ped: Single<(&Transform, &Ped), Without<FlyCam>>,
    mut hud: Single<(&mut Text, &mut Visibility), With<Hud>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut shown: Local<Option<bool>>,
    mut fps_log: Local<(f32, u32)>,
) {
    // F3 toggles the debug text (hidden by default; SA_DEBUG_TEXT=1 shows it at start).
    let on = shown.get_or_insert_with(|| std::env::var("SA_DEBUG_TEXT").is_ok());
    if keys.just_pressed(KeyCode::F3) {
        *on = !*on;
    }
    let want = if *on { Visibility::Inherited } else { Visibility::Hidden };
    if *hud.1 != want {
        *hud.1 = want;
    }
    // SA_FPSLOG=1: log the average frame rate every 2 s (debug).
    if std::env::var("SA_FPSLOG").is_ok() {
        fps_log.0 += time.delta_secs();
        fps_log.1 += 1;
        if fps_log.0 >= 2.0 {
            info!("fps {:.1}", fps_log.1 as f32 / fps_log.0);
            *fps_log = (0.0, 0);
        }
    }
    let hud = &mut hud.0;
    let (tf, fc) = *cam;
    let (ptf, ped) = *ped;
    let p = b2g(if *mode == Mode::Walk { ptf.translation } else { tf.translation });
    let s = st.stats;
    hud.0 = format!(
        "pos {:.0} {:.0} {:.0}  speed {:.0}  fps {:.0}\n\
         instances {}  pending {}  models {} (+{} loading)  txds {}\n\
         mode {:?}{}{}{}{}  (F2 toggles walk/fly)\n\
         {:02}:{:02}  {} -> {} ({:.0}%)  rain {:.2}  wind {:.2}  (N next weather, M release, B damage car)\n\
         walk: click grabs mouse, Esc releases, WASD, Shift sprint, Alt walk, Space jump, V spawn car, F enter/exit\n\
         guns: F1 debug UI gives weapons, RMB aim, LMB fire, wheel or Q/E switch, C crouch (A/D while aiming rolls), Home camera zoom\n\
         drive: W throttle, S brake/reverse, A/D steer, Space handbrake\n\
         fly: RMB look, WASD/QE move, Shift fast, wheel speed",
        p[0],
        p[1],
        p[2],
        fc.speed,
        1.0 / time.delta_secs().max(1e-4),
        s.spawned,
        s.pending,
        s.models_ready,
        s.models_loading,
        s.txds,
        *mode,
        if ped.frozen { "  [waiting for collision]" } else { "" },
        if ped.grounded { "  grounded" } else { "" },
        {
            use saphys::SaPhysExt;
            sa.logic::<sa_physics::ped::PedLogic>(ped.sa)
                .map(|l| format!("  health {:.0}/{:.0}  armour {:.0}", l.tasks.health.health, l.tasks.health.max_health, l.tasks.health.armour))
                .unwrap_or_default()
                + &format!("  wanted {} (chaos {})", sa.world.wanted.level, sa.world.wanted.chaos)
        },
        driving
            .0
            .and_then(|c| cars.get(c).ok())
            .map(|v| format!("  driving {} {:.0} km/h  health {:.0}", v.name, v.speed.abs() * 3.6, v.health))
            .unwrap_or_default(),
        sa.world.clock.hours,
        sa.world.clock.minutes,
        sa_physics::weather::WEATHER_NAMES[sa.world.weather.old_type as usize],
        sa_physics::weather::WEATHER_NAMES[sa.world.weather.new_type as usize],
        sa.world.weather.interpolation * 100.0,
        sa.world.weather.rain,
        sa.world.weather.wind,
    );
}

/// `SA_FPS_CAP=<fps>`: sleep to cap the frame rate (debug: check frame-rate independence).
fn fps_cap(mut last: Local<Option<std::time::Instant>>) {
    let Some(cap) = std::env::var("SA_FPS_CAP").ok().and_then(|v| v.parse::<f64>().ok()) else { return };
    let frame = std::time::Duration::from_secs_f64(1.0 / cap);
    if let Some(t) = *last {
        if let Some(left) = frame.checked_sub(t.elapsed()) {
            std::thread::sleep(left);
        }
    }
    *last = Some(std::time::Instant::now());
}

/// `SA_SHOT=<png>`: once streaming has settled, save a screenshot and exit.
/// `SA_POS=x,y,z,yaw,pitch` (GTA coords) overrides the start camera.
fn auto_screenshot(
    mut commands: Commands,
    time: Res<Time>,
    st: Res<Streamer>,
    mut idle: Local<f32>,
    mut state: Local<u8>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(path) = std::env::var("SA_SHOT") else { return };
    let s = st.stats;
    let settled = s.pending == 0 && s.models_loading == 0 && s.spawned > 0;
    *idle = if settled { *idle + time.delta_secs() } else { 0.0 };
    // SA_SHOT_AFTER=<secs>: shoot at a fixed time instead of when streaming settles.
    // SA_SHOT_AFTER=<s1>,<s2>,...: several shots, `{}` in the path replaced by the index.
    let times: Vec<f32> = std::env::var("SA_SHOT_AFTER").unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
    if times.len() > 1 {
        let k = (*state as usize).min(times.len());
        if k < times.len() && time.elapsed_secs() > times[k] {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.replace("{}", &k.to_string())));
            *state += 1;
        } else if k == times.len() && time.elapsed_secs() > times[k - 1] + 1.5 {
            exit.write(AppExit::Success);
        }
        return;
    }
    let after: Option<f32> = times.first().copied();
    let due = match after {
        Some(t) => time.elapsed_secs() > t,
        None => *idle > 1.5 || time.elapsed_secs() > 90.0,
    };
    match *state {
        0 if due => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            *state = 1;
            *idle = 0.0;
        }
        1 if *idle > 1.0 || after.is_some_and(|t| time.elapsed_secs() > t + 1.5) => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}
