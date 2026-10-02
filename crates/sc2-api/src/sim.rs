//! SC2 as a lockstep simulation thread: the engine sends [`Command`]s and
//! receives [`Event`]s (map info once, then a [`Snapshot`] every step).
//!
//! Everything crossing the channel is plain data in SC2 map coordinates
//! (cells, Z up), so a reimplemented simulation can produce the same stream.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    client::{Client, Ports},
    proto::{self, action_raw, action_raw_unit_command::Target, debug_command},
};

/// Game loops per real second at "Faster" speed.
pub const LOOPS_PER_SEC: f32 = 22.4;

/// Generic ability ids (accepted for every unit type).
pub mod ability {
    pub const SMART: i32 = 1;
    pub const STOP: i32 = 3665;
    pub const ATTACK: i32 = 3674;
    pub const MOVE: i32 = 3794;
    pub const HOLD: i32 = 3793;
}

pub struct SimConfig {
    pub game_dir: PathBuf,
    pub map: PathBuf,
    pub port: u16,
    pub opponent: Opponent,
    pub race: proto::Race,
    /// Game loops per step: lower is smoother, higher is cheaper.
    pub step: u32,
    /// Remove starting units (except town halls) after joining: a sandbox for debug spawns.
    pub clear_map: bool,
    pub seed: Option<u32>,
}

impl SimConfig {
    pub fn new(game_dir: impl Into<PathBuf>, map: impl Into<PathBuf>) -> Self {
        Self {
            game_dir: game_dir.into(),
            map: map.into(),
            port: 5679,
            opponent: Opponent::Puppet(proto::Race::Zerg),
            race: proto::Race::Terran,
            step: 2,
            clear_map: true,
            seed: None,
        }
    }
}

