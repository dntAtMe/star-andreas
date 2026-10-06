//! Mission scripts: main.scm on the [`vm::Vm`] with the game commands of the opening
//! (scm.md §12). `SA_SCM=1` runs MAIN from the start of a new game.

pub mod ops;
pub mod vm;

use std::{collections::HashMap, sync::Arc};

use bevy::prelude::*;
use sa_physics::{ped::PedLogic, world::EntityId};

use crate::{
    cutscene::Cutscene,
    hud::{IntroText, Overlay},
    peds::ScriptPedReq,
    player::Ped,
    saphys::{SaPhys, SaPhysExt},
    world::g2b,
};
use vm::{Exec, Flow, Host, Vm};

pub struct ScriptPlugin;

impl Plugin for ScriptPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScriptCam>()
            .init_resource::<ScriptWalk>()
            .init_resource::<PlayerControl>()
            .add_systems(Startup, init_scripts)
            .add_systems(Update, run_scripts.before(crate::saphys::SaStep).after(crate::player::player_control))
            .add_systems(
                PostUpdate,
                script_camera
                    .after(crate::saphys::SaSync)
                    .after(crate::camera::sa_camera)
                    .after(crate::player::orbit_camera)
                    .before(crate::cutscene::update_cutscene)
                    .before(bevy::transform::TransformSystems::Propagate),
            );
    }
}

/// `TASK_GO_STRAIGHT_TO_COORD` on the player: ped, target, move state (4 walk, 6 run, 7 sprint).
#[derive(Resource, Default)]
pub struct ScriptWalk(pub Option<(EntityId, Vec3, i32)>);

/// `SET_PLAYER_CONTROL` (MakePlayerSafe): false = no player input.
#[derive(Resource)]
pub struct PlayerControl(pub bool);

impl Default for PlayerControl {
    fn default() -> Self {
        Self(true)
    }
}

/// A `CAMERA_SET_VECTOR_MOVE` / `_TRACK` interpolation.
#[derive(Clone, Copy, Debug)]
struct VecMove {
    from: Vec3,
    to: Vec3,
    start_ms: u32,
    dur_ms: f32,
    ease: bool,
}

impl VecMove {
    /// The per-frame update (0x516440 / 0x5164A0): evaluated while `now <= end` (inclusive);
    /// ease `(sin((270 - 180 f) deg) + 1) / 2`.
    fn at(&self, now: u32) -> Option<Vec3> {
        let end = self.start_ms as f32 + self.dur_ms;
        let t = now as f32;
        if t > end {
            return None;
        }
        let mut f = if self.dur_ms > 0.0 { (t - self.start_ms as f32) / self.dur_ms } else { 1.0 };
        if self.ease {
            f = (((270.0 - f * 180.0) * 0.017_453_292f32).sin() + 1.0) * 0.5;
        }
        Some(self.from + (self.to - self.from) * f)
    }
}

/// Script camera state (script_presentation.md §1): the stored fixed-mode source (015F), the
/// MODE_FIXED shot taken by 0160 (source, target), and the "new scriptables".
#[derive(Resource, Default)]
pub struct ScriptCam {
    stored_source: Vec3,
    /// The active MODE_FIXED shot (None = the player camera).
    shot: Option<(Vec3, Vec3)>,
    mv: Option<VecMove>,
    track: Option<VecMove>,
    persist_pos: bool,
    persist_track: bool,
    /// The last evaluated move / track values (held while persisting).
    last_mv: Option<Vec3>,
    last_track: Option<Vec3>,
}

/// Script-side state the host keeps: handles, briefs, models, timers.
#[derive(Default)]
struct HostState {
    next_handle: i32,
    peds: HashMap<i32, EntityId>,
    cars: HashMap<i32, (Entity, EntityId)>,
    /// Special character slots (`LOAD_SPECIAL_CHARACTER`): model name per slot 1..10.
    special: [String; 10],
    /// The current brief and when it ends (game ms).
    brief_until: u32,
    use_text_commands: u8,
    texts: Vec<IntroText>,
    cur_text: IntroText,
    /// peds.ide: id → (model, ped type name, anim group).
    peds_ide: HashMap<u32, (String, String, usize)>,
    /// vehicles.ide: id → model.
    cars_ide: HashMap<u32, String>,
    /// data/Paths/carrec.img, its entries by recording number, and the loaded recordings.
    carrec: Option<sa_formats::img::Img>,
    carrec_names: HashMap<u32, String>,
    recordings: HashMap<u32, Arc<Vec<sa_formats::carrec::Record>>>,
    /// The beat track (0952..0955): stream track, status (0 none, 2 loaded, 3 playing), sink.
    beat_track: Option<u16>,
    beat_status: i32,
    beat_entity: Option<Entity>,
    /// Mission audio slots 1..4: the sound, its sink, played.
    maudio: [Option<(u16, u16)>; 4],
    maudio_entity: [Option<Entity>; 4],
    maudio_played: [bool; 4],
}

#[derive(Resource)]
pub struct Scripts {
    vm: Vm,
    st: HostState,
}

fn init_scripts(mut commands: Commands, root: Res<crate::player::GameRoot>) {
    if std::env::var("SA_SCM").is_err() {
        return;
    }
    let Ok(file) = std::fs::read(root.0.join("data/script/main.scm")) else {
        warn!("main.scm missing");
        return;
    };
    let mut st = HostState { next_handle: 1, ..default() };
    if let Ok(t) = std::fs::read(root.0.join("data/peds.ide")) {
        for p in sa_formats::population::parse_peds_ide(&String::from_utf8_lossy(&t)) {
            let g = sa_physics::anim::AnimManager::group_by_name(&p.anim_group).unwrap_or(sa_physics::anim::group::DEFAULT);
            st.peds_ide.insert(p.id, (p.model.to_ascii_lowercase(), p.ped_type, g));
        }
    }
    if let Ok(t) = std::fs::read(root.0.join("data/vehicles.ide")) {
        for v in sa_formats::vehicle::parse_vehicles_ide(&String::from_utf8_lossy(&t)) {
            st.cars_ide.insert(v.id, v.model.to_ascii_lowercase());
        }
    }
    st.carrec = sa_formats::img::Img::open(&root.0.join("data/Paths/carrec.img")).ok();
    if let Some(img) = &st.carrec {
        for e in img.entries() {
            st.carrec_names.insert(sa_formats::carrec::number(&e.name), e.name.clone());
        }
    }
    let vm = Vm::new(file);
    info!("main.scm: {} missions, {} models", vm.mission_offsets.len(), vm.model_names.len());
    commands.insert_resource(Scripts { vm, st });
}

