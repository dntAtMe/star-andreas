//! Weapon visuals: the weapon model in the ped's hand (`CPed::AddWeaponModel`,
//! `RenderWeaponPedsForPC`: the clump's root takes the right-hand bone matrix; twin pistols
//! draw it again on the left hand), the model gun flash (`SetGunFlashAlpha`),
//! `CBulletTraces::Render`, and the HUD bits (`CHud::DrawCrossHairs` third-person branch,
//! `DrawWeaponIcon`, `DrawAmmo`).

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    transform::TransformSystems,
};
use sa_formats::{dff, ide, txd};
use sa_physics::{ped::PedLogic, weapon::wf};

use crate::{
    camera::{CHAIR_X, CamMode, SaCam},
    player::{GameRoot, Ped, frame_transform},
    saphys::{SaPhys, SaPhysExt, SaSync},
    stream::{convert_texture, make_image},
    world::{WorldRes, g2b},
};

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (load_weapon_defs, setup_hud, setup_traces))
            .add_systems(
                PostUpdate,
                (update_weapon_model, draw_traces, update_hud, sync_projectiles, rocket_view)
                    .after(SaSync)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// default.ide `weap` entries: model id → (dff, txd).
#[derive(Resource, Default)]
struct WeaponDefs(HashMap<i32, (String, String)>);

fn load_weapon_defs(mut commands: Commands, root: Res<GameRoot>) {
    let mut defs = HashMap::new();
    match std::fs::read(root.0.join("data/default.ide")) {
        Ok(text) => match ide::parse(&String::from_utf8_lossy(&text)) {
            Ok(ide) => {
                for w in ide.weapons {
                    defs.insert(w.id as i32, (w.model, w.txd));
                }
            }
            Err(e) => warn!("default.ide: {e:#}"),
        },
        Err(e) => warn!("default.ide: {e}"),
    }
    commands.insert_resource(WeaponDefs(defs));
}

/// A loaded weapon model.
struct WeaponModel {
    clump: dff::Clump,
    textures: HashMap<String, (Handle<Image>, bool)>,
    icon: Option<Handle<Image>>,
}

#[derive(Default)]
struct Cache(HashMap<i32, Option<WeaponModel>>);

/// The weapon object currently in the player's hand.
#[derive(Component)]
struct HeldWeapon {
    model: i32,
    left: bool,
}

/// Marks weapon clumps that are not in a hand (projectiles).
#[derive(Component)]
struct Loose;

/// The gunflash atomic of a held weapon: (material, base local transform).
#[derive(Component)]
struct GunFlash {
    material: Handle<StandardMaterial>,
    base: Transform,
    left: bool,
}

fn load_model(world: &WorldRes, images: &mut Assets<Image>, name: &str, txd_name: &str) -> Option<WeaponModel> {
    let clump = dff::parse(world.0.file(&format!("{name}.dff"))?).ok()?;
    let mut textures = HashMap::new();
    if let Some(data) = world.0.file(&format!("{txd_name}.txd")) {
        for t in txd::parse(data).ok()?.into_iter().filter_map(|t| convert_texture(t, false)) {
            let alpha = t.alpha;
            textures.insert(t.name.to_ascii_lowercase(), (images.add(make_image(t)), alpha));
        }
    }
    let icon = textures.get(&format!("{name}icon")).map(|t| t.0.clone());
    Some(WeaponModel { clump, textures, icon })
}

fn geometry_meshes(geo: &dff::Geometry) -> Vec<(usize, Mesh)> {
    let mut out = Vec::new();
    for mi in 0..geo.materials.len() {
        let mut remap = vec![u32::MAX; geo.positions.len()];
        let (mut pos, mut nrm, mut uv, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for t in geo.triangles.iter().filter(|t| t.material as usize == mi) {
            for &v in &t.v {
                let v = v as usize;
                if v >= remap.len() {
                    continue;
                }
                if remap[v] == u32::MAX {
                    remap[v] = pos.len() as u32;
                    pos.push(geo.positions[v]);
                    nrm.push(geo.normals.get(v).copied().unwrap_or([0.0, 0.0, 1.0]));
                    uv.push(geo.uvs.first().and_then(|u| u.get(v)).copied().unwrap_or([0.0; 2]));
                }
                idx.push(remap[v]);
            }
        }
        if idx.len() < 3 {
            continue;
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nrm);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_indices(Indices::U32(idx));
        out.push((mi, mesh));
    }
    out
}

/// Spawn the weapon clump under `parent` (a hand bone) with the root at `root_tf`.
#[allow(clippy::too_many_arguments)]
fn spawn_weapon(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    wm: &WeaponModel,
    parent: Entity,
    root_tf: Transform,
    model: i32,
    left: bool,
) {
    let c = &wm.clump;
    let frames: Vec<Entity> = c
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let tf = if f.parent < 0 { root_tf } else { frame_transform(f) };
            let mut e = commands.spawn((tf, Visibility::default(), Name::new(f.name.clone())));
            if i == 0 || f.parent < 0 {
                e.insert(HeldWeapon { model, left });
            }
            e.id()
        })
        .collect();
    for (i, f) in c.frames.iter().enumerate() {
        let p = if f.parent >= 0 { frames[f.parent as usize] } else { parent };
        commands.entity(p).add_child(frames[i]);
    }
    for a in &c.atomics {
        let Some(geo) = c.geometries.get(a.geometry as usize) else { continue };
        let frame = a.frame as usize;
        let is_flash = c.frames[frame].name.eq_ignore_ascii_case("gunflash");
        for (mi, mesh) in geometry_meshes(geo) {
            let mat = &geo.materials[mi];
            let tex = mat.texture.as_ref().and_then(|t| wm.textures.get(&t.name.to_ascii_lowercase()));
            let col = mat.color;
            let material = materials.add(StandardMaterial {
                base_color: if is_flash {
                    Color::srgba(1.0, 1.0, 1.0, 0.0)
                } else {
                    Color::srgba_u8(col[0], col[1], col[2], col[3])
                },
                base_color_texture: tex.map(|t| t.0.clone()),
                alpha_mode: if is_flash {
                    AlphaMode::Blend
                } else if tex.is_some_and(|t| t.1) {
                    AlphaMode::Mask(20.0 / 255.0)
                } else {
                    AlphaMode::Opaque
                },
                unlit: is_flash,
                perceptual_roughness: 0.7,
                double_sided: true,
                cull_mode: None,
                ..default()
            });
            let vis = if is_flash { Visibility::Hidden } else { Visibility::Inherited };
            let id = commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone()), NoFrustumCulling, vis)).id();
            if is_flash {
                commands.entity(frames[frame]).insert(GunFlash {
                    material: material.clone(),
                    base: frame_transform(&c.frames[frame]),
                    left,
                });
            }
            commands.entity(frames[frame]).add_child(id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn update_weapon_model(
    mut commands: Commands,
    world: Res<WorldRes>,
    defs: Option<Res<WeaponDefs>>,
    sa: Res<SaPhys>,
    mut cache: Local<Cache>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    ped: Single<&Ped>,
    held: Query<(Entity, &HeldWeapon, &ChildOf)>,
    loose: Query<(), With<Loose>>,
    mut flashes: Query<(&GunFlash, &mut Transform, &Children)>,
    mut vis: Query<&mut Visibility>,
) {
    let Some(defs) = defs else { return };
    let Some(logic) = sa.logic::<PedLogic>(ped.sa) else { return };
    let t = &logic.tasks;
    let model = t.weapon_model;
    let twin = t.info_of(t.active_weapon().ty).is_some_and(|i| i.has(wf::TWIN_PISTOL));
    let current: Vec<(Entity, i32, bool)> =
        held.iter().filter(|(_, _, p)| !loose.contains(p.parent())).map(|(e, h, _)| (e, h.model, h.left)).collect();
    let want_left = model >= 0 && twin;
    let ok = current.iter().all(|&(_, m, _)| m == model)
        && current.iter().any(|&(_, _, l)| !l) == (model >= 0)
        && current.iter().any(|&(_, _, l)| l) == want_left;
    if !ok {
        for (e, _, _) in &current {
            commands.entity(*e).despawn();
        }
        if model >= 0 {
            let entry = cache.0.entry(model).or_insert_with(|| {
                let (name, txd_name) = defs.0.get(&model)?;
                load_model(&world, &mut images, name, txd_name)
            });
            if let (Some(wm), Some(clump)) = (entry.as_ref(), logic.clump.as_deref()) {
                let bone = |tag: i32| clump.frame_of_tag(tag).and_then(|k| ped.node_frames.get(k)).and_then(|&f| ped.bones.get(f)).copied();
                if let Some(rh) = bone(24) {
                    spawn_weapon(&mut commands, &mut meshes, &mut materials, wm, rh, Transform::IDENTITY, model, false);
                }
                if want_left {
                    if let Some(lh) = bone(34) {
                        // RwMatrixRotate(X, 180°) then RwMatrixTranslate((0.04, -0.05, 0)), both pre-concatenated.
                        let r = Quat::from_rotation_x(std::f32::consts::PI);
                        let tf = Transform { rotation: r, translation: r * Vec3::new(0.04, -0.05, 0.0), ..default() };
                        spawn_weapon(&mut commands, &mut meshes, &mut materials, wm, lh, tf, model, true);
                    }
                }
            }
        }
        return;
    }
    // Gun flash: alpha = min(raw * 350 / 10000, 255), rolled about the barrel (X).
    for (f, mut tf, children) in &mut flashes {
        let raw = t.gun_flash[f.left as usize].0 as i32;
        let a = if raw <= 0 { 0 } else { (raw * 350 / 10000).min(255) };
        if let Some(mut m) = materials.get_mut(&f.material) {
            m.base_color = Color::srgba(1.0, 1.0, 1.0, a as f32 / 255.0);
        }
        tf.rotation = f.base.rotation * Quat::from_rotation_x(t.gun_flash_roll.to_radians());
        for c in children.iter() {
            if let Ok(mut v) = vis.get_mut(c) {
                *v = if a > 0 { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
    }
}

// ------------------------------------------------------------------ projectiles

/// The visual of `CProjectileInfo` slot `.0` (body `.1`).
#[derive(Component)]
struct ProjectileVis(usize, sa_physics::world::EntityId);

/// Spawn / move / despawn the models of the world's projectiles; also give the world the
/// projectile models' bounds (AddProjectile's sphere = 0.75 · bound radius).
#[allow(clippy::too_many_arguments)]
fn sync_projectiles(
    mut commands: Commands,
    world: Res<WorldRes>,
    defs: Option<Res<WeaponDefs>>,
    mut sa: ResMut<SaPhys>,
    mut cache: Local<Cache>,
    mut bounds_done: Local<bool>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut vis: Query<(Entity, &ProjectileVis, &mut Transform)>,
) {
    let Some(defs) = defs else { return };
    if !*bounds_done {
        *bounds_done = true;
        for model in [342, 343, 344, 345, 363] {
            let entry = cache.0.entry(model).or_insert_with(|| {
                let (name, txd_name) = defs.0.get(&model)?;
                load_model(&world, &mut images, name, txd_name)
            });
            if let Some(wm) = entry {
                let pts: Vec<Vec3> = wm.clump.geometries.iter().flat_map(|g| g.positions.iter().map(|p| Vec3::from(*p))).collect();
                if let (Some(lo), Some(hi)) = (pts.iter().copied().reduce(Vec3::min), pts.iter().copied().reduce(Vec3::max)) {
                    let c = (lo + hi) * 0.5;
                    let r = pts.iter().map(|p| (*p - c).length()).fold(0.0, f32::max);
                    sa.world.projectiles.model_bounds.insert(model, (c, r));
                }
            }
        }
    }
    let infos: Vec<(usize, sa_physics::world::EntityId, sa_physics::physical::Matrix, i32)> = sa
        .world
        .projectiles
        .infos
        .iter()
        .enumerate()
        .filter(|(_, p)| p.active)
        .filter_map(|(i, p)| {
            let id = p.body?;
            let b = sa.world.body(id)?;
            let model = b.logic.as_any().downcast_ref::<sa_physics::projectile::ProjectileLogic>()?.model;
            Some((i, id, b.phys.matrix, model))
        })
        .collect();
    let mut have = Vec::new();
    for (e, pv, mut tf) in &mut vis {
        match infos.iter().find(|(i, id, _, _)| *i == pv.0 && *id == pv.1) {
            Some((_, _, m, _)) => {
                *tf = crate::saphys::transform_from_gta(m);
                have.push(pv.0);
            }
            None => commands.entity(e).despawn(),
        }
    }
    for (i, id, m, model) in infos {
        if have.contains(&i) {
            continue;
        }
        let entry = cache.0.entry(model).or_insert_with(|| {
            let (name, txd_name) = defs.0.get(&model)?;
            load_model(&world, &mut images, name, txd_name)
        });
        let Some(wm) = entry.as_ref() else { continue };
        // The model is authored in GTA space: one -90° X turn into Bevy space.
        let root = commands
            .spawn((crate::saphys::transform_from_gta(&m), Visibility::default(), ProjectileVis(i, id)))
            .id();
        let inner = commands.spawn((Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)), Visibility::default())).id();
        commands.entity(root).add_child(inner);
        spawn_weapon(&mut commands, &mut meshes, &mut materials, wm, inner, Transform::IDENTITY, model, false);
        commands.entity(inner).insert(Loose);
    }
}

/// In the 1st-person rocket camera the player is not drawn (only the crosshair).
fn rocket_view(cam: Res<SaCam>, mut ped: Query<&mut Visibility, With<Ped>>) {
    let want = if cam.mode == CamMode::Rocket { Visibility::Hidden } else { Visibility::Inherited };
    for mut v in &mut ped {
        if *v != want {
            *v = want;
        }
    }
}

// ------------------------------------------------------------------ bullet traces

#[derive(Resource)]
struct Traces {
    entity: Entity,
    mesh: Handle<Mesh>,
}

fn empty_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn setup_traces(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mesh = meshes.add(empty_mesh());
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        fog_enabled: false,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let entity = commands
        .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material), Transform::default(), NoFrustumCulling))
        .id();
    commands.insert_resource(Traces { entity, mesh });
}

