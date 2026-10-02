//! Blocking s2client protocol connection to a retail game client.

use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use prost::Message as _;
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use crate::proto::{self, request::Request as Req, response::Response as Resp};

/// Internal ports for a two-player local game.
#[derive(Clone, Copy, Debug)]
pub struct Ports {
    pub shared: i32,
    pub server: [i32; 2],
    pub client: [i32; 2],
}

impl Ports {
    pub fn from_base(base: i32) -> Self {
        Self { shared: base, server: [base + 1, base + 2], client: [base + 3, base + 4] }
    }
}

pub struct Client {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    child: Option<Child>,
}

/// Newest `Versions/BaseNNNNN/SC2_x64.exe` of an install.
pub fn find_executable(game_dir: &Path) -> Result<PathBuf> {
    let versions = game_dir.join("Versions");
    std::fs::read_dir(&versions)
        .with_context(|| format!("{}", versions.display()))?
        .filter_map(|e| {
            let p = e.ok()?.path();
            let build: u32 = p.file_name()?.to_str()?.strip_prefix("Base")?.parse().ok()?;
            p.join("SC2_x64.exe").is_file().then_some((build, p.join("SC2_x64.exe")))
        })
        .max_by_key(|(b, _)| *b)
        .map(|(_, p)| p)
        .with_context(|| format!("no SC2_x64.exe under {}", versions.display()))
}

impl Client {
    /// Starts `SC2_x64.exe -listen` and connects to it. The process is killed
    /// when the client is dropped.
    pub fn launch(game_dir: &Path, port: u16) -> Result<Self> {
        let exe = find_executable(game_dir)?;
        let child = Command::new(&exe)
            .current_dir(game_dir.join("Support64"))
            .args(["-listen", "127.0.0.1", "-port", &port.to_string(), "-displayMode", "0"])
            .args(["-windowwidth", "640", "-windowheight", "480", "-windowx", "0", "-windowy", "0"])
            .arg("-dataDir")
            .arg(game_dir)
            .spawn()
            .with_context(|| format!("launch {}", exe.display()))?;
        let mut child = Some(child);
        match Self::connect_retry(port, Duration::from_secs(120), &mut child) {
            Ok(ws) => Ok(Self { ws, child }),
            Err(e) => {
                if let Some(mut c) = child {
                    let _ = c.kill();
                }
                Err(e)
            }
        }
    }

    /// Connects to an already running `-listen` client.
    pub fn connect(port: u16) -> Result<Self> {
        Ok(Self { ws: Self::connect_retry(port, Duration::from_secs(5), &mut None)?, child: None })
    }

