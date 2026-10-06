//! `CEntryExit` / `CEntryExitManager` (interiors.md §2-3): the IPL `enex` doors, linked by name,
//! the per-frame detection (rotated rect, |Δz| < 1, no facing test), TransitionStarted's gates
//! and walk-in shot, and TransitionFinished's states (walk to the door, 1.0 s fade out — 0.5 s
//! in a vehicle —, arrival: currArea, stack, teleport, heading, camera restore, 1.0 s fade in).
//! Not yet: the yellow cone markers (C3dMarkers), door objects, extra colours, interior peds,
//! shops, gang warp.

use bevy::prelude::*;
use sa_physics::{automobile::Automobile, bike::Bike, ped::PedLogic};

use crate::{
    cutscene::Cutscene,
    hud::Overlay,
    saphys::{SaPhys, SaPhysExt},
    script::{PlayerControl, ScriptCam, ScriptWalk},
    world::{WorldRes, g2b},
};

pub struct EntryExitPlugin;

impl Plugin for EntryExitPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init_entry_exits)
            .add_systems(Update, update_entry_exits.after(crate::player::player_control).before(crate::saphys::SaStep));
    }
}

/// One `CEntryExit` (0x3C).
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    /// Entrance z + 1.
    z: f32,
    /// The file's radian value × deg2rad again (AddOne quirk) — used by IsInArea.
    rot: f32,
    /// Exit point (z + 1) and heading in degrees.
    exit: Vec3,
    exit_rot: f32,
    flags: u16,
    area: u8,
    num_peds: u8,
    time_on: u8,
    time_off: u8,
    link: Option<usize>,
}

const ENABLED: u16 = 0x4000;
const IN_PROGRESS: u16 = 0x2000;

impl Entry {
    fn centre(&self) -> Vec3 {
        Vec3::new((self.left + self.right) * 0.5, (self.top + self.bottom) * 0.5, self.z)
    }

    /// `CEntryExit::IsInArea` (0x43E460): the point rotated by +rot about the rect centre.
    fn is_in_area(&self, p: Vec3) -> bool {
        let q = if self.rot == 0.0 {
            p.truncate()
        } else {
            let c = self.centre().truncate();
            let d = p.truncate() - c;
            let (s, co) = self.rot.sin_cos();
            Vec2::new(co * d.x - s * d.y, s * d.x + co * d.y) + c
        };
        self.left <= q.x && q.x <= self.right && self.bottom <= q.y && q.y <= self.top && (p.z - self.z).abs() < 1.0
    }
}

/// `CEntryExitManager` statics.
#[derive(Resource, Default)]
pub struct EntryExits {
    entries: Vec<Entry>,
    /// `ms_exitEnterState`.
    pub state: u8,
    active: Option<usize>,
    spawn_point: usize,
    stack: Vec<usize>,
    pub disabled: bool,
    /// 0x96A7B8: the target is more than 10 m away.
    far: bool,
}

impl EntryExits {
    /// `EnableEntryExits(name, b)` (script 07FB): every entry with that name.
    pub fn enable(&mut self, name: &str, on: bool) {
        for e in self.entries.iter_mut().filter(|e| e.name.len() >= name.len().min(8) && e.name[..e.name.len().min(8)].eq_ignore_ascii_case(&name[..name.len().min(8)])) {
            if on {
                e.flags |= ENABLED;
            } else {
                e.flags &= !ENABLED;
            }
        }
    }
}

