//! RTS input: box/click selection, right-click orders, debug spawns, HUD.
//!
//! Tab toggles command mode. LMB click/drag selects own units, RMB click
//! orders them (smart; Ctrl = attack-move; Shift = queue), Z stop, H hold,
//! F3 selects the whole army, 1-6 spawn test units at the cursor.

use std::collections::HashSet;

use bevy::{prelude::*, window::{CursorGrabMode, CursorOptions, PrimaryWindow}};
use sa_physics::bevy_api::SaPhysics;
use sc2_api::sim::{Alliance, Command};

use crate::{Sc2Link, Sc2Settings, anim::Dying, units::Sc2Unit};

pub(crate) struct ControlPlugin;

impl Plugin for ControlPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Sc2Control { active: true, selected: HashSet::new() })
            .add_systems(Startup, setup_ui)
            .add_systems(Update, (toggle, select, order, keys, draw_selection, update_hud).chain());
    }
}

#[derive(Resource)]
pub struct Sc2Control {
    pub active: bool,
    pub selected: HashSet<u64>,
}

#[derive(Component)]
struct SelectBox;

#[derive(Component)]
struct Hud;

/// (unit type, own side?, count) for keys 1-6.
const SPAWNS: [(KeyCode, u32, bool, u32); 6] = [
    (KeyCode::Digit1, 48, true, 4),   // Marine
    (KeyCode::Digit2, 105, false, 6), // Zergling
    (KeyCode::Digit3, 73, false, 2),  // Zealot
    (KeyCode::Digit4, 51, true, 2),   // Marauder
    (KeyCode::Digit5, 110, false, 3), // Roach
    (KeyCode::Digit6, 33, true, 1),   // Siege Tank
];

fn setup_ui(mut commands: Commands) {
    commands.spawn((
        SelectBox,
        Node { position_type: PositionType::Absolute, border: UiRect::all(px(1)), display: Display::None, ..default() },
        BorderColor::all(Color::srgb(0.3, 1.0, 0.3)),
        BackgroundColor(Color::srgba(0.3, 1.0, 0.3, 0.08)),
    ));
    commands.spawn((
        Hud,
        Text::default(),
        TextFont { font_size: bevy::text::FontSize::Px(14.0), ..default() },
        TextColor(Color::srgb(0.85, 1.0, 0.85)),
        Node { position_type: PositionType::Absolute, bottom: px(8), left: px(8), ..default() },
    ));
}

struct Pointer<'a> {
    window: &'a Window,
    camera: &'a Camera,
    cam_tf: &'a GlobalTransform,
}