/// `CBulletTraces::Render` (0x723C10): camera-facing 6-vertex ribbons, opaque only at the
/// centre of the end point; the tail slides to the end and the width shrinks with time.
fn draw_traces(
    sa: Res<SaPhys>,
    traces: Option<Res<Traces>>,
    camera: Single<&GlobalTransform, With<Camera3d>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tfs: Query<&mut Transform>,
) {
    let Some(traces) = traces else { return };
    let origin = camera.translation();
    let cam = Vec3::from(crate::world::b2g(origin));
    let now = sa.world.now_ms;
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::<u32>::new());
    let yellow = Color::srgb_u8(255, 255, 128).to_linear();
    for e in sa.world.bullet_traces.traces.iter().flatten() {
        let v = (e.start - cam).normalize_or_zero();
        let d = e.end - e.start;
        let len = d.length();
        let d = d.normalize_or_zero();
        let side = v.cross(d).normalize_or_zero();
        let f = 1.0 - now.wrapping_sub(e.created_ms) as f32 / e.life_ms.max(1) as f32;
        let a = (e.alpha as f32 * f) as u8;
        let s = side * (f * e.width);
        let p = e.end - d * (len * f);
        let base = pos.len() as u32;
        for (q, alpha) in [(p, 0), (p + s, 0), (p - s, 0), (e.end, a), (e.end + s, 0), (e.end - s, 0)] {
            pos.push((g2b(q.to_array()) - origin).to_array());
            col.push([yellow.red, yellow.green, yellow.blue, alpha as f32 / 255.0]);
        }
        idx.extend([4, 1, 3, 1, 0, 3, 0, 2, 3, 3, 2, 5].map(|i| base + i));
    }
    if let Ok(mut tf) = tfs.get_mut(traces.entity) {
        tf.translation = origin;
    }
    let Some(mut m) = meshes.get_mut(&traces.mesh) else { return };
    if idx.is_empty() {
        *m = empty_mesh();
        return;
    }
    let n = pos.len();
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    m.insert_indices(Indices::U32(idx));
}