/// `CFileLoader::LoadEntryExit` → `AddOne` (0x43FA00): the burglary / random-closure /
/// enabled flag rules, linking by name (flag 4) with the partner's window opened to 0..24.
fn init_entry_exits(mut commands: Commands, world: Res<WorldRes>) {
    let mut ee = EntryExits::default();
    let mut seed: u32 = 0x1234_5678;
    for d in &world.0.entry_exits {
        let (mut on, mut off) = (d.time_on, d.time_off);
        let mut flags = d.flags;
        if flags & 0x1000 != 0 {
            on = 0;
            off = 24;
        }
        seed = seed.wrapping_mul(214013).wrapping_add(2531011);
        if flags & 0x400 != 0 && ((seed >> 16) & 0x7FFF) < 0x3FFF {
            on = 0;
            off = 0;
        }
        // Burglary houses are only enabled by script 09E6.
        if flags & 0x1000 == 0 {
            flags |= ENABLED;
        }
        let name: String = d.name.chars().take(8).collect();
        let e = Entry {
            name: name.clone(),
            left: d.pos[0] - d.size[0] * 0.5,
            top: d.pos[1] + d.size[1] * 0.5,
            right: d.pos[0] + d.size[0] * 0.5,
            bottom: d.pos[1] - d.size[1] * 0.5,
            z: d.pos[2] + 1.0,
            rot: d.rot * 0.017_453_292,
            exit: Vec3::new(d.exit[0], d.exit[1], d.exit[2] + 1.0),
            exit_rot: d.exit_rot,
            flags,
            area: d.area,
            num_peds: d.num_peds,
            time_on: on,
            time_off: off,
            link: None,
        };
        ee.entries.push(e);
    }
    // Linking by name for CREATE_LINKED_PAIR (flag 4): FindByName(name, 0, 4) = the last entry
    // with that name and without flag 4. Done once everything is loaded [I: the interior IPLs
    // load after the city ones, so the post-creation pass must do it].
    for idx in 0..ee.entries.len() {
        let (flags, name) = (ee.entries[idx].flags, ee.entries[idx].name.clone());
        if flags & 4 == 0 || name.is_empty() {
            continue;
        }
        if let Some(o) = ee.entries.iter().rposition(|x| x.flags & 4 == 0 && x.name.eq_ignore_ascii_case(&name)) {
            ee.entries[idx].link = Some(o);
            let other = &mut ee.entries[o];
            if other.link.is_none() {
                other.link = Some(idx);
            }
            other.time_on = 0;
            other.time_off = 24;
        }
    }
    let _ = ee.entries.iter().map(|e| e.num_peds).count();
    info!("entry-exits: {}", ee.entries.len());
    commands.insert_resource(ee);
}

/// `CEntryExitManager::Update` (0x440D10): the transition in progress, else detection.
#[allow(clippy::too_many_arguments)]
fn update_entry_exits(
    ee: Option<ResMut<EntryExits>>,
    mut sa: ResMut<SaPhys>,
    mut overlay: ResMut<Overlay>,
    mut cam: ResMut<ScriptCam>,
    mut walk: ResMut<ScriptWalk>,
    mut control: ResMut<PlayerControl>,
    cs: Res<Cutscene>,
    mut sacam: ResMut<crate::camera::SaCam>,
) {
    let Some(mut ee) = ee else { return };
    let ee = &mut *ee;
    let Some(pid) = sa.world.player_id() else { return };
    let veh = sa.logic::<PedLogic>(pid).and_then(|l| l.vehicle.as_ref().map(|v| v.veh));
    let blocked = cs.running() || !control.0 || ee.disabled;
    // ---- a transition in progress (TransitionFinished) ----
    if let Some(i) = ee.active {
        let done = transition_finished(ee, i, &mut sa, &mut overlay, &mut cam, &mut walk, &mut control, veh);
        if done {
            ee.active = None;
            // RestoreWithJumpCut: the follow camera starts over behind the player.
            sacam.reset_from_orbit();
        }
        return;
    }
    // ---- detection ----
    let p = sa.world.body(veh.unwrap_or(pid)).map_or(Vec3::ZERO, |b| b.phys.matrix.pos);
    let mut inside = false;
    for i in 0..ee.entries.len() {
        let e = &ee.entries[i];
        if e.flags & ENABLED == 0 || !e.is_in_area(p) {
            continue;
        }
        inside = true;
        if !blocked && transition_started(ee, i, &mut sa, &mut cam, &mut walk, &mut control, veh) {
            ee.active = Some(i);
            return;
        }
    }
    if !inside {
        ee.state = if ee.state == 3 { 4 } else { 0 };
    }
}