/// `CTheScripts::Process` once per frame.
fn run_scripts(world: &mut World) {
    if !world.contains_resource::<Scripts>() {
        return;
    }
    // The game loads before the scripts run: wait for the player to be placed.
    let started = world.query::<&Ped>().iter(world).next().is_some_and(|p| p.started);
    if !started {
        return;
    }
    let dt = world.resource::<Time>().delta_secs();
    world.resource_scope(|world, mut scripts: Mut<Scripts>| {
        let Scripts { vm, st } = &mut *scripts;
        let now = world.resource::<SaPhys>().world.now_ms;
        // Briefs time out.
        if st.brief_until != 0 && now >= st.brief_until {
            st.brief_until = 0;
            world.resource_mut::<Overlay>().subtitle = None;
        }
        // USE_TEXT_COMMANDS: the text lines are rebuilt every frame.
        if st.use_text_commands != 0 {
            st.texts.clear();
            st.cur_text = IntroText::default();
            if st.use_text_commands == 1 {
                st.use_text_commands = 0;
            }
        }
        // SA_SCMLOG=1: the active scripts and their IPs every 2 s.
        if std::env::var("SA_SCMLOG").is_ok() && now / 2000 != now.wrapping_sub((dt * 1000.0) as u32) / 2000 {
            let list: Vec<String> = vm.active_scripts().iter().map(|&i| format!("{}@{:X}", vm.scripts[i].name, vm.scripts[i].ip)).collect();
            info!("scripts: {}", list.join(" "));
            let w = &world.resource::<SaPhys>().world;
            for (h, c) in &st.cars {
                info!("script car {h}: {:?}", w.body(c.1).map(|b| b.phys.matrix.pos));
            }
        }
        // Mission cars and peds keep their collision streamed in.
        let pts: Vec<Vec3> = {
            let w = &world.resource::<SaPhys>().world;
            st.cars.values().map(|c| c.1).chain(st.peds.values().copied()).filter_map(|id| w.body(id).map(|b| b.phys.matrix.pos)).collect()
        };
        if let Some(mut cs) = world.get_resource_mut::<crate::colstore::ColStore>() {
            cs.mission_points = pts;
        }
        let reqs = std::mem::take(&mut world.resource_mut::<SaPhys>().world.pickups.help_requests);
        if let Some((key, quick)) = reqs.into_iter().last() {
            let mut o = world.resource_mut::<Overlay>();
            o.help = key;
            o.help_quick = quick;
            o.help_permanent = false;
        }
        let mut host = WorldHost { world, st, ts: dt * 50.0 };
        vm.process(&mut host, (dt * 1000.0) as u32);
        let texts = std::mem::take(&mut host.st.texts);
        host.world.resource_mut::<Overlay>().texts = texts.clone();
        host.st.texts = texts;
    });
}

struct WorldHost<'a> {
    world: &'a mut World,
    st: &'a mut HostState,
    ts: f32,
}

fn deg(h: f32) -> f32 {
    let h = if h < 0.0 { h + 360.0 } else if h > 360.0 { h - 360.0 } else { h };
    h.to_radians()
}