// ------------------------------------------------------------------ HUD

#[derive(Component)]
struct CrossQuad(u8);

#[derive(Component)]
struct CrossDot;

#[derive(Component)]
struct RocketQuad(u8);

#[derive(Component)]
struct WeaponIcon;

#[derive(Component)]
struct AmmoText;

fn setup_hud(mut commands: Commands, root: Res<GameRoot>, mut images: ResMut<Assets<Image>>) {
    let site = std::fs::read(root.0.join("models/hud.txd"))
        .ok()
        .and_then(|d| txd::parse(&d).ok())
        .and_then(|t| t.into_iter().find(|t| t.name.eq_ignore_ascii_case("siteM16")))
        .and_then(|t| convert_texture(t, false))
        .map(|t| images.add(make_image(t)));
    let rocket = std::fs::read(root.0.join("models/hud.txd"))
        .ok()
        .and_then(|d| txd::parse(&d).ok())
        .and_then(|t| t.into_iter().find(|t| t.name.eq_ignore_ascii_case("siterocket")))
        .and_then(|t| convert_texture(t, false))
        .map(|t| images.add(make_image(t)));
    if let Some(rocket) = rocket {
        for q in 0..4u8 {
            commands.spawn((
                ImageNode { image: rocket.clone(), flip_x: q & 1 != 0, flip_y: q & 2 != 0, ..default() },
                Node { position_type: PositionType::Absolute, ..default() },
                Visibility::Hidden,
                RocketQuad(q),
            ));
        }
    }
    let Some(site) = site else {
        warn!("hud.txd siteM16 missing: no crosshair");
        return;
    };
    for q in 0..4u8 {
        commands.spawn((
            ImageNode { image: site.clone(), flip_x: q & 1 != 0, flip_y: q & 2 != 0, ..default() },
            Node { position_type: PositionType::Absolute, ..default() },
            Visibility::Hidden,
            CrossQuad(q),
        ));
    }
    commands.spawn((
        Node { position_type: PositionType::Absolute, width: px(2), height: px(2), ..default() },
        BackgroundColor(Color::WHITE),
        Visibility::Hidden,
        CrossDot,
    ));
    commands.spawn((
        ImageNode::default(),
        Node { position_type: PositionType::Absolute, ..default() },
        Visibility::Hidden,
        WeaponIcon,
    ));
    commands.spawn((
        Text::default(),
        TextFont { font_size: bevy::text::FontSize::Px(16.0), ..default() },
        TextColor(Color::srgb_u8(180, 25, 29)),
        TextLayout { justify: Justify::Center, ..default() },
        Node { position_type: PositionType::Absolute, ..default() },
        Visibility::Hidden,
        AmmoText,
    ));
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_hud(
    sa: Res<SaPhys>,
    cam: Res<SaCam>,
    window: Single<&Window>,
    world: Res<WorldRes>,
    defs: Option<Res<WeaponDefs>>,
    mut icons: Local<HashMap<i32, Option<Handle<Image>>>>,
    mut images: ResMut<Assets<Image>>,
    ped: Single<&Ped>,
    driving: Res<crate::vehicle::Driving>,
    mut quads: Query<(&CrossQuad, &mut Node, &mut Visibility), (Without<CrossDot>, Without<WeaponIcon>, Without<AmmoText>, Without<RocketQuad>)>,
    mut dot: Query<(&mut Node, &mut Visibility), (With<CrossDot>, Without<WeaponIcon>, Without<AmmoText>, Without<RocketQuad>)>,
    mut rockets: Query<(&RocketQuad, &mut Node, &mut Visibility), (Without<CrossQuad>, Without<CrossDot>, Without<WeaponIcon>, Without<AmmoText>)>,
    mut icon: Query<(&mut ImageNode, &mut Node, &mut Visibility), (With<WeaponIcon>, Without<AmmoText>, Without<CrossQuad>, Without<RocketQuad>)>,
    mut ammo: Query<(&mut Text, &mut TextFont, &mut Node, &mut Visibility), (With<AmmoText>, Without<CrossQuad>, Without<RocketQuad>)>,
) {
    let (w, h) = (window.width(), window.height());
    let logic = sa.logic::<PedLogic>(ped.sa);
    // Third-person crosshair: free aim in MODE_AIMWEAPON, not during the camera transition.
    let mut radius = None;
    if let Some(l) = logic {
        let t = &l.tasks;
        let wt = t.active_weapon().ty;
        if cam.mode == CamMode::AimWeapon && !cam.in_transition() && t.pd.free_aim && matches!(wt, 22..=33 | 37 | 38) {
            if let Some(info) = t.info_of(wt) {
                // CPlayerPed::GetWeaponRadiusOnScreen (0x609CD0).
                let a = 0.5 / info.accuracy;
                let r = if matches!(wt, 25..=27) {
                    a
                } else {
                    let k = (15.0 / info.weapon_range).min(1.0);
                    a * k * (0.5 * t.pd.attack_counter + 1.0)
                };
                let r = if t.ducking { r * 0.5 } else { r };
                radius = Some(r.max(0.2));
            }
        }
    }
    // Rocket launcher camera: siterocket, four 24×24 quarters pushed 20 units out from the centre.
    for (q, mut node, mut vis) in &mut rockets {
        if cam.mode != CamMode::Rocket {
            *vis = Visibility::Hidden;
            continue;
        }
        let (qw, qh) = (w / 640.0 * 24.0, h / 448.0 * 24.0);
        let (ox, oy) = (w / 640.0 * 20.0, h / 448.0 * 20.0);
        let x = (w / 2.0).floor() + if q.0 & 1 != 0 { qw * 0.5 + ox } else { -(qw * 0.5 + ox) };
        let y = (h / 2.0).floor() + if q.0 & 2 != 0 { qh * 0.5 + oy } else { -(qh * 0.5 + oy) };
        node.left = px(x - qw * 0.5);
        node.top = px(y - qh * 0.5);
        node.width = px(qw);
        node.height = px(qh);
        *vis = Visibility::Inherited;
    }
    let (cx, cy) = (w * CHAIR_X, h * 0.4);
    for (q, mut node, mut vis) in &mut quads {
        let Some(r) = radius else {
            *vis = Visibility::Hidden;
            continue;
        };
        let qw = w / 640.0 * 64.0 * r;
        let qh = h / 448.0 * 64.0 * r;
        node.left = px(cx - qw * 0.5 + if q.0 & 1 != 0 { qw * 0.5 } else { 0.0 });
        node.top = px(cy - qh * 0.5 + if q.0 & 2 != 0 { qh * 0.5 } else { 0.0 });
        node.width = px(qw * 0.5);
        node.height = px(qh * 0.5);
        *vis = Visibility::Inherited;
    }
    if let Ok((mut node, mut vis)) = dot.single_mut() {
        *vis = if radius == Some(0.2) { Visibility::Inherited } else { Visibility::Hidden };
        node.left = px(cx - 1.0);
        node.top = px(cy - 1.0);
    }

    // Weapon icon (497, 20) 47x58 and ammo (520.5, 63) in 640x448 units.
    let sx = w / 640.0;
    let sy = h / 448.0;
    let mut shown = false;
    if let (Some(l), Some(defs), None) = (logic, defs, driving.0) {
        let t = &l.tasks;
        let wpn = *t.active_weapon();
        if let Some(info) = t.infos.as_deref().map(|i| i.get(wpn.ty, 1)) {
            let model = info.model1;
            if model > 0 {
                let handle = icons
                    .entry(model)
                    .or_insert_with(|| {
                        let (name, txd_name) = defs.0.get(&model)?;
                        load_model(&world, &mut images, name, txd_name)?.icon
                    })
                    .clone();
                if let (Some(hd), Ok((mut img, mut node, mut vis))) = (handle, icon.single_mut()) {
                    img.image = hd;
                    node.left = px(w - (sx * 32.0 + w * 0.173_430_46));
                    node.top = px(sy * 20.0);
                    node.width = px(sx * 47.0);
                    node.height = px(sy * 58.0);
                    *vis = Visibility::Inherited;
                    shown = true;
                }
            }
            let clip = t.info_of(wpn.ty).map_or(0, |i| i.ammo_clip);
            if let Ok((mut text, mut font, mut node, mut vis)) = ammo.single_mut() {
                let show = model > 0 && info.slot > 1 && info.fire_type != sa_physics::weapon::fire::USE;
                text.0 = if 1 < clip && clip < 1000 {
                    format!("{}-{}", (wpn.total_ammo - wpn.ammo_in_clip).min(9999), wpn.ammo_in_clip)
                } else {
                    format!("{}", wpn.total_ammo)
                };
                font.font_size = bevy::text::FontSize::Px(sy * 14.0);
                node.left = px(w - (w * 0.173_43 + sx * 32.0) + sx * 47.0 * 0.5 - sx * 30.0);
                node.width = px(sx * 60.0);
                node.top = px(sy * 63.0);
                *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
    }
    if !shown {
        if let Ok((_, _, mut vis)) = icon.single_mut() {
            *vis = Visibility::Hidden;
        }
    }
    if driving.0.is_some() {
        if let Ok((_, _, _, mut vis)) = ammo.single_mut() {
            *vis = Visibility::Hidden;
        }
    }
}
