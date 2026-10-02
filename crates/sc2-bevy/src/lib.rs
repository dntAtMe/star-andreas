//! StarCraft II units in the San Andreas world.
//!
//! The SC2 simulation (`sc2_api::sim`) runs on its own thread; this plugin
//! mirrors its unit snapshots into entities, interpolates them between game
//! steps, snaps them to the ground via Rapier ray casts, and turns RTS-style
//! input into SC2 commands.
//!
//! SC2 map cells map onto GTA metres around [`Sc2Settings::anchor`]; one cell
//! is [`Sc2Settings::scale`] metres. Bevy space follows sa-app: GTA (x, y, z)
//! is Bevy (x, z, -y).

mod control;
mod demo;
mod models;
mod units;

use std::{path::PathBuf, sync::Mutex};

use bevy::prelude::*;
use sc2_api::sim::{Event, MapInfo, SimConfig, SimHandle};

pub use control::Sc2Control;
pub use units::Sc2Unit;

pub struct Sc2Plugin {
    pub settings: Sc2Settings,
}

impl Default for Sc2Plugin {
    fn default() -> Self {
        Self { settings: Sc2Settings::from_env() }
    }
}

#[derive(Resource, Clone)]
pub struct Sc2Settings {
    pub game_dir: PathBuf,
    pub map: PathBuf,
    /// GTA coordinates the SC2 playable-area centre maps to.
    pub anchor: [f32; 3],
    /// Metres per SC2 cell.
    pub scale: f32,
    /// Game loops per simulation step.
    pub step: u32,
    /// Mirror neutral map features (minerals, rocks). They come from the SC2
    /// map, not the GTA world, so they're hidden unless `SC2_NEUTRAL` is set.
    pub show_neutral: bool,
}

impl Sc2Settings {
    /// `SC2_DIR`, `SC2_MAP` (path or name in `<SC2_DIR>/Maps`), `SC2_ANCHOR=x,y,z`, `SC2_SCALE`, `SC2_NEUTRAL`.
    pub fn from_env() -> Self {
        let game_dir = PathBuf::from(std::env::var("SC2_DIR").unwrap_or_else(|_| r"G:\SC 2\StarCraft II".into()));
        let map = std::env::var("SC2_MAP").unwrap_or_else(|_| "AcropolisLE.SC2Map".into());
        let map = if map.contains(['/', '\\']) { PathBuf::from(map) } else { game_dir.join("Maps").join(map) };
        let anchor = std::env::var("SC2_ANCHOR")
            .ok()
            .and_then(|s| {
                let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                v.try_into().ok()
            })
            // Grove Street cul-de-sac.
            .unwrap_or([2495.0, -1670.0, 13.3]);
        // 2 m per cell: SC2 models are ~0.9 cells tall, GTA peds ~1.8 m.
        let scale = std::env::var("SC2_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0);
        Self { game_dir, map, anchor, scale, step: 2, show_neutral: std::env::var_os("SC2_NEUTRAL").is_some() }
    }

    /// SC2 map position -> Bevy world position (height from the anchor).
    pub fn to_world(&self, info: &MapInfo, p: [f32; 2]) -> Vec3 {
        let c = info.center();
        let g = [self.anchor[0] + (p[0] - c[0]) * self.scale, self.anchor[1] + (p[1] - c[1]) * self.scale, self.anchor[2]];
        Vec3::new(g[0], g[2], -g[1])
    }

    /// Bevy world position -> SC2 map position.
    pub fn to_map(&self, info: &MapInfo, w: Vec3) -> [f32; 2] {
        let c = info.center();
        let (gx, gy) = (w.x, -w.z);
        [c[0] + (gx - self.anchor[0]) / self.scale, c[1] + (gy - self.anchor[1]) / self.scale]
    }
}

/// Connection to the simulation thread plus the latest map info.
#[derive(Resource)]
pub struct Sc2Link {
    sim: Mutex<SimHandle>,
    pub info: Option<MapInfo>,
    pub status: String,
    pub game_loop: u32,
    /// `Time::elapsed_secs` when the latest snapshot arrived.
    pub snapshot_at: f32,
    pub step_secs: f32,
}

impl Sc2Link {
    pub fn send(&self, cmd: sc2_api::sim::Command) {
        if let Ok(sim) = self.sim.lock() {
            sim.send(cmd);
        }
    }

    pub fn ready(&self) -> Option<&MapInfo> {
        self.info.as_ref()
    }
}

/// Snapshots received this frame, in order.
#[derive(Message)]
pub struct SnapshotArrived(pub sc2_api::sim::Snapshot);

impl Plugin for Sc2Plugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.settings.clone())
            .add_message::<SnapshotArrived>()
            .add_systems(Startup, start_sim)
            .add_systems(PreUpdate, pump_events)
            .add_systems(Update, demo::demo)
            .add_plugins((units::UnitsPlugin, control::ControlPlugin, models::ModelsPlugin));
    }
}

fn start_sim(mut commands: Commands, settings: Res<Sc2Settings>) {
    let mut cfg = SimConfig::new(&settings.game_dir, &settings.map);
    cfg.step = settings.step;
    info!("sc2: launching {} on {}", settings.game_dir.display(), settings.map.display());
    commands.insert_resource(Sc2Link {
        sim: Mutex::new(SimHandle::spawn(cfg)),
        info: None,
        status: "starting StarCraft II...".into(),
        game_loop: 0,
        snapshot_at: 0.0,
        step_secs: settings.step as f32 / sc2_api::sim::LOOPS_PER_SEC,
    });
}

fn pump_events(time: Res<Time>, link: Option<ResMut<Sc2Link>>, mut out: MessageWriter<SnapshotArrived>) {
    let Some(mut link) = link else { return };
    let events: Vec<Event> = match link.sim.lock() {
        Ok(sim) => sim.events.try_iter().collect(),
        Err(_) => return,
    };
    for ev in events {
        match ev {
            Event::Ready(info) => {
                info!("sc2: ready, map {} {}x{}", info.name, info.size[0], info.size[1]);
                link.status = format!("{} ({}x{})", info.name, info.size[0], info.size[1]);
                link.info = Some(info);
            }
            Event::Snapshot(s) => {
                link.game_loop = s.game_loop;
                link.snapshot_at = time.elapsed_secs();
                out.write(SnapshotArrived(s));
            }
            Event::Ended(r) => link.status = format!("game over: {r}"),
            Event::Failed(e) => {
                error!("sc2: {e}");
                link.status = format!("failed: {e}");
            }
        }
    }
}