impl WorldHost<'_> {
    /// REQUEST_CAR_RECORDING: load and smooth a recording (synchronously).
    fn recording(&mut self, n: u32) -> Option<Arc<Vec<sa_formats::carrec::Record>>> {
        if let Some(r) = self.st.recordings.get(&n) {
            return Some(r.clone());
        }
        let name = self.st.carrec_names.get(&n)?;
        let data = self.st.carrec.as_ref()?.get(name)?;
        let r = Arc::new(sa_formats::carrec::parse(data));
        self.st.recordings.insert(n, r.clone());
        Some(r)
    }

    /// A 2D audio sink for a stream track or an SFX bank sound.
    fn play(&mut self, track: Option<u16>, sound: Option<(u16, u16)>, volume_db: f32) -> Option<Entity> {
        use bevy::audio::{AudioPlayer, PlaybackMode, PlaybackSettings};
        let audio = self.world.get_resource::<crate::audio::Audio>()?.clone();
        if let Some(id) = track {
            let h = self.world.resource_scope(|_, mut a: Mut<Assets<AudioSource>>| audio.track(&mut a, id))?;
            let s = PlaybackSettings { mode: PlaybackMode::Despawn, volume: crate::audio::db(volume_db), ..default() };
            return Some(self.world.spawn((AudioPlayer::<AudioSource>(h), s)).id());
        }
        let (bank, idx) = sound?;
        let (h, headroom) = self.world.resource_scope(|_, mut a: Mut<Assets<crate::audio::PcmSound>>| audio.sound(&mut a, bank, idx))?;
        let s = PlaybackSettings { mode: PlaybackMode::Despawn, volume: crate::audio::db(volume_db - headroom), ..default() };
        Some(self.world.spawn((AudioPlayer::<crate::audio::PcmSound>(h), s)).id())
    }

    fn stop(&mut self, e: Option<Entity>) {
        if let Some(e) = e {
            if let Ok(em) = self.world.get_entity_mut(e) {
                em.despawn();
            }
        }
    }

    /// LOAD_SCENE stand-in: a player warped by the script waits (static) for the
    /// collision and models around the new spot, then stands on the ground.
    fn hold_player_for_streaming(&mut self, id: EntityId) {
        if Some(id) != self.player_ped() {
            return;
        }
        let now = self.world.resource::<Time>().elapsed_secs();
        if let Some(b) = self.sa().world.body_mut(id) {
            b.phys.eflags |= sa_physics::physical::ef::IS_STATIC;
        }
        let mut q = self.world.query::<&mut Ped>();
        for mut ped in q.iter_mut(self.world) {
            ped.frozen = true;
            ped.frozen_at = now;
        }
    }

    fn sa(&mut self) -> Mut<'_, SaPhys> {
        self.world.resource_mut::<SaPhys>()
    }

    fn handle(&mut self) -> i32 {
        let h = self.st.next_handle;
        self.st.next_handle += 1;
        h
    }

    fn ped(&self, h: i32) -> Option<EntityId> {
        self.st.peds.get(&h).copied()
    }

    fn car(&self, h: i32) -> Option<EntityId> {
        self.st.cars.get(&h).map(|c| c.1)
    }

    fn player_ped(&mut self) -> Option<EntityId> {
        self.world.resource::<SaPhys>().world.player_id()
    }

    fn pos_of(&self, id: EntityId) -> Option<Vec3> {
        let w = &self.world.resource::<SaPhys>().world;
        // In a vehicle: the vehicle's position.
        let veh = w.body(id).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).and_then(|p| p.vehicle.as_ref().map(|v| v.veh));
        w.body(veh.unwrap_or(id)).map(|b| b.phys.matrix.pos)
    }

    fn in_vehicle(&self, id: EntityId) -> Option<EntityId> {
        let w = &self.world.resource::<SaPhys>().world;
        w.body(id).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).and_then(|p| p.vehicle.as_ref().map(|v| v.veh))
    }

    /// A model id (negative = UsedObjectArray) to a ped / car model name.
    fn model_name(&self, vm: &Vm, m: i32) -> String {
        if m < 0 {
            return vm.model_names.get((-m) as usize).cloned().unwrap_or_default().to_ascii_lowercase();
        }
        let m = m as u32;
        if (290..300).contains(&m) {
            return self.st.special[(m - 290) as usize].clone();
        }
        if let Some(c) = self.st.cars_ide.get(&m) {
            return c.clone();
        }
        self.st.peds_ide.get(&m).map(|p| p.0.clone()).unwrap_or_default()
    }

    /// Entity flag 0x40000 (mission-cleanup scripts' CREATE_*): static until the collision at
    /// its position is loaded (colstore.rs clears it).
    fn wait_for_collision(&mut self, id: EntityId) {
        if let Some(b) = self.sa().world.body_mut(id) {
            b.phys.eflags |= sa_physics::physical::ef::IS_STATIC;
        }
        if let Some(mut cs) = self.world.get_resource_mut::<crate::colstore::ColStore>() {
            cs.waiting.push(id);
        }
    }

    /// A pickup model: negative = the script's used-object table.
    fn pickup_model(&self, vm: &Vm, m: i32) -> u16 {
        if m >= 0 {
            return m as u16;
        }
        let name = self.model_name(vm, m);
        self.world.resource::<crate::world::WorldRes>().0.objects.iter().find(|(_, o)| o.model.eq_ignore_ascii_case(&name)).map_or(0, |(&id, _)| id as u16)
    }

    fn create_char(&mut self, vm: &Vm, ped_type: i32, model: i32, pos: Vec3, seat: Option<(EntityId, i8)>, mission: bool) -> i32 {
        let name = self.model_name(vm, model);
        // Special characters take the anim group of their peds.ide slot (special01..).
        let group = self.st.peds_ide.get(&(model as u32)).map_or(sa_physics::anim::group::DEFAULT, |p| p.2);
        let mut pos = pos;
        if seat.is_none() && pos.z <= -100.0 {
            pos.z = self.sa().world.find_ground_z(pos + Vec3::new(0.0, 0.0, 50.0)).unwrap_or(pos.z);
        }
        let req = ScriptPedReq { model: name.clone(), id: model as u32, ped_type: ped_type as u8, anim_group: group, pos, heading: 0.0, seat };
        let id = self.world.run_system_cached_with(crate::peds::spawn_script_ped, req).ok().flatten();
        let h = self.handle();
        match id {
            Some(id) => {
                self.st.peds.insert(h, id);
                if mission && seat.is_none() {
                    self.wait_for_collision(id);
                }
            }
            None => warn!("script: CREATE_CHAR {model} ({name}) failed"),
        }
        h
    }
}

