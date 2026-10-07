//! Souls combat mode for the player (not part of GTA SA; see sa-physics `souls`).
//!
//! F5 toggles it on foot. The data is exported locally from the user's own Elden Ring
//! (tools/er/er_export.py) into `er-data/` (or `SA_ER_DATA`): without it the mode is
//! unavailable.
//!
//! Keys: WASD move, Alt walk, Space tap = roll / backstep, hold = sprint, F jump, C / X
//! crouch, LMB light, Shift+LMB heavy (hold to charge), RMB guard / left-hand attack,
//! mouse-wheel click lock-on (flick the mouse to switch targets), 1 two-hand the right
//! weapon, 2 two-hand the left item, 3 next right weapon, 4 next left-hand item.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use sa_physics::ped::PedLogic;
use sa_physics::souls::{ActionDef, AirAttackDef, AirKind, Hit, Souls, SoulsData, SwapDef, WeaponInfo};
use serde_json::Value;

use crate::player::Ped;
use crate::saphys::{SaPhys, SaPhysExt};

pub struct SoulsPlugin;

impl Plugin for SoulsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoulsRes>()
            .add_systems(Startup, load)
            .add_systems(Update, (souls_input.after(crate::player::player_control).before(crate::saphys::SaStep), left_hand_props));
    }
}

#[derive(Resource, Default)]
pub struct SoulsRes(pub Option<Arc<SoulsData>>);

// ------------------------------------------------------------------ loading

