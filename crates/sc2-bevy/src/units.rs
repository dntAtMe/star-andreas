//! Snapshot -> entity mirroring, interpolation, ground snapping and overlays.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use sc2_api::sim::Alliance;

use crate::{
    Sc2Link, Sc2Settings, SnapshotArrived,
    models::{ModelState, Models},
};

/// M3 models face -Y (Bevy +Z after the axis swap); turn them to SC2 facing 0 (+X).
const MODEL_YAW: f32 = std::f32::consts::FRAC_PI_2;

pub(crate) struct UnitsPlugin;

impl Plugin for UnitsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TagMap>()
            .add_systems(Startup, setup_assets)
            .add_systems(Update, (apply_snapshots, attach_models, place_units, draw_bars).chain());
    }
}

/// SC2 unit tag -> entity.
#[derive(Resource, Default)]
pub struct TagMap(pub HashMap<u64, Entity>);

#[derive(Component, Clone, Debug)]
pub struct Sc2Unit {
    pub tag: u64,
    pub unit_type: u32,
    pub name: String,
    pub owner: i32,
    pub alliance: Alliance,
    pub radius: f32,
    pub flying: bool,
    pub health: f32,
    pub health_max: f32,
    pub shield: f32,
    pub shield_max: f32,
    pub engaged: Option<u64>,
    /// Visual height in metres (for overlays).
    pub height: f32,
    /// Map positions/facings at the previous and latest snapshot.
    prev: ([f32; 2], f32),
    curr: ([f32; 2], f32),
    ground: Option<(Vec2, f32)>,
}

impl Sc2Unit {
    pub fn map_pos(&self) -> [f32; 2] {
        self.curr.0
    }
}

#[derive(Resource)]
struct UnitAssets {
    body: Handle<Mesh>,
    nose: Handle<Mesh>,
    own: Handle<StandardMaterial>,
    ally: Handle<StandardMaterial>,
    enemy: Handle<StandardMaterial>,
    neutral: Handle<StandardMaterial>,
}

fn setup_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mut mat = |c: Color| mats.add(StandardMaterial { base_color: c, perceptual_roughness: 0.6, ..default() });
    commands.insert_resource(UnitAssets {
        // Unit capsule: radius 0.5, total height 2; scaled per unit.
        body: meshes.add(Capsule3d::new(0.5, 1.0)),
        nose: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        own: mat(Color::srgb(0.15, 0.45, 0.95)),
        ally: mat(Color::srgb(0.2, 0.8, 0.9)),
        enemy: mat(Color::srgb(0.9, 0.18, 0.12)),
        neutral: mat(Color::srgb(0.55, 0.55, 0.5)),
    });
}

/// Placeholder visuals, replaced once the unit's model has loaded.
#[derive(Component)]
struct Placeholder;

/// Unit still waiting for its model.
#[derive(Component)]
struct NeedsModel;

/// Body height in metres for a unit of footprint radius `r` (metres).
fn body_height(r: f32) -> f32 {
    (r * 4.0).clamp(1.2, 6.0)
}