impl Host for WorldHost<'_> {
    fn now_ms(&self) -> u32 {
        self.world.resource::<SaPhys>().world.now_ms
    }

    fn timestep(&self) -> f32 {
        self.ts
    }

    fn skip_pressed(&mut self) -> bool {
        let k = self.world.resource::<ButtonInput<KeyCode>>();
        let m = self.world.resource::<ButtonInput<MouseButton>>();
        k.any_just_pressed([KeyCode::Space, KeyCode::Enter, KeyCode::NumpadEnter]) || m.just_pressed(MouseButton::Left)
    }

    fn command(&mut self, x: &mut Exec, op: u16) -> Option<Flow> {
        match op {
            // ---- player / peds
            0x0053 => {
                // CREATE_PLAYER idx x y z → var
                let idx = x.int();
                let p = Vec3::from(x.floats::<3>());
                if let Some(id) = self.player_ped() {
                    let mut sa = self.sa();
                    let z = if p.z <= -100.0 { sa.world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z) } else { p.z };
                    crate::player::ped_teleport(&mut sa, id, g2b([p.x, p.y, z + 1.0]), None);
                    self.hold_player_for_streaming(id);
                }
                x.store(&[idx]);
            }
            0x01F5 => {
                // GET_PLAYER_CHAR
                let _p = x.int();
                let pl = self.player_ped();
                let h = match self.st.peds.iter().find(|(_, v)| Some(**v) == pl) {
                    Some((&h, _)) => h,
                    None => {
                        let h = self.handle();
                        if let Some(id) = self.player_ped() {
                            self.st.peds.insert(h, id);
                        }
                        h
                    }
                };
                x.store(&[h]);
            }
            0x0256 => {
                let _p = x.int();
                let alive = self.player_ped().and_then(|id| self.world.resource::<SaPhys>().logic::<PedLogic>(id).map(|l| l.tasks.health.health > 0.0));
                x.cond(alive.unwrap_or(false));
            }
            0x01B4 => {
                let [_p, on] = x.ints::<2>();
                self.world.resource_mut::<PlayerControl>().0 = on != 0;
            }
            0x0173 => {
                let h = x.int();
                let d = x.float();
                if let Some(id) = self.ped(h) {
                    if self.in_vehicle(id).is_none() {
                        if let Some(l) = self.sa().logic_mut::<PedLogic>(id) {
                            l.cur_rot = deg(d);
                            l.aim_rot = deg(d);
                        }
                    }
                }
            }
            0x00A0 => {
                let h = x.int();
                let p = self.ped(h).and_then(|id| self.pos_of(id)).unwrap_or_default();
                x.store(&[p.x.to_bits() as i32, p.y.to_bits() as i32, p.z.to_bits() as i32]);
            }
            0x00A1 | 0x0362 => {
                // SET_CHAR_COORDINATES / WARP_CHAR_FROM_CAR_TO_COORD
                let h = x.int();
                let p = Vec3::from(x.floats::<3>());
                if let Some(id) = self.ped(h) {
                    let mut sa = self.sa();
                    if op == 0x0362 {
                        sa.world.set_ped_out_of_car(id, p, 0.0);
                    }
                    let z = if p.z <= -100.0 { sa.world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z) } else { p.z };
                    crate::player::ped_teleport(&mut sa, id, g2b([p.x, p.y, z + 1.0]), None);
                    drop(sa);
                    self.hold_player_for_streaming(id);
                }
            }
            0x009A => {
                let [t, m] = x.ints::<2>();
                let p = Vec3::from(x.floats::<3>());
                let mc = x.mission_cleanup();
                let h = self.create_char(x.vm, t, m, p, None, mc);
                x.store(&[h]);
            }
            0x0129 => {
                // CREATE_CHAR_INSIDE_CAR veh type model → var (driver)
                let [v, t, m] = x.ints::<3>();
                let seat = self.car(v).map(|c| (c, -1));
                let p = self.car(v).and_then(|c| self.pos_of(c)).unwrap_or_default();
                let mc = x.mission_cleanup();
                let h = self.create_char(x.vm, t, m, p, seat, mc);
                x.store(&[h]);
            }
            0x01C8 => {
                // CREATE_CHAR_AS_PASSENGER veh type model seat → var
                let [v, t, m, s] = x.ints::<4>();
                let seat = self.car(v).map(|c| (c, s.max(0) as i8));
                let p = self.car(v).and_then(|c| self.pos_of(c)).unwrap_or_default();
                let mc = x.mission_cleanup();
                let h = self.create_char(x.vm, t, m, p, seat, mc);
                x.store(&[h]);
            }
            0x0430 => {
                // WARP_CHAR_INTO_CAR_AS_PASSENGER ped veh seat
                let [p, v, s] = x.ints::<3>();
                if let (Some(p), Some(v)) = (self.ped(p), self.car(v)) {
                    self.sa().world.set_ped_in_car_as_passenger(p, v, s.max(0) as i8);
                }
            }
            0x009B | 0x01C2 => {
                let h = x.int();
                if op == 0x009B {
                    if let Some(id) = self.st.peds.remove(&h) {
                        let mut sa = self.sa();
                        sa.world.remove(id);
                        sa.world.npc_removed.push(id);
                    }
                }
            }
            0x0792 | 0x0687 => {
                // CLEAR_CHAR_TASKS(_IMMEDIATELY): the scripted walk ends.
                let h = x.int();
                if let Some(id) = self.ped(h) {
                    let mut w = self.world.resource_mut::<ScriptWalk>();
                    if w.0.is_some_and(|(p, _, _)| p == id) {
                        w.0 = None;
                    }
                }
            }
            0x048F => {
                x.int();
            }
            0x0223 => {
                let h = x.int();
                let v = x.int();
                if let Some(id) = self.ped(h) {
                    let mut sa = self.sa();
                    if let Some(l) = sa.logic_mut::<PedLogic>(id) {
                        l.tasks.health.health = v as f32;
                    }
                }
            }
            0x00DB => {
                let [p, v] = x.ints::<2>();
                let r = self.ped(p).and_then(|p| self.in_vehicle(p)).is_some_and(|c| Some(c) == self.car(v));
                x.cond(r);
            }
            0x00DD => {
                // IS_CHAR_IN_MODEL ped model
                let [p, m] = x.ints::<2>();
                let w = &self.world.resource::<SaPhys>().world;
                let r = self
                    .ped(p)
                    .and_then(|p| self.in_vehicle(p))
                    .and_then(|v| w.body(v))
                    .and_then(|b| {
                        let l = b.logic.as_any();
                        l.downcast_ref::<sa_physics::automobile::Automobile>()
                            .map(|c| c.model as i32)
                            .or_else(|| l.downcast_ref::<sa_physics::bike::Bike>().map(|c| c.model as i32))
                    })
                    .is_some_and(|model| model == m);
                x.cond(r);
            }
            0x03EE => {
                // CAN_PLAYER_START_MISSION (CPlayerPed::CanPlayerStartMission 0x609590) [S]:
                // playing, on foot, not entering / leaving a vehicle.
                let _p = x.int();
                let r = self.player_ped().is_some_and(|id| {
                    self.world.resource::<SaPhys>().logic::<PedLogic>(id).is_some_and(|l| {
                        l.tasks.health.health > 0.0 && l.vehicle.is_none() && l.enter.is_none() && l.leave.is_none()
                    })
                });
                x.cond(r);
            }
            0x00DF => {
                let p = x.int();
                let r = self.ped(p).and_then(|p| self.in_vehicle(p)).is_some();
                x.cond(r);
            }
            0x044B => {
                let p = x.int();
                let r = self.ped(p).is_some_and(|p| self.in_vehicle(p).is_none());
                x.cond(r);
            }
            0x0118 => {
                let p = x.int();
                let dead = self.ped(p).is_none_or(|id| self.world.resource::<SaPhys>().logic::<PedLogic>(id).is_none_or(|l| l.tasks.health.health <= 0.0));
                x.cond(dead);
            }
            0x00EC..=0x00F1 | 0x00FE..=0x0103 => {
                let is3d = (0x00FE..=0x0103).contains(&op);
                let h = x.int();
                let (c, r) = if is3d {
                    let v = x.floats::<6>();
                    (Vec3::new(v[0], v[1], v[2]), Vec3::new(v[3], v[4], v[5]))
                } else {
                    let v = x.floats::<4>();
                    (Vec3::new(v[0], v[1], 0.0), Vec3::new(v[2], v[3], 0.0))
                };
                let _sphere = x.int();
                let id = self.ped(h);
                let p = id.and_then(|id| self.pos_of(id)).unwrap_or(Vec3::splat(1e9));
                let mut inside = (p.x - c.x).abs() <= r.x && (p.y - c.y).abs() <= r.y;
                if is3d {
                    inside &= (p.z - c.z).abs() <= r.z;
                }
                let in_car = id.and_then(|id| self.in_vehicle(id)).is_some();
                let k = if is3d { op - 0x00FE } else { op - 0x00EC };
                let ok = match k % 3 {
                    0 => inside,
                    1 => inside && !in_car,
                    _ => inside && in_car,
                };
                x.cond(ok);
            }
            // ---- cars
            0x00A5 => {
                let m = x.int();
                let p = Vec3::from(x.floats::<3>());
                let name = self.model_name(x.vm, m);
                let h = self.handle();
                let p = if p.z <= -100.0 { Vec3::new(p.x, p.y, self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z)) } else { p };
                match self.world.run_system_cached_with(crate::vehicle::spawn_script_car, (name.clone(), p, 0.0)).ok().flatten() {
                    Some(c) => {
                        self.st.cars.insert(h, c);
                        if x.mission_cleanup() {
                            self.wait_for_collision(c.1);
                        }
                    }
                    None => warn!("script: CREATE_CAR {m} ({name}) failed"),
                }
                x.store(&[h]);
            }
            0x00A6 => {
                let h = x.int();
                if let Some((e, id)) = self.st.cars.remove(&h) {
                    let occupants = self.world.resource::<SaPhys>().world.vehicle_occupants(id);
                    let player = self.player_ped();
                    let mut sa = self.sa();
                    for p in occupants {
                        if Some(p) == player {
                            warp_out_beside(&mut sa.world, p);
                            continue;
                        }
                        sa.world.remove(p);
                        sa.world.npc_removed.push(p);
                    }
                    sa.world.remove(id);
                    self.world.despawn(e);
                }
            }
            0x0175 => {
                let h = x.int();
                let d = x.float();
                if let Some(id) = self.car(h) {
                    let a = deg(d);
                    if let Some(b) = self.sa().world.body_mut(id) {
                        let pos = b.phys.matrix.pos;
                        let (s, c) = a.sin_cos();
                        b.phys.matrix = sa_physics::physical::Matrix { right: Vec3::new(c, s, 0.0), fwd: Vec3::new(-s, c, 0.0), up: Vec3::Z, pos };
                    }
                }
            }
            0x0119 => {
                let h = x.int();
                let dead = self.car(h).is_none_or(|id| self.world.resource::<SaPhys>().world.body(id).is_none());
                x.cond(dead);
            }
            0x01C3 | 0x067F => {
                // Accepted, not modelled yet (no-longer-needed cars, light overrides).
                let n = x.vm_count(op);
                for _ in 0..n {
                    x.skip_param();
                }
            }
            // ---- radar blips (CRadar, radar.rs)
            0x0186 | 0x0187 => {
                // ADD_BLIP_FOR_CAR / ADD_BLIP_FOR_CHAR: SetEntityBlip(type, entity, 0, 3),
                // ChangeBlipScale 3.
                use crate::radar::BlipType;
                let h = x.int();
                let ent = if op == 0x0186 { self.car(h) } else { self.ped(h) };
                let ty = if op == 0x0186 { BlipType::Car } else { BlipType::Char };
                let mut radar = self.world.resource_mut::<crate::radar::Radar>();
                let b = ent.map_or(-1, |e| radar.set_entity_blip(ty, e, 0, 3));
                if let Some(t) = radar.get_mut(b) {
                    t.size = 3;
                }
                x.store(&[b]);
            }
            0x0164 => {
                let b = x.int();
                self.world.resource_mut::<crate::radar::Radar>().clear_blip(b);
            }
            0x018B => {
                let [b, d] = x.ints::<2>();
                if let Some(t) = self.world.resource_mut::<crate::radar::Radar>().get_mut(b) {
                    t.display = d as u8;
                }
            }
            0x07E0 => {
                let [b, f] = x.ints::<2>();
                if let Some(t) = self.world.resource_mut::<crate::radar::Radar>().get_mut(b) {
                    t.friendly = f != 0;
                }
            }
            0x02A7 | 0x02A8 | 0x04CE | 0x0570 => {
                // ADD_SPRITE_BLIP_FOR_CONTACT_POINT / _FOR_COORD and the short-range variants:
                // SetCoordBlip(contact 5 / coord 4, pos, colour, 3) + SetBlipSprite.
                use crate::radar::BlipType;
                let mut p = Vec3::from(x.floats::<3>());
                let sprite = x.int();
                if p.z <= -100.0 {
                    p.z = self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z);
                }
                let (ty, colour, short) = match op {
                    0x02A7 => (BlipType::Contact, 0, false),
                    0x02A8 => (BlipType::Coord, 5, false),
                    0x04CE => (BlipType::Coord, 5, true),
                    _ => (BlipType::Contact, 2, true),
                };
                let mut radar = self.world.resource_mut::<crate::radar::Radar>();
                let b = radar.set_coord_blip(ty, p, colour, 3);
                if let Some(t) = radar.get_mut(b) {
                    t.sprite = sprite.clamp(0, 63) as u8;
                    t.short_range = short;
                }
                x.store(&[b]);
            }
            0x05EB | 0x085E => {
                // START_PLAYBACK_RECORDED_CAR (085E: looped)
                let [v, n] = x.ints::<2>();
                if let (Some(v), Some(r)) = (self.car(v), self.recording(n as u32)) {
                    self.sa().world.start_playback(v, r, op == 0x085E);
                }
            }
            0x05EC => {
                let v = x.int();
                if let Some(v) = self.car(v) {
                    self.sa().world.stop_playback(v);
                }
            }
            0x060E => {
                let v = x.int();
                let r = self.car(v).is_some_and(|v| self.world.resource::<SaPhys>().world.is_playback_going_on(v));
                x.cond(r);
            }
            0x099A => {
                // SET_CAR_COLLISION: bUsesCollision and bApplyGravity together.
                let [v, on] = x.ints::<2>();
                if let Some(v) = self.car(v) {
                    if let Some(b) = self.sa().world.body_mut(v) {
                        use sa_physics::physical::{ef, pf};
                        if on != 0 {
                            b.phys.eflags |= ef::USES_COLLISION;
                            b.phys.flags |= pf::APPLY_GRAVITY;
                        } else {
                            b.phys.eflags &= !ef::USES_COLLISION;
                            b.phys.flags &= !pf::APPLY_GRAVITY;
                        }
                    }
                }
            }
            0x0622 => {
                // TASK_LEAVE_CAR_IMMEDIATELY ped veh
                let [p, _v] = x.ints::<2>();
                if let Some(p) = self.ped(p) {
                    let mut sa = self.sa();
                    if !sa.world.start_leave_car_immediately(p) {
                        // No door to leave by: out beside the car.
                        warp_out_beside(&mut sa.world, p);
                    }
                }
            }
            0x05D3 => {
                // TASK_GO_STRAIGHT_TO_COORD ped x y z moveState time
                let p = x.int();
                let t = Vec3::from(x.floats::<3>());
                let [ms, _time] = x.ints::<2>();
                if let Some(id) = self.ped(p) {
                    self.world.resource_mut::<ScriptWalk>().0 = Some((id, t, ms));
                }
            }
            0x06D8 => {
                // CREATE_MISSION_TRAIN: no trains yet → handle of a dead vehicle.
                x.collect(5);
                let h = self.handle();
                x.store(&[h]);
            }
            0x06DC | 0x06DD => {
                x.int();
                x.float();
            }
            // ---- models / streaming: everything is loaded on demand
            0x07C0 => {
                let n = x.int();
                self.recording(n as u32);
            }
            0x07C1 => {
                let n = x.int();
                let r = self.recording(n as u32).is_some();
                x.cond(r);
            }
            0x0247 | 0x0249 | 0x0296 | 0x08A9 | 0x090F => {
                x.int();
            }
            0x0248 | 0x023D | 0x08AB => {
                x.int();
                x.cond(true);
            }
            0x023C => {
                let slot = x.int();
                let name = x.text().to_ascii_lowercase();
                if (1..=10).contains(&slot) {
                    self.st.special[(slot - 1) as usize] = name;
                }
            }
            0x0395 => {
                // CLEAR_AREA x y z radius bool: random (non-mission) cars and peds go.
                let c = Vec3::from(x.floats::<3>());
                let r = x.float();
                let _ = x.int();
                let player = self.player_ped();
                let mut sa = self.sa();
                let w = &mut sa.world;
                let mut cars = Vec::new();
                let mut peds = Vec::new();
                for id in w.body_ids() {
                    let Some(b) = w.body(id) else { continue };
                    if (b.phys.matrix.pos - c).length() > r {
                        continue;
                    }
                    if let Some(car) = b.logic.as_any().downcast_ref::<sa_physics::automobile::Automobile>() {
                        if car.autopilot.is_some() && car.rec_slot.is_none() {
                            cars.push(id);
                        }
                    } else if let Some(p) = b.logic.as_any().downcast_ref::<PedLogic>() {
                        if Some(id) != player && p.vehicle.is_none() && p.npc.as_ref().is_some_and(|n| !n.mission) {
                            peds.push(id);
                        }
                    }
                }
                for id in cars {
                    for p in w.vehicle_occupants(id) {
                        w.remove(p);
                        w.npc_removed.push(p);
                    }
                    w.remove(id);
                    if let Some(t) = w.traffic.as_mut() {
                        t.removed.push(id);
                    }
                }
                for id in peds {
                    w.remove(id);
                    w.npc_removed.push(id);
                }
            }
            0x03CB | 0x04E4 | 0x0A0B | 0x04BB => {
                let n = x.vm_count(op);
                for _ in 0..n {
                    x.skip_param();
                }
            }
            // ---- time / weather / world
            0x00C0 => {
                let [h, m] = x.ints::<2>();
                let mut sa = self.sa();
                let now = sa.world.now_ms;
                sa.world.clock.set(now, h as u8, m as u8);
            }
            0x00BF => {
                let c = &self.world.resource::<SaPhys>().world.clock;
                let (h, m) = (c.hours as i32, c.minutes as i32);
                x.store(&[h, m]);
            }
            0x01B6 => {
                let t = x.int();
                self.sa().world.weather.force_now(t as i16);
            }
            0x01B7 => self.sa().world.weather.release(),
            0x01BD => {
                let t = self.now_ms() as i32;
                x.store(&[t]);
            }
            // ---- fades / screen
            0x016A => {
                let [ms, dir] = x.ints::<2>();
                self.world.resource_mut::<Overlay>().fade(ms as f32 * 0.001, dir as u8);
            }
            0x016B => {
                let f = self.world.resource::<Overlay>().fading();
                x.cond(f);
            }
            // ---- pickups (CPickups, pickups.md §2.2): z <= -100 → ground + 0.5
            0x0213 | 0x032B => {
                let m = x.int();
                let t = x.int();
                let ammo = if op == 0x032B { x.int() } else { 0 };
                let mut p = Vec3::from(x.floats::<3>());
                let model = self.pickup_model(x.vm, m);
                if p.z <= -100.0 {
                    p.z = self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z) + 0.5;
                }
                let h = self.sa().world.generate_pickup(p, model, t as u8, ammo as u32, 0, false, 0);
                x.store(&[h]);
            }
            0x02E1 => {
                // CREATE_MONEY_PICKUP x y z amount permanent
                let mut p = Vec3::from(x.floats::<3>());
                let [amount, permanent] = x.ints::<2>();
                if p.z <= -100.0 {
                    p.z = self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z) + 0.5;
                }
                let t = if permanent != 0 { 19 } else { 8 };
                let h = self.sa().world.generate_pickup(p, sa_physics::pickups::mi::MONEY, t, amount as u32, 0, false, 0);
                x.store(&[h]);
            }
            0x0517 | 0x0518 => {
                // Locked / for-sale property pickups.
                let mut p = Vec3::from(x.floats::<3>());
                let price = if op == 0x0518 { x.int() } else { 0 };
                let key = x.text();
                if p.z <= -100.0 {
                    p.z = self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z) + 0.5;
                }
                let idx = match key.to_ascii_uppercase().as_str() {
                    "PROP_3" => 1,
                    "PROP_4" => 2,
                    _ => 0,
                };
                let (model, t) = if op == 0x0517 { (sa_physics::pickups::mi::PROPERTY_LOCKED, 17) } else { (sa_physics::pickups::mi::PROPERTY_FSALE, 18) };
                let h = self.sa().world.generate_pickup(p, model, t, price as u32, 0, false, idx);
                x.store(&[h]);
            }
            0x0958..=0x095A => {
                // Snapshot / horseshoe / oyster collectables (the stat counters are not ported).
                let mut p = Vec3::from(x.floats::<3>());
                if p.z <= -100.0 {
                    p.z = self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z) + 0.5;
                }
                let (model, t) = match op {
                    0x0958 => (sa_physics::pickups::mi::CAMERAPICKUP, 20),
                    0x0959 => (954, 3),
                    _ => (953, 3),
                };
                let h = self.sa().world.generate_pickup(p, model, t, 0, 0, false, 0);
                x.store(&[h]);
            }
            0x0214 => {
                let h = x.int();
                let r = self.sa().world.is_pickup_picked_up(h);
                x.cond(r);
            }
            0x0215 => {
                let h = x.int();
                self.sa().world.remove_pickup(h);
            }
            0x01EB | 0x03DE => {
                let v = x.float();
                if let Some(p) = self.sa().world.population.as_mut() {
                    if op == 0x01EB {
                        p.car_density_mult = v;
                    } else {
                        p.ped_density_mult = v;
                    }
                }
            }
            0x0169 => {
                x.collect(3);
            }
            0x02A3 => {
                let on = x.int();
                if std::env::var("SA_SCMLOG").is_ok() {
                    info!("script {} @{:X}: SET_WIDESCREEN {on}", x.vm.scripts[x.s].name, x.vm.scripts[x.s].ip);
                }
                self.world.resource_mut::<Overlay>().widescreen = on != 0;
            }
            // ---- text
            0x054C => {
                let t = x.text();
                self.world.resource_mut::<Overlay>().mission_table = Some(t);
            }
            0x00BC | 0x00BA => {
                let key = x.text();
                let [time, _flag] = x.ints::<2>();
                let now = self.now_ms();
                self.st.brief_until = now + time.max(0) as u32;
                self.world.resource_mut::<Overlay>().subtitle = Some(key);
            }
            0x00BE => {
                self.st.brief_until = 0;
                self.world.resource_mut::<Overlay>().subtitle = None;
            }
            0x03D5 => {
                let key = x.text();
                let mut o = self.world.resource_mut::<Overlay>();
                if o.subtitle.as_deref() == Some(key.as_str()) {
                    o.subtitle = None;
                }
            }
            0x03E5 | 0x0512 => {
                // PRINT_HELP / PRINT_HELP_FOREVER: CHud::SetHelpMessage(text, 0, permanent).
                let key = x.text();
                if std::env::var("SA_SCMLOG").is_ok() {
                    info!("script {}: PRINT_HELP {key} at {:.1}s", x.vm.scripts[x.s].name, self.world.resource::<Time>().elapsed_secs());
                }
                let mut o = self.world.resource_mut::<Overlay>();
                o.help = Some(key);
                o.help_quick = false;
                o.help_permanent = op == 0x0512;
            }
            0x03E6 => {
                let mut o = self.world.resource_mut::<Overlay>();
                o.help = None;
                o.help_quick = true;
                o.help_permanent = false;
            }
            0x09C8 => x.cond(true),
            0x03F0 => {
                let b = x.int();
                self.st.use_text_commands = if b != 0 { 2 } else { 1 };
            }
            0x033E => {
                let [px, py] = x.floats::<2>();
                let key = x.text();
                let mut t = self.st.cur_text.clone();
                t.x = px;
                t.y = py;
                t.key = key;
                self.st.texts.push(t);
                self.st.cur_text = IntroText::default();
            }
            0x033F => {
                let [a, b] = x.floats::<2>();
                self.st.cur_text.scale = (a, b);
            }
            0x0340 => {
                let c = x.ints::<4>();
                self.st.cur_text.colour = c.map(|v| v as u8);
            }
            0x0341 => self.st.cur_text.justify = x.int() != 0,
            0x0342 => self.st.cur_text.centre = x.int() != 0,
            0x0343 => self.st.cur_text.wrap_x = x.float(),
            0x0344 => self.st.cur_text.centre_size = x.float(),
            0x0345 => self.st.cur_text.background = x.int() != 0,
            0x0348 => self.st.cur_text.proportional = x.int() != 0,
            0x0349 => self.st.cur_text.font = x.int() as u8,
            0x03E0 => self.st.cur_text.draw_before_fade = x.int() != 0,
            0x03E4 => self.st.cur_text.right = x.int() != 0,
            0x060D => {
                let [s, r, g, b, a] = x.ints::<5>();
                self.st.cur_text.shadow = s as u8;
                self.st.cur_text.drop_colour = [r as u8, g as u8, b as u8, a as u8];
            }
            0x081C => {
                let [o, r, g, b, a] = x.ints::<5>();
                self.st.cur_text.outline = o as u8;
                self.st.cur_text.drop_colour = [r as u8, g as u8, b as u8, a as u8];
            }
            // ---- cutscenes
            0x02E4 => {
                let name = x.text();
                self.world.resource_mut::<Cutscene>().load(&name);
            }
            0x06B9 => {
                let r = self.world.resource::<Cutscene>().has_loaded();
                x.cond(r);
            }
            0x02E7 => self.world.resource_mut::<Cutscene>().start(),
            0x02E9 => {
                let r = self.world.resource::<Cutscene>().has_finished();
                x.cond(r);
            }
            0x02EA => self.world.resource_mut::<Cutscene>().clear(),
            0x056A => {
                let r = self.world.resource::<Cutscene>().skipped;
                x.cond(r);
            }
            // ---- camera
            0x015F => {
                let v = x.floats::<6>();
                if std::env::var("SA_SCMLOG").is_ok() {
                    info!("script {} @{:X}: SET_FIXED_CAMERA_POSITION {:?}", x.vm.scripts[x.s].name, x.vm.scripts[x.s].ip, &v[..3]);
                }
                self.world.resource_mut::<ScriptCam>().stored_source = Vec3::new(v[0], v[1], v[2]);
            }
            0x0160 => {
                // TakeControlNoEntity: MODE_FIXED on the next camera update (style 2 jump cut;
                // the interpolated style is taken as a cut too).
                let mut p = Vec3::from(x.floats::<3>());
                let _style = x.int();
                if p.z <= -100.0 {
                    p.z = self.sa().world.find_ground_z(p + Vec3::Z * 50.0).unwrap_or(p.z);
                }
                let mut c = self.world.resource_mut::<ScriptCam>();
                c.shot = Some((c.stored_source, p));
            }
            0x015A | 0x02EB => {
                // Restore / RestoreWithJumpCut: back to the player camera (the 1350 ms blend of
                // Restore is taken as a cut).
                self.world.resource_mut::<ScriptCam>().shot = None;
            }
            0x0373 => {}
            0x0925 => {
                let mut c = self.world.resource_mut::<ScriptCam>();
                c.mv = None;
                c.track = None;
                c.persist_pos = false;
                c.persist_track = false;
                c.last_mv = None;
                c.last_track = None;
            }
            0x092F => self.world.resource_mut::<ScriptCam>().persist_track = x.int() != 0,
            0x0930 => self.world.resource_mut::<ScriptCam>().persist_pos = x.int() != 0,
            0x0920 | 0x0936 => {
                let v = x.floats::<6>();
                let [ms, ease] = x.ints::<2>();
                let now = self.now_ms();
                let m = VecMove { from: Vec3::new(v[0], v[1], v[2]), to: Vec3::new(v[3], v[4], v[5]), start_ms: now, dur_ms: ms as f32, ease: ease != 0 };
                let mut c = self.world.resource_mut::<ScriptCam>();
                if op == 0x0936 {
                    c.mv = Some(m);
                } else {
                    c.track = Some(m);
                }
            }
            0x041D | 0x099C => {
                let n = x.vm_count(op);
                for _ in 0..n {
                    x.skip_param();
                }
            }
            // ---- audio
            0x0952 => {
                // PRELOAD_BEAT_TRACK: table 0x8AE538 → stream track; loads at once here.
                const BEATS: [u16; 14] = [180, 175, 178, 179, 177, 175, 175, 175, 175, 176, 184, 183, 182, 181];
                let id = x.int();
                let e = self.st.beat_entity.take();
                self.stop(e);
                self.st.beat_track = BEATS.get(id as usize).copied();
                self.st.beat_status = if self.st.beat_track.is_some() { 2 } else { 0 };
            }
            0x0953 => {
                if self.st.beat_status == 3 && self.st.beat_entity.is_some_and(|e| self.world.get_entity(e).is_err()) {
                    self.st.beat_status = 8;
                }
                let r = self.st.beat_status;
                x.store(&[r]);
            }
            0x0954 => {
                // PLAY_BEAT_TRACK: the cutscene track manager, -3 dB.
                if let Some(t) = self.st.beat_track {
                    self.st.beat_entity = self.play(Some(t), None, -3.0);
                    self.st.beat_status = 3;
                }
            }
            0x0955 => {
                let e = self.st.beat_entity.take();
                self.stop(e);
                self.st.beat_status = 0;
            }
            0x03CF => {
                // LOAD_MISSION_AUDIO slot id: speech ids >= 2000 → bank 147 + (id-2000)/200.
                let [slot, id] = x.ints::<2>();
                if let Some(k) = (slot as usize).checked_sub(1).filter(|&k| k < 4) {
                    self.st.maudio[k] = sa_formats::audio::SaAudio::mission_speech(id as u32);
                    self.st.maudio_played[k] = false;
                }
            }
            0x03D0 => {
                let slot = x.int();
                let r = (slot as usize).checked_sub(1).is_some_and(|k| self.st.maudio.get(k).is_some_and(|m| m.is_some()));
                x.cond(r);
            }
            0x03D1 => {
                // PLAY_MISSION_AUDIO: speech slots play 2D at +6 dB.
                let slot = x.int();
                if let Some(k) = (slot as usize).checked_sub(1).filter(|&k| k < 4) {
                    let e = self.play(None, self.st.maudio[k], 6.0);
                    self.st.maudio_entity[k] = e;
                    self.st.maudio_played[k] = true;
                }
            }
            0x03D2 => {
                let slot = x.int();
                let k = (slot as usize).saturating_sub(1).min(3);
                let done = !self.st.maudio_played[k] || self.st.maudio_entity[k].is_none_or(|e| self.world.get_entity(e).is_err());
                x.cond(done);
            }
            0x040D => {
                let slot = x.int();
                if let Some(k) = (slot as usize).checked_sub(1).filter(|&k| k < 4) {
                    let e = self.st.maudio_entity[k].take();
                    self.stop(e);
                    self.st.maudio[k] = None;
                    self.st.maudio_played[k] = false;
                }
            }
            0x0949 => {
                x.collect(2);
            }
            _ => return None,
        }
        Some(Flow::Continue)
    }
}

