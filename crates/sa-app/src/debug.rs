//! Debug UI (Dear ImGui via bevy_mod_imgui). F1 toggles it (`SA_DEBUG_UI=1` opens it at start).
//!
//! Panels: world stats, time & weather (CClock / CWeather), the current vehicle's
//! damage and spawning, FX (quality, toggles, explosions and fires at the player).

use bevy::prelude::*;
use bevy_mod_imgui::prelude::*;
use sa_physics::{
    automobile::Automobile,
    damage::DamageManager,
    effects::ExplosionType,
    physical::Status,
    weather::WEATHER_NAMES,
};

use crate::{
    fx::Fx,
    player::Ped,
    saphys::SaPhys,
    stream::Streamer,
    vehicle::{Driving, SpawnQueue, Vehicle, VehicleModels},
    world::b2g,
};

pub struct DebugPlugin;

impl Plugin for DebugPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ImguiPlugin { ini_filename: Some("sa-rs-imgui.ini".into()), ..default() })
            .init_resource::<DebugUi>()
            .add_systems(Update, (toggle, ui).chain());
    }
}

/// Debug switches other systems read, plus the UI's own state.
#[derive(Resource)]
pub struct DebugUi {
    pub open: bool,
    /// ImGui wants the mouse / keyboard this frame (game input should ignore them).
    pub capture_mouse: bool,
    pub capture_keyboard: bool,
    pub heat_haze: bool,
    pub shadows: bool,
    pub rain_streaks: bool,
    pub sky: bool,
    pub fog: bool,
    pub colour_filter: bool,
    /// None = the PC time cycle's DirMult (0); Some = forced directional multiplier.
    pub dir_mult_override: Option<f32>,
    /// FrontEnd brightness (256 = neutral).
    pub brightness: i32,
    spawn_idx: usize,
    weather_idx: usize,
    explosion_idx: usize,
    hour: i32,
    minute: i32,
}

impl Default for DebugUi {
    fn default() -> Self {
        Self {
            // SA_DEBUG_UI=1 starts with the window open.
            open: std::env::var("SA_DEBUG_UI").is_ok(),
            capture_mouse: false,
            capture_keyboard: false,
            heat_haze: true,
            shadows: true,
            rain_streaks: true,
            sky: true,
            fog: true,
            colour_filter: true,
            dir_mult_override: None,
            brightness: 256,
            spawn_idx: 0,
            weather_idx: 0,
            explosion_idx: 4,
            hour: 12,
            minute: 0,
        }
    }
}

const EXPLOSIONS: [(&str, ExplosionType); 13] = [
    ("GRENADE", ExplosionType::Grenade),
    ("MOLOTOV", ExplosionType::Molotov),
    ("ROCKET", ExplosionType::Rocket),
    ("WEAK_ROCKET", ExplosionType::WeakRocket),
    ("CAR", ExplosionType::Car),
    ("QUICK_CAR", ExplosionType::QuickCar),
    ("BOAT", ExplosionType::Boat),
    ("AIRCRAFT", ExplosionType::Aircraft),
    ("MINE", ExplosionType::Mine),
    ("OBJECT", ExplosionType::Object),
    ("TANK_FIRE", ExplosionType::TankFire),
    ("SMALL", ExplosionType::Small),
    ("RC_VEHICLE", ExplosionType::RcVehicle),
];

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut dbg: ResMut<DebugUi>) {
    if keys.just_pressed(KeyCode::F1) {
        dbg.open = !dbg.open;
    }
}

