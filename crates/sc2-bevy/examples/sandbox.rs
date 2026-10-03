//! SC2 units on a flat test ground (no GTA world).
//! `cargo run -p sc2-bevy --example sandbox`
//! Camera: WASD pan, wheel zoom, Q/E rotate.
//! `SC2_DEMO=1 [SC2_SHOT=<png>]`: scripted fight (see sc2_bevy demo).
//! `SANDBOX_CAM=x,z,dist,yaw`: start view.

use bevy::{input::mouse::AccumulatedMouseScroll, prelude::*};
use sc2_bevy::{Sc2Plugin, Sc2Settings};

fn main() {
    let mut settings = Sc2Settings::from_env();
    settings.anchor = [0.0, 0.0, 0.0];
    settings.show_neutral = true;
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: "sc2 sandbox".into(), ..default() }),
            ..default()
        }))
        .add_plugins(Sc2Plugin { settings })
        .insert_resource(ClearColor(Color::srgb(0.55, 0.65, 0.8)))
        .add_systems(Startup, setup)
        .add_systems(Update, rts_camera)
        .run();
}

#[derive(Component)]
struct RtsCam {
    focus: Vec3,
    yaw: f32,
    dist: f32,
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(400.0, 400.0))),
        MeshMaterial3d(mats.add(StandardMaterial { base_color: Color::srgb(0.35, 0.42, 0.3), ..default() })),
    ));
    for i in -20..=20 {
        let f = i as f32 * 10.0;
        let c = Color::srgb(0.3, 0.36, 0.26);
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(400.0, 0.02, 0.08))),
            MeshMaterial3d(mats.add(StandardMaterial { base_color: c, unlit: true, ..default() })),
            Transform::from_xyz(0.0, 0.01, f),
        ));
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(0.08, 0.02, 400.0))),
            MeshMaterial3d(mats.add(StandardMaterial { base_color: c, unlit: true, ..default() })),
            Transform::from_xyz(f, 0.01, 0.0),
        ));
    }
    commands.spawn((
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: true, ..default() },
        Transform::default().looking_to(Vec3::new(-0.4, -1.0, -0.3), Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 400.0, ..default() });
    // `SANDBOX_CAM=x,z,dist,yaw` overrides the start view.
    let v: Vec<f32> = std::env::var("SANDBOX_CAM").unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let (focus, dist, yaw) = match v[..] {
        [x, z, d, y] => (Vec3::new(x, 0.0, z), d, y),
        _ => (Vec3::ZERO, 30.0, 0.0),
    };
    commands.spawn((Camera3d::default(), Transform::default(), RtsCam { focus, yaw, dist }));
}

fn rts_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    cam: Single<(&mut Transform, &mut RtsCam)>,
) {
    let (mut tf, mut rc) = cam.into_inner();
    let dt = time.delta_secs();
    rc.dist = (rc.dist * 0.9f32.powf(scroll.delta.y)).clamp(5.0, 200.0);
    if keys.pressed(KeyCode::KeyQ) {
        rc.yaw += dt * 1.5;
    }
    if keys.pressed(KeyCode::KeyE) {
        rc.yaw -= dt * 1.5;
    }
    let fwd = Vec3::new(-rc.yaw.sin(), 0.0, -rc.yaw.cos());
    let right = Vec3::new(-fwd.z, 0.0, fwd.x);
    let mut d = Vec3::ZERO;
    for (k, v) in [(KeyCode::KeyW, fwd), (KeyCode::KeyS, -fwd), (KeyCode::KeyD, right), (KeyCode::KeyA, -right)] {
        if keys.pressed(k) {
            d += v;
        }
    }
    let speed = rc.dist * 1.2;
    rc.focus += d.normalize_or_zero() * speed * dt;
    let offset = (-fwd * 0.75 + Vec3::Y).normalize() * rc.dist;
    *tf = Transform::from_translation(rc.focus + offset).looking_at(rc.focus, Vec3::Y);
}