/// A ped out of its vehicle at the vehicle's left side.
fn warp_out_beside(w: &mut sa_physics::world::World, p: EntityId) {
    let seat = w.body(p).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).and_then(|l| l.vehicle.as_ref().map(|v| (v.veh, v.seat_index)));
    let Some((veh, idx)) = seat else { return };
    let Some(m) = w.body(veh).map(|b| b.phys.matrix) else { return };
    // Driver and rear-left (seat 1) get out on the left, the others on the right.
    let side = if idx < 0 || idx == 1 { -1.0 } else { 1.0 };
    let pos = m.pos + m.right * (1.8 * side) + Vec3::Z * 0.2;
    let heading = (-m.fwd.x).atan2(m.fwd.y);
    w.set_ped_out_of_car(p, pos, heading);
}

/// MODE_FIXED (Process_Fixed: FOV 70, world up) with the move (Source) and track (Front)
/// overrides of CCam::Process (script_presentation.md §1.5-1.7), over the player camera.
fn script_camera(
    sa: Res<SaPhys>,
    mut sc: ResMut<ScriptCam>,
    cs: Res<Cutscene>,
    cam: Single<(&mut Transform, &mut Projection), With<crate::player::OrbitCam>>,
) {
    let Some((mut src, mut tgt)) = sc.shot else { return };
    if cs.running() {
        return;
    }
    let now = sa.world.now_ms;
    if let Some(m) = sc.mv {
        match m.at(now) {
            Some(p) => sc.last_mv = Some(p),
            None if !sc.persist_pos => sc.last_mv = None,
            None => {}
        }
    }
    if let Some(m) = sc.track {
        match m.at(now) {
            Some(p) => sc.last_track = Some(p),
            None if !sc.persist_track => sc.last_track = None,
            None => {}
        }
    }
    if let Some(p) = sc.last_mv {
        src = p;
    }
    if let Some(p) = sc.last_track {
        tgt = p;
    }
    let (mut tf, mut proj) = cam.into_inner();
    let mut front = (tgt - src).normalize_or(Vec3::Y);
    if front.x == 0.0 && front.y == 0.0 {
        front = Vec3::new(1e-4, 1e-4, front.z).normalize();
    }
    let right = front.cross(Vec3::Z).normalize_or(Vec3::X);
    let up = right.cross(front);
    tf.translation = g2b(src.to_array());
    *tf = tf.looking_to(g2b(front.to_array()), g2b(up.to_array()));
    if let Projection::Perspective(p) = &mut *proj {
        p.fov = 2.0 * ((70.0f32 * 0.5).to_radians().tan() / p.aspect_ratio).atan();
    }
}