#[allow(clippy::too_many_arguments)]
fn ui(
    mut ctx: NonSendMut<ImguiContext>,
    mut dbg: ResMut<DebugUi>,
    mut sa: ResMut<SaPhys>,
    fx: Option<ResMut<Fx>>,
    time: Res<Time>,
    st: Res<Streamer>,
    models: Option<Res<VehicleModels>>,
    mut spawn: ResMut<SpawnQueue>,
    driving: Res<Driving>,
    cars: Query<(Entity, &Vehicle, &Transform)>,
    ped_q: Single<(&Transform, &Ped)>,
) {
    let (ped, ped_c) = *ped_q;
    let ui = ctx.ui();
    let io = ui.io();
    dbg.capture_mouse = dbg.open && io.want_capture_mouse;
    dbg.capture_keyboard = dbg.open && io.want_capture_keyboard;
    if !dbg.open {
        return;
    }
    let dbg = &mut *dbg;
    let player = Vec3::from(b2g(ped.translation));
    ui.window("sa-rs debug (F1)")
        .size([380.0, 620.0], Condition::FirstUseEver)
        .position([10.0, 140.0], Condition::FirstUseEver)
        .build(|| {
            // ---------------------------------------------------------------- world
            if ui.collapsing_header("World", TreeNodeFlags::DEFAULT_OPEN) {
                let s = st.stats;
                ui.text(format!("fps {:.0}  frame {}", 1.0 / time.delta_secs().max(1e-4), sa.world.frame));
                ui.text(format!("player {:.1} {:.1} {:.1}", player.x, player.y, player.z));
                ui.text(format!("instances {}  pending {}  models {}", s.spawned, s.pending, s.models_ready));
                ui.text(format!("SA bodies {}", sa.world.body_ids().len()));
            }

            // ---------------------------------------------------------------- time & weather
            if ui.collapsing_header("Time & weather", TreeNodeFlags::DEFAULT_OPEN) {
                let w = &mut sa.world;
                ui.text(format!(
                    "{:02}:{:02}:{:02}  day {} month {}",
                    w.clock.hours, w.clock.minutes, w.clock.seconds, w.clock.days, w.clock.month
                ));
                ui.slider("hour", 0, 23, &mut dbg.hour);
                ui.slider("minute", 0, 59, &mut dbg.minute);
                if ui.button("Set time") {
                    let now = w.now_ms;
                    w.clock.set(now, dbg.hour as u8, dbg.minute as u8);
                }
                let mut ms = w.clock.ms_per_minute as i32;
                if ui.slider("ms per game minute", 50, 10000, &mut ms) {
                    w.clock.ms_per_minute = ms.max(1) as u32;
                }
                ui.separator();
                let wt = &w.weather;
                ui.text(format!(
                    "{} -> {}  ({:.0}%)",
                    WEATHER_NAMES[wt.old_type as usize],
                    WEATHER_NAMES[wt.new_type as usize],
                    wt.interpolation * 100.0
                ));
                ui.text(format!(
                    "region {}  list idx {}  forced {}",
                    wt.region,
                    wt.type_in_list,
                    if wt.forced_type >= 0 { WEATHER_NAMES[wt.forced_type as usize] } else { "-" }
                ));
                ui.combo_simple_string("weather", &mut dbg.weather_idx, &WEATHER_NAMES[..20]);
                if ui.button("Force now") {
                    w.weather.force_now(dbg.weather_idx as i16);
                }
                ui.same_line();
                if ui.button("Release") {
                    w.weather.release();
                }
                let wt = &w.weather;
                ui.text(format!("rain {:.2}  wet roads {:.2}  sandstorm {:.2}", wt.rain, wt.wet_roads, wt.sandstorm));
                ui.text(format!(
                    "wind {:.2}  dir {:.2} {:.2} {:.2}",
                    wt.wind, wt.wind_dir.x, wt.wind_dir.y, wt.wind_dir.z
                ));
                ui.text(format!(
                    "fog {:.2}  clouds {:.2}  extra sunny {:.2}  rainbow {:.2}",
                    wt.foggyness, wt.cloud_coverage, wt.extra_sunnyness, wt.rainbow
                ));
                ui.text(format!(
                    "heat haze {:.2}  fx {:.2}  lightning {}",
                    wt.heat_haze, wt.heat_haze_fx_control, wt.lightning_flash
                ));
                ui.text(format!("DN balance {:.2}", w.clock.dn_balance()));
            }

            // ---------------------------------------------------------------- time cycle
            if ui.collapsing_header("Time cycle", TreeNodeFlags::empty()) {
                if let Some(tc) = sa.world.timecycle.as_ref() {
                    let c = &tc.current;
                    let rgb = |v: [f32; 3]| format!("{:.0} {:.0} {:.0}", v[0], v[1], v[2]);
                    ui.text(format!(
                        "amb {:.2} {:.2} {:.2}  obj {:.2} {:.2} {:.2}",
                        c.ambient.x, c.ambient.y, c.ambient.z, c.ambient_obj.x, c.ambient_obj.y, c.ambient_obj.z
                    ));
                    ui.text(format!("sky top {}  bottom {}", rgb(c.sky_top), rgb(c.sky_bottom)));
                    ui.text(format!("sun core {}  corona {}  size {:.0}", rgb(c.sun_core), rgb(c.sun_corona), c.sun_size));
                    ui.text(format!("far clip {:.0}  fog start {:.0}  dir mult {:.2}", c.far_clip, c.fog_start, c.dir_mult));
                    let p1 = c.post_fx1;
                    let p2 = c.post_fx2;
                    ui.text(format!("postfx1 {:.0} {:.0} {:.0} a{:.0}", p1[0], p1[1], p1[2], p1[3]));
                    ui.text(format!("postfx2 {:.0} {:.0} {:.0} a{:.0}", p2[0], p2[1], p2[2], p2[3]));
                    let s = tc.vector_to_sun;
                    ui.text(format!("sun dir {:.2} {:.2} {:.2}", s.x, s.y, s.z));
                }
                ui.checkbox("sky", &mut dbg.sky);
                ui.same_line();
                ui.checkbox("fog", &mut dbg.fog);
                ui.same_line();
                ui.checkbox("colour filter", &mut dbg.colour_filter);
                let mut force = dbg.dir_mult_override.is_some();
                if ui.checkbox("force directional light", &mut force) {
                    dbg.dir_mult_override = force.then_some(1.0);
                }
                if let Some(d) = dbg.dir_mult_override.as_mut() {
                    ui.slider("dir mult", 0.0, 2.0, d);
                }
                ui.slider("brightness", 0, 512, &mut dbg.brightness);
            }

            // ---------------------------------------------------------------- vehicle
            if ui.collapsing_header("Vehicle", TreeNodeFlags::DEFAULT_OPEN) {
                let target = driving.0.and_then(|e| cars.get(e).ok()).or_else(|| {
                    cars.iter().min_by(|a, b| {
                        let d = |t: &Transform| t.translation.distance_squared(ped.translation);
                        d(a.2).total_cmp(&d(b.2))
                    })
                });
                match target {
                    Some((_, v, _)) => {
                        let id = v.sa;
                        let Some(b) = sa.world.body_mut(id) else { return };
                        let (phys, logic) = (&mut b.phys, &mut b.logic);
                        if let Some(car) = logic.as_any_mut().downcast_mut::<Automobile>() {
                            let d = &mut car.damage;
                            ui.text(format!(
                                "{} ({})  {:?}  {:.0} km/h",
                                v.name,
                                if driving.0.is_some() { "driving" } else { "nearest" },
                                phys.status,
                                v.speed.abs() * 3.6
                            ));
                            ui.text(format!(
                                "engine {}  burn {:.0} ms  bomb {} ms",
                                d.dm.engine, d.burn_timer_ms, d.bomb_timer_ms
                            ));
                            ui.text(format!("wheels {:?}  doors {:?}", d.dm.wheels, d.dm.doors));
                            ui.text(format!("panels {:?}  lights {:?}", d.dm.panels, d.dm.lights));
                            let mut hp = d.health;
                            if ui.slider("health", 0.0, 1000.0, &mut hp) {
                                d.health = hp;
                            }
                            for (label, hp) in [("1000", 1000.0), ("640 smoke", 640.0), ("400", 400.0), ("240 fire", 240.0)] {
                                if ui.button(label) {
                                    d.health = hp;
                                }
                                ui.same_line();
                            }
                            ui.new_line();
                            ui.text(format!(
                                "lights {}  lamps {:04b}",
                                if car.lights.on { "on" } else { "off" },
                                car.lights.render & 0xF
                            ));
                            ui.checkbox("siren", &mut car.lights.siren);
                            ui.same_line();
                            ui.checkbox("taxi light", &mut car.lights.taxi_light);
                            let mut force = car.lights.force as usize;
                            if ui.combo_simple_string("lights", &mut force, &["auto", "force off", "force on"]) {
                                car.lights.force = force as u8;
                            }
                            let wrecked = phys.status == Status::Wrecked;
                            if !wrecked {
                                if ui.button("Blow up") {
                                    d.blow_up(phys, None);
                                }
                                ui.same_line();
                                if ui.button("Repair") {
                                    // CAutomobile::Fix-like: undamaged state, doors shut.
                                    d.health = 1000.0;
                                    d.dm = DamageManager::default();
                                    for door in &mut d.doors {
                                        door.angle = 0.0;
                                        door.prev_angle = 0.0;
                                        door.ang_vel = 0.0;
                                    }
                                    d.burn_timer_ms = 0.0;
                                    d.bomb_timer_ms = 0;
                                    d.on_fire = false;
                                }
                            } else {
                                ui.text_disabled("wrecked");
                            }
                        }
                    }
                    None => ui.text_disabled("no vehicle"),
                }
                ui.separator();
                if let Some(models) = &models {
                    if !models.0.is_empty() {
                        dbg.spawn_idx = dbg.spawn_idx.min(models.0.len() - 1);
                        ui.combo_simple_string("model", &mut dbg.spawn_idx, &models.0);
                        if ui.button("Spawn (on foot)") {
                            spawn.0.push(models.0[dbg.spawn_idx].clone());
                        }
                    }
                }
            }

            // ---------------------------------------------------------------- weapons
            if ui.collapsing_header("Weapons", TreeNodeFlags::DEFAULT_OPEN) {
                use crate::saphys::SaPhysExt;
                use sa_physics::{ped::PedLogic, weapon::WEAPON_NAMES};
                if let Some(l) = sa.logic_mut::<PedLogic>(ped_c.sa) {
                    let t = &mut l.tasks;
                    let w = *t.active_weapon();
                    let skill = ["POOR", "STD", "PRO", "COP"][t.weapon_skill(w.ty).min(3) as usize];
                    ui.text(format!(
                        "{} ({skill})  clip {}  total {}  state {}",
                        WEAPON_NAMES[w.ty as usize],
                        w.ammo_in_clip,
                        w.total_ammo,
                        w.state
                    ));
                    ui.text(format!(
                        "move state {}  group {}  aim {}  gun task {}",
                        t.move_state,
                        t.anim_group,
                        t.pd.free_aim,
                        t.gun.as_ref().map_or("-".to_string(), |g| format!("{:?}", g.last_cmd))
                    ));
                    ui.text("LMB fire, RMB aim, wheel / Q / E switch");
                    for (i, &ty) in [22u32, 23, 24, 25, 26, 27, 28, 29, 32, 30, 31, 33, 38, 16, 17, 18, 39, 35, 36, 37, 41, 42].iter().enumerate() {
                        if i % 4 != 0 {
                            ui.same_line();
                        }
                        if ui.button(WEAPON_NAMES[ty as usize]) {
                            let slot = t.give_weapon(ty, 500);
                            t.pd.chosen_slot = slot;
                        }
                    }
                    let names = ["pistol", "silenced", "deagle", "shotgun", "sawnoff", "spas", "uzi", "mp5", "ak47", "m4"];
                    for (i, n) in names.iter().enumerate() {
                        ui.slider(format!("{n} skill"), 0.0, 1000.0, &mut t.skill_stats[i]);
                    }
                }
            }

            // ---------------------------------------------------------------- fx
            if ui.collapsing_header("Effects", TreeNodeFlags::DEFAULT_OPEN) {
                if let Some(mut fx) = fx {
                    ui.text(format!(
                        "particles {} / {}  systems {}",
                        fx.man.live_particles(),
                        sa_fx::MAX_PARTICLES,
                        fx.man.system_count()
                    ));
                    let mut q = fx.man.quality as i32;
                    if ui.slider("FX quality", 0, 3, &mut q) {
                        fx.man.quality = q as u8;
                    }
                }
                ui.checkbox("heat haze", &mut dbg.heat_haze);
                ui.same_line();
                ui.checkbox("shadows", &mut dbg.shadows);
                ui.same_line();
                ui.checkbox("rain streaks", &mut dbg.rain_streaks);
                let names: Vec<&str> = EXPLOSIONS.iter().map(|e| e.0).collect();
                ui.combo_simple_string("explosion", &mut dbg.explosion_idx, &names);
                let ahead = Vec3::from(b2g(ped.translation + ped.rotation * Vec3::NEG_Z * 8.0));
                if ui.button("Explode 8 m ahead") {
                    let kind = EXPLOSIONS[dbg.explosion_idx].1;
                    sa.world.add_explosion(None, None, kind, ahead, 0, -1.0, false);
                }
                ui.same_line();
                if ui.button("Fire 5 m ahead") {
                    let p = Vec3::from(b2g(ped.translation + ped.rotation * Vec3::NEG_Z * 5.0));
                    if let Some(z) = sa.world.find_ground_z(p + Vec3::Z * 2.0) {
                        sa.world.start_fire_at(Vec3::new(p.x, p.y, z), None, 7000, 100);
                    }
                }
                let fires = sa.world.active_fires();
                ui.text(format!("fires {fires}  lights {}", sa.world.effects.lights.len()));
            }
        });
}