fn f(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

fn pairs(v: &Value) -> Vec<(f32, f32)> {
    v.as_array().map_or(Vec::new(), |a| a.iter().map(|p| (f(&p[0]), f(&p[1]))).collect())
}

fn blade(v: &Value) -> Vec<[f32; 6]> {
    v.as_array().map_or(Vec::new(), |rows| rows.iter().map(|r| std::array::from_fn(|i| f(&r[i]))).collect())
}

fn action(v: &Value) -> ActionDef {
    let c = &v["cancel"];
    ActionDef {
        source: v["source"].as_str().unwrap_or_default().to_string(),
        total: f(&v["total"]),
        input_from: f(&v["input_from"]),
        input_dodge_from: f(&v["input_dodge_from"]),
        cancel_light: f(&c["light"]),
        cancel_heavy: f(&c["heavy"]),
        cancel_dodge: f(&c["dodge"]),
        cancel_jump: f(&c["jump"]),
        cancel_guard: f(&c["guard"]),
        cancel_move: f(&c["move"]),
        cancel_left: f(&c["left"]),
        iframes: (f(&v["iframes"][0]), f(&v["iframes"][1])),
        jump_frames: v["jump_frames"].as_bool().unwrap_or(false),
        hits: v["hits"].as_array().map_or(Vec::new(), |hs| {
            hs.iter()
                .map(|h| Hit {
                    from: f(&h["from"]),
                    to: f(&h["to"]),
                    mv: f(&h["mv"]),
                    guard_damage: f(&h["guard_damage"]),
                    stamina: f(&h["stamina"]),
                    stop: f(&h["stop"]),
                    radius: f(&h["radius"]),
                    blade: blade(&h["blade"]),
                })
                .collect()
        }),
        charge: v["charge"].as_array().map(|c| (f(&c[0]), f(&c[1]))),
        no_turn: pairs(&v["no_turn"]),
        turn: v["turn"].as_array().map_or(Vec::new(), |a| a.iter().map(|t| (f(&t[0]), f(&t[1]), f(&t[2]))).collect()),
        motion: v["motion"].as_array().map_or(Vec::new(), |a| a.iter().map(|m| [f(&m[0]), f(&m[1]), f(&m[2])]).collect()),
    }
}

fn read(dir: &std::path::Path) -> Result<SoulsData, String> {
    let json = std::fs::read(dir.join("actions.json")).map_err(|e| format!("actions.json: {e}"))?;
    let j: Value = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
    let sp = &j["speeds"];
    let mut d = SoulsData {
        walk_speed: f(&sp["WALK_SPEED"]),
        run_speed: f(&sp["RUN_SPEED"]),
        run_back_speed: f(&sp["RUN_BACK_SPEED"]),
        run_side_speed: f(&sp["RUN_SIDE_SPEED"]),
        sprint_speed: f(&sp["SPRINT_SPEED"]),
        crouch_walk_speed: f(&sp["CROUCH_WALK_SPEED"]),
        crouch_run_speed: f(&sp["CROUCH_RUN_SPEED"]),
        max_stamina: f(&j["max_stamina"]),
        weapons: j["weapons"].as_array().map_or(Vec::new(), |ws| {
            ws.iter()
                .map(|w| WeaponInfo {
                    name: w["name"].as_str().unwrap_or_default().to_string(),
                    attack: f(&w["attack"]),
                    weight: f(&w["weight"]),
                    stance: [w["stance"][0].as_u64().unwrap_or(0) as u8, w["stance"][1].as_u64().unwrap_or(0) as u8],
                })
                .collect()
        }),
        base: HashMap::new(),
        attacks: HashMap::new(),
        ..Default::default()
    };
    if let Some(b) = j["base"].as_object() {
        for (k, v) in b {
            d.base.insert(k.clone(), action(v));
        }
    }
    for e in j["attacks"].as_array().into_iter().flatten() {
        let key = (e["weapon"].as_u64().unwrap_or(0) as usize, e["two_hand"].as_bool().unwrap_or(false), e["kind"].as_str().unwrap_or_default().to_string());
        d.attacks.insert(key, action(&e["def"]));
    }
    for e in j["air"].as_array().into_iter().flatten() {
        let kind = match &e["heavy"] {
            Value::Bool(true) => AirKind::Heavy,
            Value::Bool(false) => AirKind::Light,
            _ => AirKind::Paired,
        };
        let key = (e["weapon"].as_u64().unwrap_or(0) as usize, e["two_hand"].as_bool().unwrap_or(false), kind);
        d.air.insert(
            key,
            AirAttackDef {
                from: f(&e["from"]),
                to: f(&e["to"]),
                stamina: f(&e["stamina"]),
                source: e["source"].as_str().unwrap_or_default().to_string(),
                radius: f(&e["radius"]),
                blade: blade(&e["blade"]),
            },
        );
    }
    if let Some(s) = j["swaps"].as_object() {
        for (k, v) in s {
            d.swaps.insert(
                k.clone(),
                SwapDef {
                    start: v["start"].as_str().unwrap_or_default().to_string(),
                    end: v["end"].as_str().unwrap_or_default().to_string(),
                    start_len: f(&v["start_len"]),
                    end_len: f(&v["end_len"]),
                    apply: f(&v["apply"]),
                    free_from: f(&v["free_from"]),
                },
            );
        }
    }
    let anims = std::fs::read(dir.join("anims.bin")).map_err(|e| format!("anims.bin: {e}"))?;
    d.load_anims(&anims)?;
    Ok(d)
}

fn load(mut res: ResMut<SoulsRes>) {
    let dir = std::env::var("SA_ER_DATA").map(std::path::PathBuf::from).unwrap_or_else(|_| "er-data".into());
    match read(&dir) {
        Ok(d) => {
            info!("souls: {} actions, {} attacks, {} air attacks, {} clips", d.base.len(), d.attacks.len(), d.air.len(), d.clips.len());
            res.0 = Some(Arc::new(d));
        }
        Err(e) => info!("souls mode unavailable ({}): {e}", dir.display()),
    }
}

// ------------------------------------------------------------------ input

/// F5 toggles the mode; the keys are folded into the player's souls input.
#[allow(clippy::too_many_arguments)]
fn souls_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    res: Res<SoulsRes>,
    cam: Res<crate::camera::SaCam>,
    mut sa: ResMut<SaPhys>,
    mut flick: Local<f32>,
    ped: Single<&Ped>,
) {
    let id = ped.sa;
    let auto = std::env::var("SA_SOULS").is_ok();
    let toggle = keys.just_pressed(KeyCode::F5);
    let Some(l) = sa.logic_mut::<PedLogic>(id) else { return };
    if toggle || (auto && l.souls.is_none() && res.0.is_some() && l.tasks.health.alive()) {
        if l.souls.is_some() {
            l.souls = None;
            info!("souls mode off");
        } else if let Some(d) = &res.0 {
            // The selected slot (the drawn one lags behind while GTA's tasks are off).
            let ty = l.tasks.weapons.get(l.tasks.pd.chosen_slot).map_or(0, |w| w.ty);
            l.souls = Some(Box::new(Souls::new(d.clone(), l.cur_rot, ty)));
            info!("souls mode on");
        } else {
            info!("souls mode unavailable: no er-data");
        }
    }
    let Some(s) = l.souls.as_deref_mut() else { return };
    // Camera-relative movement (GTA world xy).
    let fwd = Vec2::new(cam.front.x, cam.front.y).normalize_or(Vec2::Y);
    let right = Vec2::new(fwd.y, -fwd.x);
    let mut mv = Vec2::ZERO;
    for (k, d) in [(KeyCode::KeyW, fwd), (KeyCode::KeyS, -fwd), (KeyCode::KeyD, right), (KeyCode::KeyA, -right)] {
        if keys.pressed(k) {
            mv += d;
        }
    }
    let i = &mut s.input;
    i.mv = mv.normalize_or_zero();
    i.walk = keys.pressed(KeyCode::AltLeft);
    let shift = keys.pressed(KeyCode::ShiftLeft);
    let lmb = buttons.pressed(MouseButton::Left);
    i.dodge.feed(keys.pressed(KeyCode::Space));
    i.jump.feed(keys.pressed(KeyCode::KeyF));
    i.light.feed(lmb && !shift);
    i.heavy.feed(lmb && shift);
    i.guard.feed(buttons.pressed(MouseButton::Right));
    i.crouch |= keys.just_pressed(KeyCode::KeyC) || keys.just_pressed(KeyCode::KeyX);
    i.lock |= buttons.just_pressed(MouseButton::Middle);
    i.two_hand_right |= keys.just_pressed(KeyCode::Digit1);
    i.two_hand_left |= keys.just_pressed(KeyCode::Digit2);
    i.next_weapon |= keys.just_pressed(KeyCode::Digit3);
    i.next_left |= keys.just_pressed(KeyCode::Digit4);
    // Locked on, a sideways flick of the mouse switches targets.
    *flick = (*flick - time.delta_secs()).max(0.0);
    if s.locked && *flick <= 0.0 && motion.delta.x.abs() > 25.0 {
        i.switch_target = motion.delta.x.signum() as i8;
        *flick = 0.35;
    }
    // SA_SOULSLOG=1: the peds fighting with the ER brain, every 2 s.
    if std::env::var("SA_SOULSLOG").is_ok() && sa.world.frame % 100 == 0 {
        let hp = sa.logic::<PedLogic>(id).map_or(0.0, |l| l.tasks.health.health);
        let n = sa.world.body_ids().into_iter().filter(|&e| sa.logic::<PedLogic>(e).is_some_and(|l| l.souls_enemy.is_some())).count();
        info!("souls: {n} enemies, player hp {hp:.0}");
    }
    if let Ok(demo) = std::env::var("SA_SOULS_DEMO") {
        let down = sa.world.now_ms % 2000 < 150;
        let lock_tick = sa.world.now_ms % 4000 < 20;
        let Some(s) = sa.logic_mut::<PedLogic>(id).and_then(|l| l.souls.as_deref_mut()) else { return };
        let i = &mut s.input;
        match demo.as_str() {
            "light" => i.light.feed(down),
            "heavy" => i.heavy.feed(down),
            "roll" => {
                i.mv = Vec2::Y;
                i.dodge.feed(down);
            }
            "jump" => {
                i.mv = Vec2::Y;
                i.jump.feed(down);
            }
            "jumpattack" => {
                i.mv = Vec2::Y;
                i.jump.feed(down);
                let t = sa.world.now_ms % 2000;
                let Some(s) = sa.logic_mut::<PedLogic>(id).and_then(|l| l.souls.as_deref_mut()) else { return };
                s.input.light.feed((300..450).contains(&t));
            }
            "guard" => i.guard.feed(true),
            "lock" => i.lock |= lock_tick,
            "hunt" => {
                let me = sa.world.body(id).map(|b| b.phys.matrix.pos).unwrap_or_default();
                let near = sa
                    .world
                    .body_ids()
                    .into_iter()
                    .filter(|&e| e != id)
                    .filter_map(|e| {
                        let b = sa.world.body(e)?;
                        let l = b.logic.as_any().downcast_ref::<PedLogic>()?;
                        l.tasks.health.alive().then_some(b.phys.matrix.pos)
                    })
                    .min_by(|a, b| a.distance(me).total_cmp(&b.distance(me)));
                let t = sa.world.now_ms;
                let Some(s) = sa.logic_mut::<PedLogic>(id).and_then(|l| l.souls.as_deref_mut()) else { return };
                if let Some(p) = near {
                    let to = (p - me).truncate();
                    s.input.mv = if to.length() > 2.0 { to.normalize() } else { Vec2::ZERO };
                    if to.length() < 12.0 && !s.locked {
                        s.input.lock = true;
                    }
                    s.input.light.feed(to.length() < 2.6 && t % 1500 < 150);
                }
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ left-hand props

#[derive(Component)]
struct SoulsProp(u8);

/// The shield and the torch in the left hand (GTA has no models for them).
#[allow(clippy::too_many_arguments)]
fn left_hand_props(
    mut commands: Commands,
    sa: Res<SaPhys>,
    ped: Single<&Ped>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    props: Query<(Entity, &SoulsProp)>,
) {
    let Some(l) = sa.logic::<PedLogic>(ped.sa) else { return };
    let want = l.souls.as_deref().and_then(|s| {
        let (shield, _, torch) = s.data.specials();
        match s.left_item() {
            Some(i) if i == shield => Some(1u8),
            Some(i) if i == torch => Some(2u8),
            _ => None,
        }
    });
    let have = props.iter().next().map(|(_, p)| p.0);
    if have == want {
        return;
    }
    for (e, _) in &props {
        commands.entity(e).despawn();
    }
    let Some(kind) = want else { return };
    let Some(clump) = l.clump.as_deref() else { return };
    let Some(hand) = clump.frame_of_tag(34).and_then(|k| ped.node_frames.get(k)).and_then(|&f| ped.bones.get(f)).copied() else { return };
    let wood = materials.add(StandardMaterial { base_color: Color::srgb(0.36, 0.24, 0.13), perceptual_roughness: 0.8, ..default() });
    let steel = materials.add(StandardMaterial { base_color: Color::srgb(0.75, 0.76, 0.78), metallic: 0.8, perceptual_roughness: 0.3, ..default() });
    let root = commands.spawn((SoulsProp(kind), Transform::default(), Visibility::default(), ChildOf(hand))).id();
    match kind {
        1 => {
            // A round-ish heater shield on the forearm side of the hand.
            commands.spawn((Mesh3d(meshes.add(Cuboid::new(0.04, 0.62, 0.48))), MeshMaterial3d(wood), Transform::from_xyz(0.0, 0.0, -0.06), ChildOf(root)));
            commands.spawn((Mesh3d(meshes.add(Sphere::new(0.07))), MeshMaterial3d(steel), Transform::from_xyz(0.0, 0.0, -0.1), ChildOf(root)));
        }
        _ => {
            let flame = materials.add(StandardMaterial { base_color: Color::srgb(1.0, 0.55, 0.15), emissive: LinearRgba::rgb(8.0, 3.5, 0.8), unlit: true, ..default() });
            commands.spawn((Mesh3d(meshes.add(Cylinder::new(0.025, 0.55))), MeshMaterial3d(wood), Transform::from_xyz(0.2, 0.0, 0.0).with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)), ChildOf(root)));
            commands.spawn((Mesh3d(meshes.add(Sphere::new(0.07))), MeshMaterial3d(flame), Transform::from_xyz(0.48, 0.0, 0.0), ChildOf(root)));
            commands.spawn((PointLight { color: Color::srgb(1.0, 0.6, 0.25), intensity: 60_000.0, range: 8.0, shadow_maps_enabled: false, ..default() }, Transform::from_xyz(0.5, 0.0, 0.0), ChildOf(root)));
        }
    }
}