/// Who plays the second slot.
#[derive(Clone, Copy, Debug)]
pub enum Opponent {
    /// Built-in AI; it also controls every unit spawned for player 2.
    Computer(proto::Race, proto::Difficulty),
    /// A second client we drive ourselves (one more SC2 process), so player 2's
    /// units only do what [`Command`]s tell them.
    Puppet(proto::Race),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alliance {
    Own,
    Ally,
    Neutral,
    Enemy,
}

#[derive(Clone, Debug)]
pub struct UnitState {
    pub tag: u64,
    pub unit_type: u32,
    pub owner: i32,
    pub alliance: Alliance,
    /// Map cells, Z up.
    pub pos: [f32; 3],
    /// Radians, counter-clockwise from +X.
    pub facing: f32,
    pub radius: f32,
    pub health: f32,
    pub health_max: f32,
    pub shield: f32,
    pub shield_max: f32,
    pub energy: f32,
    pub build_progress: f32,
    pub flying: bool,
    pub weapon_cooldown: f32,
    /// Unit currently being attacked, if any.
    pub engaged_target: Option<u64>,
    /// First queued order's ability id.
    pub order: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub game_loop: u32,
    pub units: Vec<UnitState>,
    /// Tags that died since the previous snapshot.
    pub dead: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct MapInfo {
    pub name: String,
    pub player_id: u32,
    pub size: [u32; 2],
    /// Playable rectangle, min and max corner in cells.
    pub playable: [[f32; 2]; 2],
    pub start_locations: Vec<[f32; 2]>,
    /// Unit type id -> name (e.g. 48 -> "Marine").
    pub unit_names: HashMap<u32, String>,
    /// Terrain height per cell, row 0 = bottom (y = 0).
    pub height: Vec<f32>,
}

impl MapInfo {
    pub fn center(&self) -> [f32; 2] {
        let [a, b] = self.playable;
        [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    Move { units: Vec<u64>, to: [f32; 2], queue: bool },
    AttackMove { units: Vec<u64>, to: [f32; 2], queue: bool },
    Attack { units: Vec<u64>, target: u64 },
    /// Right click: move, attack, gather... depending on what is clicked.
    Smart { units: Vec<u64>, to: [f32; 2], target: Option<u64> },
    Stop { units: Vec<u64> },
    Hold { units: Vec<u64> },
    Spawn { unit_type: u32, owner: i32, at: [f32; 2], count: u32 },
    Kill { units: Vec<u64> },
}

#[derive(Debug)]
pub enum Event {
    Ready(MapInfo),
    Snapshot(Snapshot),
    Ended(String),
    Failed(String),
}

/// Owns the simulation thread; dropping it ends the game and closes SC2.
pub struct SimHandle {
    pub events: Receiver<Event>,
    commands: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

enum Msg {
    Cmd(Command),
    Pause(bool),
    Quit,
}

impl SimHandle {
    pub fn spawn(cfg: SimConfig) -> Self {
        let (ev_tx, events) = mpsc::channel();
        let (commands, cmd_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("sc2-sim".into())
            .spawn(move || {
                if let Err(e) = run(cfg, &ev_tx, &cmd_rx) {
                    let _ = ev_tx.send(Event::Failed(format!("{e:#}")));
                }
            })
            .expect("spawn sc2-sim thread");
        Self { events, commands, thread: Some(thread) }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(Msg::Cmd(cmd));
    }

    pub fn pause(&self, paused: bool) {
        let _ = self.commands.send(Msg::Pause(paused));
    }
}

impl Drop for SimHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(cfg: SimConfig, events: &Sender<Event>, commands: &Receiver<Msg>) -> Result<()> {
    let setup = |ty: proto::PlayerType, race: proto::Race, diff: Option<proto::Difficulty>| proto::PlayerSetup {
        r#type: Some(ty as i32),
        race: Some(race as i32),
        difficulty: diff.map(|d| d as i32),
        ..Default::default()
    };
    let me = setup(proto::PlayerType::Participant, cfg.race, None);

    let (mut c, mut puppet, player_id) = match cfg.opponent {
        Opponent::Computer(race, diff) => {
            let mut c = Client::launch(&cfg.game_dir, cfg.port)?;
            c.create_game(&cfg.map, vec![me, setup(proto::PlayerType::Computer, race, Some(diff))], false, cfg.seed)?;
            let id = c.join_game(cfg.race)?;
            (c, None, id)
        }
        Opponent::Puppet(race) => {
            // Start both processes at once; startup dominates.
            let (dir, port2) = (cfg.game_dir.clone(), cfg.port + 1);
            let second = std::thread::spawn(move || Client::launch(&dir, port2));
            let mut c = Client::launch(&cfg.game_dir, cfg.port)?;
            let mut p = second.join().map_err(|_| anyhow::anyhow!("second client launcher panicked"))??;
            c.create_game(&cfg.map, vec![me, setup(proto::PlayerType::Participant, race, None)], false, cfg.seed)?;
            let ports = Ports::from_base(cfg.port as i32 + 10);
            c.send_join(cfg.race, Some(&ports))?;
            p.send_join(race, Some(&ports))?;
            let id = c.finish_join()?;
            let pid = p.finish_join()?;
            (c, Some((p, pid)), id)
        }
    };
    let info = map_info(&mut c, player_id)?;

    if cfg.clear_map {
        step(&mut c, &mut puppet, 1)?;
        let obs = c.observe()?;
        let tags = obs.observation.and_then(|o| o.raw_data).map(|r| r.units).unwrap_or_default();
        // Keep the town halls: a player with no structures loses immediately.
        const TOWN_HALLS: [u32; 3] = [18, 59, 86]; // CommandCenter, Nexus, Hatchery
        let tags: Vec<u64> = tags
            .iter()
            .filter(|u| u.alliance != Some(proto::Alliance::Neutral as i32))
            .filter(|u| !u.unit_type.is_some_and(|t| TOWN_HALLS.contains(&t)))
            .filter_map(|u| u.tag)
            .collect();
        c.debug(vec![debug(debug_command::Command::KillUnit(proto::DebugKillUnit { tag: tags }))])?;
    }
    if events.send(Event::Ready(info)).is_err() {
        return Ok(());
    }

    let step_time = Duration::from_secs_f32(cfg.step as f32 / LOOPS_PER_SEC);
    let mut next = Instant::now();
    let mut paused = false;
    let mut owners: HashMap<u64, i32> = HashMap::new();
    let puppet_id = puppet.as_ref().map(|(_, id)| *id as i32);
    loop {
        let (mut mine, mut theirs, mut dbg) = (Vec::new(), Vec::new(), Vec::new());
        loop {
            match commands.try_recv() {
                Ok(Msg::Cmd(cmd)) => translate(cmd, &mut dbg, |units, build| {
                    // The puppet player's units are commanded through its own client.
                    let (p_units, m_units): (Vec<u64>, Vec<u64>) =
                        units.into_iter().partition(|t| puppet_id.is_some() && owners.get(t).copied() == puppet_id);
                    if !m_units.is_empty() {
                        mine.push(build(m_units));
                    }
                    if !p_units.is_empty() {
                        theirs.push(build(p_units));
                    }
                }),
                Ok(Msg::Pause(p)) => paused = p,
                Ok(Msg::Quit) | Err(TryRecvError::Disconnected) => {
                    quit(&mut c, &mut puppet);
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        c.debug(dbg)?;
        c.act(mine)?;
        if let Some((p, _)) = &mut puppet {
            p.act(theirs)?;
        }

        if !paused {
            step(&mut c, &mut puppet, cfg.step)?;
        }
        let obs = c.observe()?;
        if let Some(r) = obs.player_result.iter().find(|r| r.player_id == Some(player_id)) {
            let _ = events.send(Event::Ended(format!("{:?}", proto::Result::try_from(r.result.unwrap_or(0)))));
            return Ok(());
        }
        let snap = snapshot(obs);
        owners = snap.units.iter().map(|u| (u.tag, u.owner)).collect();
        if !paused && events.send(Event::Snapshot(snap)).is_err() {
            quit(&mut c, &mut puppet);
            return Ok(());
        }

        next += step_time;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now; // fell behind: don't try to catch up in a burst
        }
    }
}

/// Steps every client before waiting on any: in a multiplayer game each one
/// blocks until the others have stepped too.
fn step(c: &mut Client, puppet: &mut Option<(Client, u32)>, loops: u32) -> Result<()> {
    let req = || proto::request::Request::Step(proto::RequestStep { count: Some(loops) });
    c.send(req())?;
    if let Some((p, _)) = puppet {
        p.send(req())?;
        p.recv()?;
    }
    c.recv()?;
    Ok(())
}

fn quit(c: &mut Client, puppet: &mut Option<(Client, u32)>) {
    c.quit();
    if let Some((p, _)) = puppet {
        p.quit();
    }
}

fn map_info(c: &mut Client, player_id: u32) -> Result<MapInfo> {
    let gi = c.game_info()?;
    let raw = gi.start_raw.unwrap_or_default();
    let size = raw.map_size.map(|s| [s.x.unwrap_or(0) as u32, s.y.unwrap_or(0) as u32]).unwrap_or([0, 0]);
    let playable = raw
        .playable_area
        .map(|r| {
            let p = |p: Option<proto::PointI>| p.map(|p| [p.x.unwrap_or(0) as f32, p.y.unwrap_or(0) as f32]).unwrap_or([0.0; 2]);
            [p(r.p0), p(r.p1)]
        })
        .unwrap_or([[0.0; 2], [size[0] as f32, size[1] as f32]]);
    // terrain_height: 8 bpp, -200..200 mapped to 0..255; rows are stored top (max y) first.
    let height = raw
        .terrain_height
        .filter(|img| img.bits_per_pixel == Some(8))
        .and_then(|img| img.data)
        .map(|data| {
            let w = size[0] as usize;
            let mut out = vec![0.0; data.len()];
            for (i, &b) in data.iter().enumerate() {
                let (x, y) = (i % w, i / w);
                let flipped = (size[1] as usize - 1 - y) * w + x;
                if let Some(o) = out.get_mut(flipped) {
                    *o = -200.0 + 400.0 * b as f32 / 255.0;
                }
            }
            out
        })
        .unwrap_or_default();
    let unit_names = c.unit_types()?.into_iter().filter_map(|u| Some((u.unit_id?, u.name?))).filter(|(_, n)| !n.is_empty()).collect();
    Ok(MapInfo {
        name: gi.map_name.unwrap_or_default(),
        player_id,
        size,
        playable,
        start_locations: raw.start_locations.iter().map(|p| [p.x.unwrap_or(0.0), p.y.unwrap_or(0.0)]).collect(),
        unit_names,
        height,
    })
}

fn snapshot(obs: proto::ResponseObservation) -> Snapshot {
    let o = obs.observation.unwrap_or_default();
    let raw = o.raw_data.unwrap_or_default();
    let units = raw
        .units
        .into_iter()
        .filter_map(|u| {
            let p = u.pos?;
            Some(UnitState {
                tag: u.tag?,
                unit_type: u.unit_type?,
                owner: u.owner.unwrap_or(0),
                alliance: match proto::Alliance::try_from(u.alliance.unwrap_or(0)) {
                    Ok(proto::Alliance::Self_) => Alliance::Own,
                    Ok(proto::Alliance::Ally) => Alliance::Ally,
                    Ok(proto::Alliance::Enemy) => Alliance::Enemy,
                    _ => Alliance::Neutral,
                },
                pos: [p.x.unwrap_or(0.0), p.y.unwrap_or(0.0), p.z.unwrap_or(0.0)],
                facing: u.facing.unwrap_or(0.0),
                radius: u.radius.unwrap_or(0.5),
                health: u.health.unwrap_or(0.0),
                health_max: u.health_max.unwrap_or(0.0),
                shield: u.shield.unwrap_or(0.0),
                shield_max: u.shield_max.unwrap_or(0.0),
                energy: u.energy.unwrap_or(0.0),
                build_progress: u.build_progress.unwrap_or(1.0),
                flying: u.is_flying.unwrap_or(false),
                weapon_cooldown: u.weapon_cooldown.unwrap_or(0.0),
                engaged_target: u.engaged_target_tag.filter(|&t| t != 0),
                order: u.orders.first().and_then(|o| o.ability_id),
            })
        })
        .collect();
    Snapshot { game_loop: o.game_loop.unwrap_or(0), units, dead: raw.event.map(|e| e.dead_units).unwrap_or_default() }
}

fn debug(cmd: debug_command::Command) -> proto::DebugCommand {
    proto::DebugCommand { command: Some(cmd) }
}

fn point(p: [f32; 2]) -> proto::Point2D {
    proto::Point2D { x: Some(p[0]), y: Some(p[1]) }
}

/// Debug commands go straight to `dbg`; unit commands are handed to `route`
/// as (units, action builder) so the caller can split them per owning client.
fn translate(cmd: Command, dbg: &mut Vec<proto::DebugCommand>, mut route: impl FnMut(Vec<u64>, &dyn Fn(Vec<u64>) -> proto::Action)) {
    let mut unit_cmd = |ability: i32, units: Vec<u64>, target: Option<Target>, queue: bool| {
        let build = |unit_tags: Vec<u64>| proto::Action {
            action_raw: Some(proto::ActionRaw {
                action: Some(action_raw::Action::UnitCommand(proto::ActionRawUnitCommand {
                    ability_id: Some(ability),
                    unit_tags,
                    queue_command: Some(queue),
                    target: target.clone(),
                })),
            }),
            ..Default::default()
        };
        route(units, &build)
    };
    match cmd {
        Command::Move { units, to, queue } => unit_cmd(ability::MOVE, units, Some(Target::TargetWorldSpacePos(point(to))), queue),
        Command::AttackMove { units, to, queue } => unit_cmd(ability::ATTACK, units, Some(Target::TargetWorldSpacePos(point(to))), queue),
        Command::Attack { units, target } => unit_cmd(ability::ATTACK, units, Some(Target::TargetUnitTag(target)), false),
        Command::Smart { units, to, target } => {
            let t = match target {
                Some(tag) => Target::TargetUnitTag(tag),
                None => Target::TargetWorldSpacePos(point(to)),
            };
            unit_cmd(ability::SMART, units, Some(t), false)
        }
        Command::Stop { units } => unit_cmd(ability::STOP, units, None, false),
        Command::Hold { units } => unit_cmd(ability::HOLD, units, None, false),
        Command::Spawn { unit_type, owner, at, count } => dbg.push(debug(debug_command::Command::CreateUnit(proto::DebugCreateUnit {
            unit_type: Some(unit_type),
            owner: Some(owner),
            pos: Some(point(at)),
            quantity: Some(count),
        }))),
        Command::Kill { units } => dbg.push(debug(debug_command::Command::KillUnit(proto::DebugKillUnit { tag: units }))),
    }
}