    fn connect_retry(
        port: u16,
        timeout: Duration,
        child: &mut Option<Child>,
    ) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
        let url = format!("ws://127.0.0.1:{port}/sc2api");
        let start = Instant::now();
        loop {
            match tungstenite::connect(&url) {
                Ok((ws, _)) => return Ok(ws),
                Err(e) if start.elapsed() > timeout => bail!("connect {url}: {e}"),
                Err(_) => {}
            }
            if let Some(c) = child
                && let Some(status) = c.try_wait()?
            {
                bail!("SC2 exited during startup: {status}");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Sends one request and returns its response payload; protocol errors become `Err`.
    pub fn request(&mut self, req: Req) -> Result<Resp> {
        self.send(req)?;
        self.recv()
    }

    /// Sends a request without waiting; pair with [`Client::recv`]. Lets two
    /// clients of one multiplayer game block on the same step concurrently.
    pub fn send(&mut self, req: Req) -> Result<()> {
        let msg = proto::Request { request: Some(req), id: None };
        self.ws.send(Message::Binary(msg.encode_to_vec().into()))?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Resp> {
        loop {
            match self.ws.read()? {
                Message::Binary(b) => {
                    let r = proto::Response::decode(&b[..])?;
                    if !r.error.is_empty() {
                        bail!("sc2: {}", r.error.join("; "));
                    }
                    return r.response.context("empty response");
                }
                Message::Close(_) => bail!("sc2 closed the connection"),
                _ => {}
            }
        }
    }

    pub fn create_game(&mut self, map: &Path, players: Vec<proto::PlayerSetup>, realtime: bool, seed: Option<u32>) -> Result<()> {
        let map = std::path::absolute(map)?;
        let resp = self.request(Req::CreateGame(proto::RequestCreateGame {
            map: Some(proto::request_create_game::Map::LocalMap(proto::LocalMap {
                map_path: Some(map.to_string_lossy().into_owned()),
                map_data: None,
            })),
            player_setup: players,
            disable_fog: Some(true),
            random_seed: seed,
            realtime: Some(realtime),
        }))?;
        match resp {
            Resp::CreateGame(r) if r.error.is_none() => Ok(()),
            Resp::CreateGame(r) => bail!("create_game: {:?} {}", r.error, r.error_details.unwrap_or_default()),
            other => bail!("create_game: unexpected {other:?}"),
        }
    }

    /// Joins as a participant with raw (unit list) observations; returns our player id.
    pub fn join_game(&mut self, race: proto::Race) -> Result<u32> {
        self.send_join(race, None)?;
        self.finish_join()
    }

    /// Join request for a multiplayer game; every client passes the same `ports`.
    pub fn send_join(&mut self, race: proto::Race, ports: Option<&Ports>) -> Result<()> {
        let ps = |p: [i32; 2]| proto::PortSet { game_port: Some(p[0]), base_port: Some(p[1]) };
        self.send(Req::JoinGame(proto::RequestJoinGame {
            participation: Some(proto::request_join_game::Participation::Race(race as i32)),
            options: Some(proto::InterfaceOptions {
                raw: Some(true),
                score: Some(false),
                show_cloaked: Some(true),
                raw_affects_selection: Some(false),
                raw_crop_to_playable_area: Some(false),
                ..Default::default()
            }),
            server_ports: ports.map(|p| ps(p.server)),
            client_ports: ports.map(|p| vec![ps(p.client)]).unwrap_or_default(),
            shared_port: ports.map(|p| p.shared),
            ..Default::default()
        }))
    }

    pub fn finish_join(&mut self) -> Result<u32> {
        match self.recv()? {
            Resp::JoinGame(r) if r.error.is_none() => r.player_id.context("join_game: no player id"),
            Resp::JoinGame(r) => bail!("join_game: {:?} {}", r.error, r.error_details.unwrap_or_default()),
            other => bail!("join_game: unexpected {other:?}"),
        }
    }

    pub fn game_info(&mut self) -> Result<proto::ResponseGameInfo> {
        match self.request(Req::GameInfo(proto::RequestGameInfo {}))? {
            Resp::GameInfo(r) => Ok(r),
            other => bail!("game_info: unexpected {other:?}"),
        }
    }

    pub fn unit_types(&mut self) -> Result<Vec<proto::UnitTypeData>> {
        match self.request(Req::Data(proto::RequestData { unit_type_id: Some(true), ..Default::default() }))? {
            Resp::Data(r) => Ok(r.units),
            other => bail!("data: unexpected {other:?}"),
        }
    }

    pub fn step(&mut self, loops: u32) -> Result<()> {
        match self.request(Req::Step(proto::RequestStep { count: Some(loops) }))? {
            Resp::Step(_) => Ok(()),
            other => bail!("step: unexpected {other:?}"),
        }
    }

    pub fn observe(&mut self) -> Result<proto::ResponseObservation> {
        match self.request(Req::Observation(proto::RequestObservation::default()))? {
            Resp::Observation(r) => Ok(r),
            other => bail!("observation: unexpected {other:?}"),
        }
    }

    pub fn act(&mut self, actions: Vec<proto::Action>) -> Result<()> {
        if actions.is_empty() {
            return Ok(());
        }
        match self.request(Req::Action(proto::RequestAction { actions }))? {
            Resp::Action(_) => Ok(()),
            other => bail!("action: unexpected {other:?}"),
        }
    }

    pub fn debug(&mut self, debug: Vec<proto::DebugCommand>) -> Result<()> {
        if debug.is_empty() {
            return Ok(());
        }
        match self.request(Req::Debug(proto::RequestDebug { debug }))? {
            Resp::Debug(_) => Ok(()),
            other => bail!("debug: unexpected {other:?}"),
        }
    }

    pub fn quit(&mut self) {
        let _ = self.request(Req::Quit(proto::RequestQuit {}));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = self.ws.close(None);
            if c.wait_timeout_ms(3000).is_none() {
                let _ = c.kill();
            }
            let _ = c.wait();
        }
    }
}

trait WaitTimeout {
    fn wait_timeout_ms(&mut self, ms: u64) -> Option<std::process::ExitStatus>;
}

impl WaitTimeout for Child {
    fn wait_timeout_ms(&mut self, ms: u64) -> Option<std::process::ExitStatus> {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if let Ok(Some(s)) = self.try_wait() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}