fn apply_snapshots(
    mut commands: Commands,
    mut arrived: MessageReader<SnapshotArrived>,
    mut tags: ResMut<TagMap>,
    mut units: Query<&mut Sc2Unit>,
    link: Option<Res<Sc2Link>>,
    settings: Res<Sc2Settings>,
    assets: Res<UnitAssets>,
) {
    let Some(link) = link else { return };
    let Some(info) = link.ready() else { return };
    let Some(SnapshotArrived(snap)) = arrived.read().last() else { return };

    let mut seen = HashSet::with_capacity(snap.units.len());
    for u in &snap.units {
        if u.alliance == Alliance::Neutral && !settings.show_neutral {
            continue;
        }
        seen.insert(u.tag);
        let pos = ([u.pos[0], u.pos[1]], u.facing);
        if let Some(&e) = tags.0.get(&u.tag)
            && let Ok(mut c) = units.get_mut(e)
        {
            c.prev = c.curr;
            c.curr = pos;
            (c.health, c.health_max, c.shield, c.shield_max) = (u.health, u.health_max, u.shield, u.shield_max);
            (c.alliance, c.flying, c.engaged, c.radius) = (u.alliance, u.flying, u.engaged_target, u.radius);
            continue;
        }

        let r = u.radius * settings.scale;
        let h = body_height(r);
        let material = match u.alliance {
            Alliance::Own => assets.own.clone(),
            Alliance::Ally => assets.ally.clone(),
            Alliance::Enemy => assets.enemy.clone(),
            Alliance::Neutral => assets.neutral.clone(),
        };
        let name = info.unit_names.get(&u.unit_type).cloned().unwrap_or_else(|| format!("#{}", u.unit_type));
        let e = commands
            .spawn((
                Name::new(format!("sc2 {name} {:x}", u.tag)),
                Sc2Unit {
                    tag: u.tag,
                    unit_type: u.unit_type,
                    name,
                    owner: u.owner,
                    alliance: u.alliance,
                    radius: u.radius,
                    flying: u.flying,
                    health: u.health,
                    health_max: u.health_max,
                    shield: u.shield,
                    shield_max: u.shield_max,
                    engaged: u.engaged_target,
                    height: h,
                    prev: pos,
                    curr: pos,
                    ground: None,
                },
                Transform::from_translation(settings.to_world(info, pos.0)),
                Visibility::default(),
                NeedsModel,
            ))
            .with_children(|p| {
                p.spawn((
                    Placeholder,
                    Mesh3d(assets.body.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::from_xyz(0.0, h * 0.5, 0.0).with_scale(Vec3::new(2.0 * r, h * 0.5, 2.0 * r)),
                ));
                // Facing marker on local +X.
                p.spawn((
                    Placeholder,
                    Mesh3d(assets.nose.clone()),
                    MeshMaterial3d(material),
                    Transform::from_xyz(r, h * 0.75, 0.0).with_scale(Vec3::splat((r * 0.6).max(0.15))),
                ));
            })
            .id();
        tags.0.insert(u.tag, e);
    }

    tags.0.retain(|tag, e| {
        let keep = seen.contains(tag);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
}

/// Swaps placeholders for the real model once it's loaded.
fn attach_models(
    mut commands: Commands,
    models: Option<ResMut<Models>>,
    settings: Res<Sc2Settings>,
    mut units: Query<(Entity, &mut Sc2Unit, &Children), With<NeedsModel>>,
    placeholders: Query<(), With<Placeholder>>,
) {
    let Some(mut models) = models else { return };
    for (e, mut u, children) in &mut units {
        match models.get(u.unit_type, &u.name) {
            ModelState::Pending => continue,
            ModelState::Failed => {}
            ModelState::Ready(parts, height) => {
                for c in children.iter().filter(|c| placeholders.contains(*c)) {
                    commands.entity(c).despawn();
                }
                let s = settings.scale;
                u.height = height * s;
                commands.entity(e).with_children(|p| {
                    for (mesh, mat) in parts {
                        p.spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(mat.clone()),
                            Transform::from_rotation(Quat::from_rotation_y(MODEL_YAW)).with_scale(Vec3::splat(s)),
                        ));
                    }
                });
            }
        }
        commands.entity(e).remove::<NeedsModel>();
    }
}

fn place_units(
    time: Res<Time>,
    link: Option<Res<Sc2Link>>,
    settings: Res<Sc2Settings>,
    rapier: ReadRapierContext,
    mut units: Query<(&mut Sc2Unit, &mut Transform)>,
) {
    let Some(link) = link else { return };
    let Some(info) = link.ready() else { return };
    let alpha = ((time.elapsed_secs() - link.snapshot_at) / link.step_secs).clamp(0.0, 1.0);
    let ctx = rapier.single().ok();

    for (mut u, mut tf) in &mut units {
        let p = [
            u.prev.0[0] + (u.curr.0[0] - u.prev.0[0]) * alpha,
            u.prev.0[1] + (u.curr.0[1] - u.prev.0[1]) * alpha,
        ];
        let w = settings.to_world(info, p);
        let xz = Vec2::new(w.x, w.z);

        // Re-probe the ground only after moving a bit. Cast from just above the
        // last known ground so roofs and bridges overhead are skipped.
        let ground = match u.ground {
            Some((at, y)) if at.distance_squared(xz) < 0.25 * 0.25 => y,
            _ => {
                let y = ctx
                    .as_ref()
                    .and_then(|c| {
                        let base = u.ground.map_or(w.y, |g| g.1);
                        let from = Vec3::new(w.x, base + 2.5, w.z);
                        c.cast_ray(from, -Vec3::Y, 200.0, true, QueryFilter::only_fixed()).map(|(_, t)| from.y - t)
                    })
                    .or(u.ground.map(|g| g.1))
                    .unwrap_or(w.y);
                u.ground = Some((xz, y));
                y
            }
        };
        let fly = if u.flying { 4.0 * settings.scale } else { 0.0 };
        tf.translation = Vec3::new(w.x, ground + fly, w.z);

        // SC2 facing is CCW from +X about Z-up, which is a rotation about Bevy +Y.
        let (a, b) = (u.prev.1, u.curr.1);
        let d = (b - a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        tf.rotation = Quat::from_rotation_y(a + d * alpha);
    }
}

fn draw_bars(
    mut gizmos: Gizmos,
    settings: Res<Sc2Settings>,
    cam: Query<&GlobalTransform, With<Camera3d>>,
    units: Query<(&Sc2Unit, &Transform)>,
) {
    let Some(cam) = cam.iter().next() else { return };
    let right = cam.right().as_vec3();
    for (u, tf) in &units {
        if u.alliance == Alliance::Neutral || u.health_max <= 0.0 {
            continue;
        }
        let r = u.radius * settings.scale;
        let top = tf.translation + Vec3::Y * (u.height + 0.35);
        let half = (r * 1.2).clamp(0.4, 3.0);
        let (a, b) = (top - right * half, top + right * half);
        let hp = (u.health / u.health_max).clamp(0.0, 1.0);
        let mid = a.lerp(b, hp);
        gizmos.line(a, mid, Color::srgb(1.0 - hp, 0.2 + 0.7 * hp, 0.1));
        if hp < 1.0 {
            gizmos.line(mid, b, Color::srgb(0.15, 0.15, 0.15));
        }
        if u.shield_max > 0.0 {
            let sh = (u.shield / u.shield_max).clamp(0.0, 1.0);
            let up = Vec3::Y * 0.12;
            gizmos.line(a + up, (a + up).lerp(b + up, sh), Color::srgb(0.3, 0.6, 1.0));
        }
    }
}
