//! Souls combat mode for the player (not part of GTA SA; see sa-physics `souls`).
//!
//! F5 toggles it on foot. The data is exported locally from the user's own Elden Ring
//! (tools/er/er_export.py) into `er-data/` (or `SA_ER_DATA`): without it the mode is
//! unavailable. Keys: WASD move, Alt walk, Space tap = roll / hold = sprint, LMB light,
//! Shift+LMB heavy (hold to charge), RMB guard, mouse-wheel click = lock-on, Q / E switch
//! between fists and the melee weapon.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use sa_physics::ped::PedLogic;
use sa_physics::souls::{ActionDef, Hit, Souls, SoulsData, WeaponInfo};
use serde_json::Value;

use crate::player::Ped;
use crate::saphys::{SaPhys, SaPhysExt};

pub struct SoulsPlugin;

impl Plugin for SoulsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoulsRes>()
            .add_systems(Startup, load)
            .add_systems(Update, souls_input.after(crate::player::player_control).before(crate::saphys::SaStep));
    }
}

#[derive(Resource, Default)]
pub struct SoulsRes(pub Option<Arc<SoulsData>>);

fn f(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

fn pairs(v: &Value) -> Vec<(f32, f32)> {
    v.as_array().map_or(Vec::new(), |a| a.iter().map(|p| (f(&p[0]), f(&p[1]))).collect())
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
        cancel_guard: f(&c["guard"]),
        cancel_move: f(&c["move"]),
        iframes: (f(&v["iframes"][0]), f(&v["iframes"][1])),
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
                    blade: h["blade"].as_array().map_or(Vec::new(), |rows| {
                        rows.iter().map(|r| std::array::from_fn(|i| f(&r[i]))).collect()
                    }),
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
        max_stamina: f(&j["max_stamina"]),
        weapons: j["weapons"]
            .as_array()
            .map_or(Vec::new(), |ws| {
                ws.iter()
                    .map(|w| WeaponInfo {
                        name: w["name"].as_str().unwrap_or_default().to_string(),
                        attack: f(&w["attack"]),
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
    if let Some(a) = j["attacks"].as_array() {
        for e in a {
            let key = (e["weapon"].as_u64().unwrap_or(0) as usize, e["two_hand"].as_bool().unwrap_or(false), e["kind"].as_str().unwrap_or_default().to_string());
            d.attacks.insert(key, action(&e["def"]));
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
            info!("souls: {} actions, {} attacks, {} clips", d.base.len(), d.attacks.len(), d.clips.len());
            res.0 = Some(Arc::new(d));
        }
        Err(e) => info!("souls mode unavailable ({}): {e}", dir.display()),
    }
}

/// F5 toggles the mode; the keys are folded into the player's souls input.
#[allow(clippy::too_many_arguments)]
fn souls_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    res: Res<SoulsRes>,
    cam: Res<crate::camera::SaCam>,
    mut sa: ResMut<SaPhys>,
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
            l.souls = Some(Box::new(Souls::new(d.clone(), l.cur_rot)));
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
    if keys.pressed(KeyCode::KeyW) {
        mv += fwd;
    }
    if keys.pressed(KeyCode::KeyS) {
        mv -= fwd;
    }
    if keys.pressed(KeyCode::KeyD) {
        mv += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        mv -= right;
    }
    s.input.mv = mv.normalize_or_zero();
    s.input.walk = keys.pressed(KeyCode::AltLeft);
    let shift = keys.pressed(KeyCode::ShiftLeft);
    let lmb = buttons.pressed(MouseButton::Left);
    s.input.dodge.feed(keys.pressed(KeyCode::Space));
    s.input.light.feed(lmb && !shift);
    s.input.heavy.feed(lmb && shift);
    s.input.guard.feed(buttons.pressed(MouseButton::Right));
    s.input.lock_pressed |= buttons.just_pressed(MouseButton::Middle);
    if let Ok(demo) = std::env::var("SA_SOULS_DEMO") {
        let t = sa.world.now_ms % 2000;
        let Some(s) = sa.logic_mut::<PedLogic>(id).and_then(|l| l.souls.as_deref_mut()) else { return };
        let down = t < 150;
        match demo.as_str() {
            "light" => s.input.light.feed(down),
            "heavy" => s.input.heavy.feed(down),
            "roll" => {
                s.input.mv = Vec2::Y;
                s.input.dodge.feed(down);
            }
            "guard" => s.input.guard.feed(true),
            _ => {}
        }
    }
}
