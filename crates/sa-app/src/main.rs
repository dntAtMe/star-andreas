mod stream;
mod world;

use std::{path::PathBuf, sync::Arc};

use bevy::{
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    window::{CursorGrabMode, CursorOptions},
};
use stream::{StreamCamera, StreamPlugin, Streamer};
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
    println!(
        "world: {} objects, {} instances, loaded in {:.2?}",
        world.objects.len(),
        world.instances.len(),
        t.elapsed()
    );

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: "sa-rs".into(), ..default() }),
            ..default()
        }))
        .insert_resource(WorldRes(Arc::new(world)))
        .insert_resource(ClearColor(SKY))
        .add_plugins(StreamPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (fly_camera, update_hud, auto_screenshot))
        .run();
    Ok(())
}

#[derive(Component)]
struct FlyCam {
    yaw: f32,
    pitch: f32,
    speed: f32,
}

#[derive(Component)]
struct Hud;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
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
        StreamCamera,
    ));

    // Placeholder sea at z = 0 until water.dat is parsed.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(12000.0, 12000.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgba(0.18, 0.33, 0.42, 0.85),
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        })),
        Transform::from_xyz(0.0, -0.05, 0.0),
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
) {
    let (mut tf, mut fc) = cam.into_inner();
    let looking = buttons.pressed(MouseButton::Right);
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
    cam: Single<(&Transform, &FlyCam)>,
    mut hud: Single<&mut Text, With<Hud>>,
) {
    let (tf, fc) = *cam;
    let p = b2g(tf.translation);
    let s = st.stats;
    hud.0 = format!(
        "pos {:.0} {:.0} {:.0}  speed {:.0}  fps {:.0}\n\
         instances {}  pending {}  models {} (+{} loading)  txds {}\n\
         RMB look, WASD/QE move, Shift fast, wheel speed",
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
    );
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
    match *state {
        0 if *idle > 1.5 || time.elapsed_secs() > 90.0 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            *state = 1;
            *idle = 0.0;
        }
        1 if *idle > 1.0 => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}