/// `CEntryExit::TransitionStarted` (0x43FFD0).
fn transition_started(
    ee: &mut EntryExits,
    i: usize,
    sa: &mut SaPhys,
    cam: &mut ScriptCam,
    walk: &mut ScriptWalk,
    control: &mut PlayerControl,
    veh: Option<sa_physics::world::EntityId>,
) -> bool {
    let e = ee.entries[i].clone();
    if e.flags & ENABLED == 0 || ee.state != 0 || !sa.world.clock.is_time_in_range(e.time_on, e.time_off) {
        return false;
    }
    if let Some(v) = veh {
        let b = sa.world.body(v);
        let is_car = b.is_some_and(|b| b.logic.as_any().downcast_ref::<Automobile>().is_some());
        let is_bike = b.is_some_and(|b| b.logic.as_any().downcast_ref::<Bike>().is_some());
        if (!is_car && !is_bike) || (is_car && e.flags & 0x20 == 0) || (is_bike && e.flags & 0x40 == 0) {
            return false;
        }
    } else if e.flags & 0x800 != 0 {
        return false;
    }
    let spawn = e.link.unwrap_or(i);
    // CanEnterEntryExit: alive, not entering / leaving a vehicle.
    let Some(pid) = sa.world.player_id() else { return false };
    let ready = sa.logic::<PedLogic>(pid).is_some_and(|l| l.tasks.health.health > 0.0 && l.enter.is_none() && l.leave.is_none());
    if !ready {
        return false;
    }
    ee.spawn_point = spawn;
    if let Some(l) = e.link {
        ee.entries[l].link = Some(i);
    }
    let centre = e.centre();
    let mut d = ee.entries[spawn].exit - centre;
    ee.entries[i].flags |= IN_PROGRESS;
    if e.flags & 0x202 == 0 && veh.is_none() {
        ee.far = d.length() > 10.0;
        d = d.normalize_or(Vec3::Y);
        // No door object (FindDoorNear is not ported): a far target skips the walk.
        if ee.far {
            ee.entries[i].flags |= 2;
            return true;
        }
        // CTaskComplexGotoDoorAndOpen(centre, centre + d·4) and the fixed shot behind.
        walk.0 = Some((pid, centre + d * 4.0, 4));
        cam.set_fixed_shot(centre - d * 3.0 + Vec3::Z, centre + d);
    } else if d.length() > 10.0 {
        ee.far = true;
    }
    control.0 = false;
    true
}

/// `CEntryExit::TransitionFinished` (0x4404A0): true when done.
#[allow(clippy::too_many_arguments)]
fn transition_finished(
    ee: &mut EntryExits,
    i: usize,
    sa: &mut SaPhys,
    overlay: &mut Overlay,
    cam: &mut ScriptCam,
    walk: &mut ScriptWalk,
    control: &mut PlayerControl,
    veh: Option<sa_physics::world::EntityId>,
) -> bool {
    let t = ee.spawn_point;
    let flags = ee.entries[i].flags;
    if flags & 0x202 == 0 && veh.is_none() {
        match ee.state {
            0 => {
                ee.state = 1;
                sa.world.curr_area = ee.entries[t].area;
                return false;
            }
            1 => {
                if walk.0.is_some() {
                    return false;
                }
                overlay.fade(1.0, 0);
                ee.state = 2;
                return false;
            }
            2 => {
                if overlay.fading() {
                    return false;
                }
                ee.state = 3;
            }
            _ => {}
        }
    } else {
        match ee.state {
            0 => {
                overlay.fade(0.5, 0);
                ee.state = 2;
                return false;
            }
            2 => {
                if overlay.fading() {
                    return false;
                }
                ee.state = 3;
                return false;
            }
            _ => sa.world.curr_area = ee.entries[t].area,
        }
    }
    // ---- arrival ----
    // PushEntryExit.
    let tt = ee.entries[i].link.unwrap_or(i);
    if ee.stack.last() == Some(&tt) {
        ee.stack.pop();
    } else if ee.entries[tt].area == 0 {
        ee.stack.clear();
    } else {
        ee.stack.push(i);
    }
    control.0 = true;
    walk.0 = None;
    if flags & 0x200 != 0 {
        ee.entries[t].flags &= !IN_PROGRESS;
        return true;
    }
    cam.restore();
    let pos = ee.entries[t].exit;
    let heading = ee.entries[t].exit_rot.to_radians();
    let Some(pid) = sa.world.player_id() else { return true };
    match veh {
        Some(v) => {
            if let Some(b) = sa.world.body_mut(v) {
                let base = -b.col.bbox_min.z;
                let (s, c) = heading.sin_cos();
                b.phys.matrix = sa_physics::physical::Matrix { right: Vec3::new(c, s, 0.0), fwd: Vec3::new(-s, c, 0.0), up: Vec3::Z, pos: pos - Vec3::Z + Vec3::Z * base };
                b.phys.move_speed = Vec3::ZERO;
                b.phys.turn_speed = Vec3::ZERO;
            }
        }
        None => crate::player::ped_teleport(sa, pid, g2b(pos.to_array()), Some(heading)),
    }
    overlay.fade(1.0, 1);
    ee.entries[t].flags &= !IN_PROGRESS;
    if ee.entries[t].flags & 0x8000 != 0 {
        ee.entries[t].flags &= !ENABLED;
    }
    true
}