impl Pointer<'_> {
    fn cursor(&self) -> Option<Vec2> {
        self.window.cursor_position()
    }

    fn screen(&self, w: Vec3) -> Option<Vec2> {
        self.camera.world_to_viewport(self.cam_tf, w).ok()
    }

    /// World point under the cursor: SA map hit, else the anchor's ground plane.
    fn ground(&self, phys: Option<&mut SaPhysics>, plane_y: f32) -> Option<Vec3> {
        let ray = self.camera.viewport_to_world(self.cam_tf, self.cursor()?).ok()?;
        let dir = ray.direction.as_vec3();
        if let Some(hit) = phys.and_then(|ph| ph.cast_ray(ray.origin, dir, 3000.0, true, None)) {
            return Some(hit.point);
        }
        let t = ray.intersect_plane(Vec3::new(0.0, plane_y, 0.0), InfinitePlane3d::new(Vec3::Y))?;
        Some(ray.get_point(t))
    }

    /// Unit closest to the cursor within its on-screen footprint (min 18px).
    fn unit_at<'u>(&self, units: impl Iterator<Item = (&'u Sc2Unit, &'u Transform)>, scale: f32) -> Option<&'u Sc2Unit> {
        let c = self.cursor()?;
        units
            .filter_map(|(u, tf)| {
                let centre = self.screen(tf.translation + Vec3::Y * 0.8)?;
                let edge = self.screen(tf.translation + Vec3::Y * 0.8 + self.cam_tf.right() * u.radius * scale)?;
                let d = centre.distance(c);
                (d <= centre.distance(edge).max(18.0)).then_some((u, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(u, _)| u)
    }
}

fn cursor_free(cursor: &CursorOptions) -> bool {
    cursor.grab_mode == CursorGrabMode::None && cursor.visible
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut ctl: ResMut<Sc2Control>) {
    if keys.just_pressed(KeyCode::Tab) {
        ctl.active = !ctl.active;
    }
}

#[allow(clippy::too_many_arguments)]
fn select(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut ctl: ResMut<Sc2Control>,
    settings: Res<Sc2Settings>,
    window: Single<(&Window, &CursorOptions), With<PrimaryWindow>>,
    cam: Single<(&Camera, &GlobalTransform), With<Camera3d>>,
    units: Query<(&Sc2Unit, &Transform), Without<Dying>>,
    mut boxq: Single<&mut Node, With<SelectBox>>,
    mut drag: Local<Option<Vec2>>,
) {
    let (window, cursor) = *window;
    let p = Pointer { window, camera: cam.0, cam_tf: cam.1 };
    if !ctl.active || !cursor_free(cursor) {
        *drag = None;
        boxq.display = Display::None;
        return;
    }
    let Some(c) = p.cursor() else { return };
    if buttons.just_pressed(MouseButton::Left) {
        *drag = Some(c);
    }
    let Some(start) = *drag else { return };
    let (min, max) = (start.min(c), start.max(c));
    let dragging = start.distance(c) > 5.0;
    if buttons.pressed(MouseButton::Left) {
        boxq.display = if dragging { Display::Flex } else { Display::None };
        (boxq.left, boxq.top, boxq.width, boxq.height) = (px(min.x), px(min.y), px(max.x - min.x), px(max.y - min.y));
        return;
    }
    // Released.
    *drag = None;
    boxq.display = Display::None;
    let additive = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !additive {
        ctl.selected.clear();
    }
    if dragging {
        for (u, tf) in &units {
            if u.alliance == Alliance::Own
                && let Some(s) = p.screen(tf.translation + Vec3::Y * 0.5)
                && s.cmpge(min).all()
                && s.cmple(max).all()
            {
                ctl.selected.insert(u.tag);
            }
        }
    } else if let Some(u) = p.unit_at(units.iter().filter(|(u, _)| u.alliance == Alliance::Own), settings.scale) {
        ctl.selected.insert(u.tag);
    }
}

#[allow(clippy::too_many_arguments)]
fn order(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    ctl: Res<Sc2Control>,
    link: Option<Res<Sc2Link>>,
    settings: Res<Sc2Settings>,
    mut phys: Option<ResMut<SaPhysics>>,
    window: Single<(&Window, &CursorOptions), With<PrimaryWindow>>,
    cam: Single<(&Camera, &GlobalTransform), With<Camera3d>>,
    units: Query<(&Sc2Unit, &Transform), Without<Dying>>,
    mut press: Local<Option<(Vec2, f32)>>,
    time: Res<Time>,
) {
    let (Some(link), (window, cursor)) = (link, *window) else { return };
    let Some(info) = link.ready() else { return };
    let p = Pointer { window, camera: cam.0, cam_tf: cam.1 };
    // RMB also drives camera look in sa-app; only a short, still click is an order.
    if buttons.just_pressed(MouseButton::Right) {
        *press = p.cursor().map(|c| (c, time.elapsed_secs()));
    }
    if !buttons.just_released(MouseButton::Right) {
        return;
    }
    let Some((at, t0)) = press.take() else { return };
    if !ctl.active || ctl.selected.is_empty() || time.elapsed_secs() - t0 > 0.35 {
        return;
    }
    // The cursor may have been hidden by camera look; use the press position.
    let _ = cursor;
    if p.cursor().is_some_and(|c| c.distance(at) > 8.0) {
        return;
    }
    let plane_y = settings.anchor[2];
    let Some(hit) = p.ground(phys.as_deref_mut(), plane_y) else { return };
    let to = settings.to_map(info, hit);
    let sel: Vec<u64> = ctl.selected.iter().copied().collect();
    let queue = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let target = p.unit_at(units.iter().filter(|(u, _)| u.alliance != Alliance::Own), settings.scale).map(|u| u.tag);
    let cmd = if keys.pressed(KeyCode::ControlLeft) {
        match target {
            Some(t) => Command::Attack { units: sel, target: t },
            None => Command::AttackMove { units: sel, to, queue },
        }
    } else if queue {
        Command::Move { units: sel, to, queue }
    } else {
        Command::Smart { units: sel, to, target }
    };
    link.send(cmd);
}

#[allow(clippy::too_many_arguments)]
fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut ctl: ResMut<Sc2Control>,
    link: Option<Res<Sc2Link>>,
    settings: Res<Sc2Settings>,
    mut phys: Option<ResMut<SaPhysics>>,
    window: Single<&Window, With<PrimaryWindow>>,
    cam: Single<(&Camera, &GlobalTransform), With<Camera3d>>,
    units: Query<&Sc2Unit, Without<Dying>>,
) {
    let Some(link) = link else { return };
    let Some(info) = link.ready() else { return };
    if !ctl.active {
        return;
    }
    let sel: Vec<u64> = ctl.selected.iter().copied().collect();
    if keys.just_pressed(KeyCode::KeyZ) {
        link.send(Command::Stop { units: sel.clone() });
    }
    if keys.just_pressed(KeyCode::KeyH) {
        link.send(Command::Hold { units: sel });
    }
    if keys.just_pressed(KeyCode::F3) {
        ctl.selected = units.iter().filter(|u| u.alliance == Alliance::Own && u.health_max < 1000.0).map(|u| u.tag).collect();
    }
    let p = Pointer { window: &window, camera: cam.0, cam_tf: cam.1 };
    for (key, unit_type, own, count) in SPAWNS {
        if keys.just_pressed(key)
            && let Some(hit) = p.ground(phys.as_deref_mut(), settings.anchor[2])
        {
            let owner = if own { info.player_id as i32 } else { 3 - info.player_id as i32 };
            link.send(Command::Spawn { unit_type, owner, at: settings.to_map(info, hit), count });
        }
    }
}

fn draw_selection(mut gizmos: Gizmos, ctl: Res<Sc2Control>, settings: Res<Sc2Settings>, units: Query<(&Sc2Unit, &Transform), Without<Dying>>) {
    for (u, tf) in &units {
        if ctl.selected.contains(&u.tag) {
            let iso = Isometry3d::new(tf.translation + Vec3::Y * 0.05, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
            gizmos.circle(iso, u.radius * settings.scale * 1.15, Color::srgb(0.2, 1.0, 0.2));
        }
    }
}

fn update_hud(link: Option<Res<Sc2Link>>, mut ctl: ResMut<Sc2Control>, units: Query<&Sc2Unit, Without<Dying>>, mut hud: Single<&mut Text, With<Hud>>) {
    let Some(link) = link else { return };
    // Forget dead units.
    let alive: HashSet<u64> = units.iter().map(|u| u.tag).collect();
    ctl.selected.retain(|t| alive.contains(t));
    let count = |a| units.iter().filter(|u| u.alliance == a).count();
    let mut sel: Vec<&Sc2Unit> = units.iter().filter(|u| ctl.selected.contains(&u.tag)).collect();
    sel.sort_by_key(|u| u.tag);
    let names = sel.iter().take(6).map(|u| format!("{} {:.0}/{:.0}", u.name, u.health, u.health_max)).collect::<Vec<_>>().join(", ");
    hud.0 = format!(
        "SC2: {}  loop {}\nown {}  enemy {}  neutral {}\ncommand mode {} [Tab]  selected {}{}\n\
         LMB select/drag, RMB order, Ctrl+RMB attack, Shift queue, Z stop, H hold, F3 army\n\
         spawn at cursor: 1 marines 2 zerglings 3 zealots 4 marauders 5 roaches 6 tank",
        link.status,
        link.game_loop,
        count(Alliance::Own),
        count(Alliance::Enemy),
        count(Alliance::Neutral),
        if ctl.active { "ON" } else { "off" },
        ctl.selected.len(),
        if names.is_empty() { String::new() } else { format!(": {names}") },
    );
}
