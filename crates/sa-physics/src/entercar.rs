//! Getting into a car (enter_exit.md §2–§4), the player as driver: the car search
//! (`FindClosestCarSectorList` / `EvaluateCarPosition`), `GetNearestCarDoor`,
//! `GetPositionToOpenCarDoor` from the vehicle anim groups' door offsets, and
//! CTaskComplexEnterCar's graph: go to the door (800), align (801), open the door (802),
//! get in (807), close the door from inside (805), shuffle across (808), set in as driver (812).
//! The ped's root and heading during the anims come from `CTaskUtilityLineUpPedWithCar`
//! (§4.7) and the door angle from `CVehicle::ProcessOpenDoor` (§4.6).
//!
//! Getting out (§6): CTaskComplexLeaveCar's wait-to-slow-down (809), get out (813) with the
//! line-up moving the ped from the seat to the door point, close the door from outside (806)
//! and set the ped out (816).
//!
//! Not ported: carjacking, NPC entering / leaving, passengers, bikes / quads / boats (still
//! warped), locked doors, convertibles' jump-in, the point route around a blocking car, the
//! quit-by-stick test, the sphere / ceiling parts of IsRoomForPedToLeaveCar, jumping out of
//! a moving car (814), crawling out of an upside-down car, PositionPedOutOfCollision.

use glam::{Quat, Vec3};
use sa_formats::vehicle::VehicleAnimGroup;

use crate::{
    anim::{AnimManager, af},
    automobile::Automobile,
    ped::{PedLogic, limit_radian_angle},
    physical::{EntityType, Matrix, Status, ef},
    world::{EntityId, LosOpts, World},
};

/// Door ids are the car-node ids: 10 front left (driver), 8 front right, 11 rear left,
/// 9 rear right.
pub const DOOR_FL: u8 = 10;
pub const DOOR_FR: u8 = 8;

/// `eDoors` of a door node.
pub fn e_door(node: u8) -> usize {
    match node {
        8 => 3,
        9 => 5,
        10 => 2,
        _ => 4,
    }
}

/// `CVehicleAnimGroup::GetGroup(animId)` (0x6E3B00).
pub fn anim_group_of(g: &VehicleAnimGroup, id: i16) -> usize {
    let bit = match id {
        351..=354 => 0,
        355 | 356 => 1,
        357 | 358 => 2,
        359 | 360 | 363 => 3,
        361 | 362 => 4,
        364..=366 => 5,
        367 | 368 => 6,
        369 | 370 => 7,
        371 | 372 => 8,
        373 | 374 => 9,
        375 | 376 => 10,
        378 | 379 => 11,
        380 | 381 => 12,
        382 | 383 => 13,
        384 | 385 => 14,
        386 => 15,
        387 | 388 => 16,
        389 | 390 => 17,
        _ => 31,
    };
    (if g.second_mask & (1u32 << bit) != 0 { g.second_group } else { g.first_group }) as usize
}

/// `GetZBlendTime(animId)` (0x6E3C80).
fn z_blend_time(g: &VehicleAnimGroup, id: i16) -> f32 {
    match id {
        351..=363 => g.z_times[0],
        373..=376 => g.z_times[2],
        378 | 379 => g.z_times[3],
        384 | 385 => g.z_times[1],
        387 | 388 => g.z_times[4],
        _ => 0.0,
    }
}

/// `Z(T)` of the line-up blend table.
fn z_ramp(t: f32, p: f32) -> f32 {
    let a = t.abs();
    if t > 0.0 {
        if p >= a { 1.0 } else { p / a }
    } else if p > a {
        (p - a) / (1.0 - a)
    } else {
        0.0
    }
}

/// What the enter code reads from a car.
#[derive(Debug, Clone)]
pub struct CarDoorInfo {
    pub m: Matrix,
    pub move_speed: Vec3,
    pub front: Vec3,
    pub rear: Vec3,
    /// handling +0xD4 SeatOffsetDistance.
    pub seat_dist: f32,
    pub g: VehicleAnimGroup,
    pub model_flags: u32,
    /// `GetDistanceFromCentreOfMassToBaseOfModel`.
    pub com_to_base: f32,
    pub has_driver: bool,
}

/// Root translation at the end of an anim (`ComputeAnimDoorOffsets`, 0x6E3D10).
pub fn anim_end_offset(m: &AnimManager, group: usize, id: i16) -> Vec3 {
    m.get(group, id).map_or(Vec3::ZERO, |(h, _)| h.root_end_translation())
}

impl CarDoorInfo {
    /// `ComputeAnimDoorOffsets(idx)`: 0 front get-in, 1 rear, 2 bike kick, 3/4 get-out, 5/6 jacked.
    fn door_offset(&self, m: &AnimManager, idx: usize) -> Vec3 {
        let id = [359, 361, 363, 373, 375, 378, 379][idx.min(6)];
        anim_end_offset(m, anim_group_of(&self.g, id), id)
    }

    /// `GetPositionToOpenCarDoor(veh, door)` (0x64E740), car path.
    pub fn door_position(&self, m: &AnimManager, door: u8) -> Vec3 {
        let dist = if self.g.first_group == 101 && matches!(door, 9 | 11) { 0.0 } else { self.seat_dist };
        let idx = match door {
            8 | 10 => 0,
            9 | 11 => 1,
            _ => 2,
        };
        let mut a = self.door_offset(m, idx);
        let mut s;
        match door {
            8 | 9 => {
                s = if door == 8 { self.front } else { self.rear };
                s.x += dist;
                a.x = -a.x;
            }
            10 | 11 => {
                s = if door == 10 { self.front } else { self.rear };
                s.x = -(s.x + dist);
            }
            _ => {
                s = self.front;
                a = Vec3::ZERO;
            }
        }
        let mut o = s - a;
        if self.model_flags & 8 != 0 {
            o.z = 0.95 - self.com_to_base;
        }
        self.m.pos + self.m.rotate(o)
    }

    /// `GetLocalTarget(veh, xyBlend, assoc)` (0x64FC10) for the enter anims.
    fn local_target(&self, m: &AnimManager, door: u8, xy: f32) -> Vec3 {
        let van_rear = self.g.first_group == 101 && matches!(door, 9 | 11);
        let dist = if van_rear { 0.0 } else { xy * self.seat_dist };
        let idx = match door {
            8 | 10 => 0,
            9 | 11 => 1,
            _ => 2,
        };
        let mut a = self.door_offset(m, idx);
        match door {
            8 | 9 => {
                let mut s = if door == 8 { self.front } else { self.rear };
                s.x += dist;
                a.x = -a.x;
                s - a
            }
            10 | 11 => {
                let mut s = if door == 10 { self.front } else { self.rear };
                s.x = -(s.x + dist);
                s - a
            }
            18 => self.front + a,
            _ => self.front,
        }
    }
}

/// `CTaskUtilityLineUpPedWithCar` (0x6513A0), type 0 (enter).
#[derive(Debug, Clone)]
pub struct LineUp {
    offset: Vec3,
    end_time: u32,
    door: u8,
}

/// CTaskComplexEnterCar stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 800 GoToCarDoorAndStandStill (run).
    GoTo,
    /// 801 Align, 802 OpenDoorFromOutside, 820 SlowDragPedOut, 807 GetIn,
    /// 805 CloseDoorFromInside, 808 Shuffle.
    Align,
    Open,
    Jack,
    GetIn,
    CloseIn,
    Shuffle,
}

/// CTaskComplexEnterCarAsDriver (701) for the player.
#[derive(Debug, Clone)]
pub struct EnterCar {
    pub veh: EntityId,
    pub door: u8,
    pub stage: Stage,
    pub target: Vec3,
    start_ms: u32,
    anim: Option<u32>,
    anim_id: i16,
    /// The door's open ratio when the open anim started (+0x20).
    init_ratio: f32,
    line_up: Option<LineUp>,
    /// The go-to stage reached the door point / the player quit.
    pub reached: bool,
    pub cancel: bool,
    /// The go-to give-up time (30 s; 10 s for CTaskComplexEnterCarAsDriverTimed).
    pub timeout_ms: u32,
}

impl EnterCar {
    pub fn lined_up(&self) -> bool {
        self.stage != Stage::GoTo
    }

    pub fn started_ms(&self) -> u32 {
        self.start_ms
    }
}

fn heading_of(m: &Matrix) -> f32 {
    (-m.fwd.x).atan2(m.fwd.y)
}

impl World {
    /// The door data of an Automobile (None for other vehicles).
    pub fn car_door_info(&self, veh: EntityId) -> Option<CarDoorInfo> {
        let b = self.body(veh)?;
        let c = b.logic.as_any().downcast_ref::<Automobile>()?;
        let g = self.veh_anim_groups.get(c.h.anim_group as usize).cloned().unwrap_or_default();
        Some(CarDoorInfo {
            m: b.phys.matrix,
            move_speed: b.phys.move_speed,
            front: c.seat_front,
            rear: c.seat_rear,
            seat_dist: c.h.seat_offset,
            g,
            model_flags: c.h.model_flags,
            com_to_base: -b.col.bbox_min.z,
            has_driver: self.driver_of(veh).is_some(),
        })
    }

    /// The ped sitting in `veh` as driver.
    pub fn driver_of(&self, veh: EntityId) -> Option<EntityId> {
        if let Some(c) = self.body(veh).and_then(|b| b.logic.as_any().downcast_ref::<Automobile>()) {
            return c.driver;
        }
        self.body_ids().into_iter().find(|&id| {
            self.body(id)
                .and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>())
                .is_some_and(|p| p.vehicle.as_ref().is_some_and(|v| v.veh == veh))
        })
    }

    /// `ComputeSlowJackedPed` (0x64F070): the occupant of the door's seat (front right: the
    /// passenger, else the driver dragged across) who is still sitting.
    fn jack_target(&self, veh: EntityId, door: u8, jacker: EntityId) -> Option<EntityId> {
        let c = self.body(veh)?.logic.as_any().downcast_ref::<Automobile>()?;
        let seated = |p: &Option<EntityId>| {
            p.filter(|&id| {
                id != jacker
                    && self
                        .body(id)
                        .and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>())
                        .is_some_and(|pl| pl.vehicle.as_ref().is_some_and(|v| v.veh == veh) && pl.leave.is_none())
            })
        };
        match door {
            DOOR_FL => seated(&c.driver),
            DOOR_FR => seated(&c.passengers[0]).or_else(|| seated(&c.driver)),
            _ => None,
        }
    }

    /// `IsRoomForPedToLeaveCar` (0x6504C0), main line-of-sight part: from the seat to 0.35 m
    /// past the door point, buildings and objects.
    fn is_room_for_door(&mut self, info: &CarDoorInfo, m: &AnimManager, door: u8, veh: EntityId) -> bool {
        let mut seat = if matches!(door, 9 | 11) { info.rear } else { info.front };
        if matches!(door, 10 | 11) {
            seat.x = -seat.x;
        }
        let mut sw = info.m.transform(seat);
        let mut dp = info.door_position(m, door);
        if info.m.up.z <= 0.0 {
            sw.z += 0.5;
            dp.z += 0.5;
        }
        let v = (dp - sw).truncate();
        let l = v.length().max(1e-4);
        let end = (sw.truncate() + v * ((l + 0.35) / l)).extend(dp.z);
        let o = LosOpts { bodies: true, peds: false, ignore: Some(veh), ..Default::default() };
        self.process_line_of_sight(sw, end, &o).is_none_or(|(id, _, _)| self.body(id).is_some_and(|b| b.phys.kind == EntityType::Vehicle))
    }

    /// `GetNearestCarDoor` (0x6528F0) for the player entering an empty car as driver.
    fn nearest_car_door(&mut self, ped_pos: Vec3, veh: EntityId, info: &CarDoorInfo, m: &AnimManager) -> Option<(u8, Vec3)> {
        let p10 = info.door_position(m, DOOR_FL);
        let p8 = info.door_position(m, DOOR_FR);
        let d10 = (p10 - ped_pos).truncate().length_squared();
        let d8 = (p8 - ped_pos).truncate().length_squared();
        let front_doors_wide = info.model_flags & 0x2 != 0;
        if front_doors_wide || d8 > d10 {
            if self.is_room_for_door(info, m, DOOR_FL, veh) {
                return Some((DOOR_FL, p10));
            }
            if self.is_room_for_door(info, m, DOOR_FR, veh) {
                return Some((DOOR_FR, p8));
            }
            None
        } else if self.is_room_for_door(info, m, DOOR_FR, veh) {
            Some((DOOR_FR, p8))
        } else if self.is_room_for_door(info, m, DOOR_FL, veh) {
            Some((DOOR_FL, p10))
        } else {
            None
        }
    }

    /// `CPlayerInfo::Process` enter trigger: the best vehicle in the 10 m box
    /// (`FindClosestCarSectorList` + `EvaluateCarPosition`).
    pub fn find_car_to_enter(&self, ped: EntityId) -> Option<EntityId> {
        let pb = self.body(ped)?;
        let (pp, pf) = (pb.phys.matrix.pos, pb.phys.matrix.fwd);
        let mut best = 0.0f32;
        let mut car = None;
        for id in self.body_ids() {
            let Some(b) = self.body(id) else { continue };
            if b.phys.kind != EntityType::Vehicle || !b.phys.has_e(ef::USES_COLLISION) || b.phys.status == Status::Wrecked {
                continue;
            }
            let d = b.phys.matrix.pos - pp;
            if d.x.abs() > 10.0 || d.y.abs() > 10.0 {
                continue;
            }
            let is_bike = b.logic.as_any().is::<crate::bike::Bike>();
            if b.phys.matrix.up.z <= 0.3 && !is_bike {
                continue;
            }
            let vz = b.phys.matrix.pos.z + b.col.bbox_min.z + 1.0;
            let is_boat = b.logic.as_any().is::<crate::boat::Boat>();
            let ok = (pp.z - vz).abs() < 2.0 || (is_boat && vz < pp.z && pp.z - 4.0 < vz);
            if !ok {
                continue;
            }
            let dist = d.truncate().length();
            if dist > 10.0 {
                continue;
            }
            // EvaluateCarPosition (0x56DAD0).
            let mut a = pf.x.atan2(pf.y) - d.x.atan2(d.y);
            while a > std::f32::consts::PI {
                a -= std::f32::consts::TAU;
            }
            while a < -std::f32::consts::PI {
                a += std::f32::consts::TAU;
            }
            let score = (10.0 - dist) * (1.0 - a.abs() * 0.159_154_94);
            if score >= best {
                best = score;
                car = Some(id);
            }
        }
        car
    }

    /// 702 CTaskComplexEnterCarAsDriverTimed: the enter task with a 10 s limit.
    pub fn start_enter_car_timed(&mut self, ped: EntityId, veh: EntityId) {
        if self.start_enter_car(ped, veh) {
            if let Some(e) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| p.enter.as_mut()) {
                e.timeout_ms = 10000;
            }
        }
    }

    /// Start CTaskComplexEnterCarAsDriver for the player. Returns false when no door can be
    /// used (or the vehicle is not a car: the caller seats the ped directly then).
    pub fn start_enter_car(&mut self, ped: EntityId, veh: EntityId) -> bool {
        let Some(m) = self.body(ped).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).and_then(|p| p.tasks.anims.clone()) else {
            return false;
        };
        let Some(info) = self.car_door_info(veh) else { return false };
        let Some(pp) = self.body(ped).map(|b| b.phys.matrix.pos) else { return false };
        let Some((door, target)) = self.nearest_car_door(pp, veh, &info, &m) else { return false };
        let now = self.now_ms;
        if let Some(p) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
            p.enter = Some(EnterCar {
                veh,
                door,
                stage: Stage::GoTo,
                target,
                start_ms: now,
                anim: None,
                anim_id: 0,
                init_ratio: 0.0,
                line_up: None,
                reached: false,
                cancel: false,
                timeout_ms: 30000,
            });
            return true;
        }
        false
    }

    /// `CVehicle::ProcessOpenDoor(ped, node, grp, id, t)` (0x6D56C0) + `OpenDoor`.
    fn process_open_door(&mut self, veh: EntityId, door: u8, id: i16, t: f32) {
        let Some(c) = self.body_mut(veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) else { return };
        let e = e_door(door);
        if c.damage.dm.doors[e] == 4 {
            return;
        }
        let gi = c.h.anim_group as usize;
        let ratio_now = |c: &Automobile| {
            let d = &c.damage.doors[e];
            if d.open_angle != 0.0 { d.angle / d.open_angle } else { 0.0 }
        };
        let g = self.veh_anim_groups.get(gi).cloned().unwrap_or_default();
        let (s, f, kind) = match id {
            355..=358 => (g.door_start[0], g.door_stop[0], 0),
            367..=370 => (g.door_start[1], g.door_stop[1], 1),
            373..=376 => (g.door_start[2], g.door_stop[2], 0),
            380..=383 => (g.door_start[3], g.door_stop[3], 1),
            _ => return,
        };
        let Some(c) = self.body_mut(veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) else { return };
        let cur = ratio_now(c);
        let r = if kind == 0 {
            if t > s && t < f {
                let r = (t - s) / (f - s);
                if r <= cur {
                    return;
                }
                r
            } else if t >= f {
                1.0
            } else {
                0.0
            }
        } else if t > s && t < f {
            let r = 1.0 - (t - s) / (f - s);
            if r >= cur {
                return;
            }
            r
        } else if t >= f {
            0.0
        } else {
            1.0
        };
        open_door(c, e, r);
    }

    /// The entering ped's anims and stages after the vehicles moved (`SetPedPosition` of the
    /// enter sub-tasks: the line-up utility).
    pub(crate) fn process_peds_entering(&mut self, ts: f32) {
        // The go-to stage: quit, or reached the door → line up.
        let going: Vec<(EntityId, bool, bool)> = self
            .body_ids()
            .into_iter()
            .filter_map(|id| {
                let e = self.body(id)?.logic.as_any().downcast_ref::<PedLogic>()?.enter.as_ref()?;
                (e.stage == Stage::GoTo).then_some((id, e.reached, e.cancel))
            })
            .collect();
        for (id, reached, cancel) in going {
            if cancel {
                self.cancel_enter(id);
            } else if reached {
                self.begin_line_up(id);
            }
        }
        let entering: Vec<EntityId> = self
            .body_ids()
            .into_iter()
            .filter(|&id| {
                self.body(id)
                    .and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>())
                    .is_some_and(|p| p.enter.as_ref().is_some_and(|e| e.lined_up()))
            })
            .collect();
        for ped in entering {
            self.process_ped_entering(ped, ts);
        }
    }

    fn process_ped_entering(&mut self, ped: EntityId, ts: f32) {
        let now = self.now_ms;
        let Some((mut e, m)) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| Some((p.enter.take()?, p.tasks.anims.clone()?)))
        else {
            return;
        };
        let Some(info) = self.car_door_info(e.veh) else {
            self.cancel_enter(ped);
            return;
        };
        // Speed abort (0.2 for the player).
        if info.move_speed.truncate().length() > 0.2 {
            self.release_door(e.veh, e.door);
            self.cancel_enter(ped);
            return;
        }
        // Anim progress (UpdateAnim).
        let done = {
            let Some(b) = self.body_mut(ped) else { return };
            let Some(pl) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return };
            let Some(clump) = pl.clump.as_deref_mut() else { return };
            pl.prev_pose.clone_from(&clump.pose);
            clump.update(ts * 0.02);
            e.anim.is_none_or(|u| clump.finished.contains(&u) || clump.by_uid(u).is_none_or(|a| a.is_finished()))
        };
        // CreateNextSubTask (0x63E990).
        if done {
            let next = match e.stage {
                // AfterAlign (0x63F970): open a door that is not fully open, jack an occupant,
                // else get in.
                Stage::Align => {
                    let has_door_to_open = self.car_door_state_full(e.veh, e.door).is_some_and(|(missing, full)| !missing && !full);
                    Some(if has_door_to_open {
                        Stage::Open
                    } else if self.jack_target(e.veh, e.door, ped).is_some() {
                        Stage::Jack
                    } else {
                        Stage::GetIn
                    })
                }
                Stage::Open if self.jack_target(e.veh, e.door, ped).is_some() => Some(Stage::Jack),
                Stage::Open | Stage::Jack => Some(Stage::GetIn),
                Stage::GetIn => Some(Stage::CloseIn),
                Stage::CloseIn if e.door == DOOR_FR => Some(Stage::Shuffle),
                Stage::CloseIn | Stage::Shuffle => None,
                Stage::GoTo => Some(Stage::Align),
            };
            match next {
                Some(st) => self.start_stage(ped, &mut e, st, &info, &m),
                None => {
                    // 812 CTaskSimpleCarSetPedInAsDriver.
                    self.release_door(e.veh, e.door);
                    self.set_ped_in_car_direct(ped, e.veh, info.front);
                    // 702: 827 (join the road, cruise 10) then 726 FleeScene (CRUISE,
                    // AVOID_CARS, cruise 40).
                    let npc = self.body(ped).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).is_some_and(|p| !p.is_player);
                    if npc {
                        self.join_car_with_road_system(e.veh, 1, 2, 40);
                    }
                    return;
                }
            }
        }
        // The door angle from the anim time.
        if matches!(e.stage, Stage::Open | Stage::CloseIn) {
            let id = e.anim_id;
            let tt = self.anim_time(ped, e.anim);
            if e.stage == Stage::Open && tt < info.g.door_start[0] {
                // A half-open door is pushed shut first.
                let r = (1.0 - tt / info.g.door_start[0].max(1e-4)) * e.init_ratio;
                if let Some(c) = self.body_mut(e.veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
                    open_door(c, e_door(e.door), r);
                }
            } else {
                self.process_open_door(e.veh, e.door, id, tt);
            }
        }
        // CTaskUtilityLineUpPedWithCar::ProcessPed.
        self.line_up_ped(ped, &e, &info, &m, now);
        if let Some(b) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
            b.enter = Some(e);
        }
    }

    /// (missing, fully open: `|angle| >= |openAngle| − 0.5`) of a door.
    fn car_door_state_full(&self, veh: EntityId, door: u8) -> Option<(bool, bool)> {
        let c = self.body(veh)?.logic.as_any().downcast_ref::<Automobile>()?;
        let e = e_door(door);
        let d = &c.damage.doors[e];
        Some((c.damage.dm.doors[e] == 4, d.angle.abs() >= d.open_angle.abs() - 0.5))
    }

    fn anim_time(&self, ped: EntityId, uid: Option<u32>) -> f32 {
        self.body(ped)
            .and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>())
            .and_then(|p| p.clump.as_deref())
            .and_then(|c| uid.and_then(|u| c.by_uid(u)))
            .map_or(0.0, |a| a.time)
    }

    /// Start a stage's anim (`StartAnim` of 801/802/807/805/808).
    fn start_stage(&mut self, ped: EntityId, e: &mut EnterCar, stage: Stage, info: &CarDoorInfo, m: &AnimManager) {
        let left = matches!(e.door, 10 | 11);
        let (id, delta) = match stage {
            Stage::Align => {
                let dz = (info.door_position(m, e.door).z - self.body(ped).map_or(0.0, |b| b.phys.matrix.pos.z)).max(0.0);
                let base = if dz <= 4.4 { 351 } else { 353 };
                (if left { base } else { base + 1 }, 4.0)
            }
            Stage::Open => (if e.door == 10 { 355 } else if e.door == 8 { 356 } else if e.door == 11 { 357 } else { 358 }, 4.0),
            // 820 CTaskSimpleCarSlowDragPedOut: CAR_pullout_LHS / RHS.
            Stage::Jack => (if left { 364 } else { 365 }, 4.0),
            Stage::GetIn => (if e.door == 10 { 359 } else if e.door == 8 { 360 } else if e.door == 11 { 361 } else { 362 }, 4.0),
            Stage::CloseIn => (if e.door == 10 { 367 } else if e.door == 8 { 368 } else if e.door == 11 { 369 } else { 370 }, 1000.0),
            Stage::Shuffle => (372, 1000.0),
            Stage::GoTo => return,
        };
        // 805: nothing to close (the door is closed or missing) → skip to the next stage.
        if stage == Stage::CloseIn {
            let st = self.car_door_state(e.veh, e.door);
            if st.is_none_or(|(_, missing, closed)| missing || closed) {
                if e.door == DOOR_FR {
                    return self.start_stage(ped, e, Stage::Shuffle, info, m);
                }
                e.stage = Stage::CloseIn;
                e.anim = None;
                e.anim_id = 0;
                return;
            }
        }
        if stage == Stage::Open {
            e.init_ratio = self.car_door_state(e.veh, e.door).map_or(0.0, |(r, _, _)| r);
        }
        if stage == Stage::Jack {
            // The victim gets 824 CTaskComplexCarSlowBeDraggedOut (823: CAR_jacked).
            if let Some(v) = self.jack_target(e.veh, e.door, ped) {
                self.start_be_dragged_out(v, e.veh, e.door, ped);
            }
        }
        let grp = anim_group_of(&info.g, id);
        let uid = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| {
            let c = p.clump.as_deref_mut()?;
            c.blend_animation(m, grp, id, delta).map(|i| {
                c.assocs[i].finish_cb = true;
                c.assocs[i].uid
            })
        });
        e.stage = stage;
        e.anim = uid;
        e.anim_id = id;
    }

    /// (open ratio, missing, closed) of a door.
    fn car_door_state(&self, veh: EntityId, door: u8) -> Option<(f32, bool, bool)> {
        let c = self.body(veh)?.logic.as_any().downcast_ref::<Automobile>()?;
        let e = e_door(door);
        let d = &c.damage.doors[e];
        let r = if d.open_angle != 0.0 { d.angle / d.open_angle } else { 0.0 };
        Some((r, c.damage.dm.doors[e] == 4, d.angle == d.closed_angle))
    }

    fn release_door(&mut self, veh: EntityId, door: u8) {
        if let Some(c) = self.body_mut(veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
            c.doors_in_use &= !(1 << e_door(door));
        }
    }

    /// Abort: the ped stands where it is (collision back on).
    pub fn cancel_enter(&mut self, ped: EntityId) {
        if let Some(b) = self.body_mut(ped) {
            b.phys.eflags = (b.phys.eflags & !ef::IS_STATIC) | ef::USES_COLLISION;
            if let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() {
                let e = p.enter.take();
                if let (Some(c), Some(m), Some(e)) = (p.clump.as_deref_mut(), p.tasks.anims.clone(), e) {
                    if let Some(a) = e.anim.and_then(|u| c.by_uid_mut(u)) {
                        a.flags |= af::DELETE_BLENDED_OUT;
                        a.blend_delta = -4.0;
                    }
                    c.blend_animation(&m, p.tasks.anim_group, crate::anim::anim_id::IDLE, 4.0);
                }
            }
        }
    }

    /// The go-to stage reached the door: register the door use, collision off, create the
    /// line-up and start the align (cars).
    pub(crate) fn begin_line_up(&mut self, ped: EntityId) {
        let Some((mut e, m)) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| Some((p.enter.take()?, p.tasks.anims.clone()?)))
        else {
            return;
        };
        let Some(info) = self.car_door_info(e.veh) else { return };
        let pos = self.body(ped).map_or(Vec3::ZERO, |b| b.phys.matrix.pos);
        let dp = info.door_position(&m, e.door);
        e.line_up = Some(LineUp { offset: dp - pos, end_time: self.now_ms + 600, door: e.door });
        if let Some(c) = self.body_mut(e.veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
            c.doors_in_use |= 1 << e_door(e.door);
        }
        if let Some(b) = self.body_mut(ped) {
            b.phys.eflags = (b.phys.eflags | ef::IS_STATIC) & !ef::USES_COLLISION;
            b.phys.move_speed = Vec3::ZERO;
            if let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() {
                p.standing = false;
                p.anim_velocity = glam::Vec2::ZERO;
                // FixHeading (automobiles): turn a nearly parallel heading toward the car.
                let mut r = info.m.right;
                if matches!(e.door, 10 | 11) {
                    r = -r;
                }
                let f = b.phys.matrix.fwd;
                let d = r.dot(f);
                if d > 0.0 && d <= 0.1 {
                    let f2 = f - r * (2.0 * d);
                    let h = limit_radian_angle(crate::pedtask::radian_angle_between_points(f2.x, f2.y, 0.0, 0.0));
                    p.cur_rot = h;
                    p.aim_rot = h;
                }
            }
        }
        self.start_stage(ped, &mut e, Stage::Align, &info, &m);
        if let Some(p) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
            p.enter = Some(e);
        }
    }

    /// `CTaskUtilityLineUpPedWithCar::ProcessPed` (type 0).
    fn line_up_ped(&mut self, ped: EntityId, e: &EnterCar, info: &CarDoorInfo, m: &AnimManager, now: u32) {
        let Some(lu) = e.line_up.as_ref() else { return };
        let (id, p) = {
            let Some(pl) = self.body(ped).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()) else { return };
            let a = pl.clump.as_deref().and_then(|c| e.anim.and_then(|u| c.by_uid(u)));
            (a.map(|a| a.id), a.map_or(0.0, |a| (a.time / a.hier.total_length.max(1e-6)).clamp(0.0, 1.0)))
        };
        let t = id.map_or(0.0, |id| z_blend_time(&info.g, id));
        let (xy, zb) = match id {
            None => (0.0, 0.0),
            Some(351..=354) => (1.0, 0.0),
            Some(355..=358) => (1.0, if t.abs() > 10.0 { 1.0 } else { 0.0 }),
            Some(364 | 365) => (1.0, 0.0),
            Some(359..=363) => (1.0 - p, if t.abs() > 10.0 { 1.0 } else { z_ramp(t, p) }),
            Some(367..=372) => (0.0, 1.0),
            Some(_) => (0.0, 0.0),
        };
        let aimed = heading_of(&info.m);
        let world_target = |x: f32| info.m.pos + info.m.rotate(info.local_target(m, lu.door, x));
        let mut t1 = world_target(xy);
        let mut t2 = world_target(1.0);
        // The ground at the door point (+1.0 to the ped root), not below the car's floor.
        let lat = (t2 - info.m.pos).dot(info.m.right);
        let base = info.m.pos.z - info.com_to_base + info.m.right.z * lat + 1.0;
        let o = LosOpts { bodies: false, peds: false, ..Default::default() };
        if let Some((_, _, cp)) = self.process_line_of_sight(t2 + Vec3::new(0.0, 0.0, 1.0), t2 - Vec3::new(0.0, 0.0, 4.0), &o) {
            t2.z = cp.point.z + 1.0;
        }
        if base - 0.5 > t2.z {
            t2.z = base;
        }
        let z_ref = t2.z;
        t1.z = z_ref + zb * (t1.z - z_ref);
        let Some(b) = self.body_mut(ped) else { return };
        let Some(pl) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return };
        b.phys.move_speed = Vec3::ZERO;
        let mut heading = aimed;
        if now < lu.end_time {
            let k = (lu.end_time - now) as f32 * (1.0 / 600.0);
            t1 -= Vec3::new(lu.offset.x, lu.offset.y, 0.0) * k;
            let mut h = limit_radian_angle(aimed);
            let cur = pl.cur_rot;
            if h - cur > std::f32::consts::PI {
                h -= std::f32::consts::TAU;
            } else if cur - h > std::f32::consts::PI {
                h += std::f32::consts::TAU;
            }
            heading = cur - (cur - h) * (1.0 - k);
        }
        pl.cur_rot = heading;
        pl.aim_rot = heading;
        if matches!(id, Some(359..=363)) {
            // Get in: slerp the body toward the car's orientation.
            let qv = Quat::from_mat3(&glam::Mat3::from_cols(info.m.right, info.m.fwd, info.m.up));
            let pm = b.phys.matrix;
            let qp = Quat::from_mat3(&glam::Mat3::from_cols(pm.right, pm.fwd, pm.up));
            let q = if p <= 0.0 { qp } else if p >= 1.0 { qv } else { qp.slerp(qv, p) };
            let r = glam::Mat3::from_quat(q.normalize());
            b.phys.matrix = Matrix { right: r.x_axis, fwd: r.y_axis, up: r.z_axis, pos: t1 };
        } else if xy <= 0.2 && info.m.up.z > -0.8 {
            // Attached to the car (close door, shuffle).
            let mut mm = info.m;
            mm.pos += info.m.rotate(info.local_target(m, lu.door, 0.0));
            b.phys.matrix = mm;
        } else {
            b.phys.matrix.pos = t1;
            crate::ped::set_heading(&mut b.phys.matrix, heading);
        }
    }
}

/// CTaskComplexLeaveCar stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveStage {
    /// 719 / 809 CarWaitToSlowDown (the car brakes).
    Wait,
    /// 813 GetOut, 806 CloseDoorFromOutside.
    GetOut,
    Close,
}

/// CTaskComplexLeaveCar (704) for the player.
#[derive(Debug, Clone)]
pub struct LeaveCar {
    pub veh: EntityId,
    pub door: u8,
    pub stage: LeaveStage,
    anim: Option<u32>,
    anim_id: i16,
    /// 813 +9: the door was there to open.
    door_to_open: bool,
    /// 823 CAR_jacked (dragged out by `jacker`).
    pub jacked_by: Option<EntityId>,
}

impl LeaveCar {
    /// The ped no longer sits in the seat (the get-out line-up places it).
    pub fn out_of_seat(&self) -> bool {
        self.stage != LeaveStage::Wait
    }
}

/// `CVehicle::CanPedStepOutCar(false)` (0x6D1F30).
pub fn can_ped_step_out_car(m: &Matrix, v: Vec3, w: Vec3, is_boat: bool) -> bool {
    if m.up.z.abs() <= 0.1 {
        return v.z.abs() <= 0.05 && v.truncate().length() <= 0.01 && w.length_squared() <= 0.0004;
    }
    if is_boat {
        return true;
    }
    if v.truncate().length() > 0.01 || v.z.abs() > 0.05 {
        return false;
    }
    w.length_squared() <= 0.0004
}

impl World {
    /// The player presses enter in a car: CTaskComplexLeaveCar(veh, 0, 0, sensible, false).
    /// Returns false for other vehicles (the caller warps the ped out).
    pub fn start_leave_car(&mut self, ped: EntityId) -> bool {
        let Some(veh) = self.body(ped).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).and_then(|p| p.vehicle.as_ref().map(|v| v.veh)) else {
            return false;
        };
        if self.car_door_info(veh).is_none() {
            return false;
        }
        if let Some(p) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
            if p.leave.is_some() {
                return true;
            }
            // ComputeTargetDoorToExit: the driver leaves by door 10.
            p.leave = Some(LeaveCar { veh, door: DOOR_FL, stage: LeaveStage::Wait, anim: None, anim_id: 0, door_to_open: false, jacked_by: None });
        }
        true
    }

    /// 824 CTaskComplexCarSlowBeDraggedOut → 823: the CAR_jackedLHS / RHS anim from the seat,
    /// lined up with the door like a get-out.
    fn start_be_dragged_out(&mut self, victim: EntityId, veh: EntityId, door: u8, jacker: EntityId) {
        let Some(info) = self.car_door_info(veh) else { return };
        let id = if matches!(door, 10 | 11) { 378 } else { 379 };
        let grp = anim_group_of(&info.g, id);
        let Some(p) = self.body_mut(victim).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) else { return };
        let Some(m) = p.tasks.anims.clone() else { return };
        p.enter = None;
        let uid = p.clump.as_deref_mut().and_then(|c| {
            for a in &mut c.assocs {
                a.flags |= af::DELETE_BLENDED_OUT;
                a.blend_delta = -1000.0;
            }
            c.blend_animation(&m, grp, id, 1000.0).map(|i| {
                c.assocs[i].finish_cb = true;
                c.assocs[i].uid
            })
        });
        p.leave = Some(LeaveCar { veh, door, stage: LeaveStage::GetOut, anim: uid, anim_id: id, door_to_open: false, jacked_by: Some(jacker) });
    }

    /// The leaving ped's tasks after the vehicles moved.
    pub(crate) fn process_peds_leaving(&mut self, ts: f32) {
        let leaving: Vec<EntityId> = self
            .body_ids()
            .into_iter()
            .filter(|&id| self.body(id).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).is_some_and(|p| p.leave.is_some()))
            .collect();
        for ped in leaving {
            self.process_ped_leaving(ped, ts);
        }
    }

    fn process_ped_leaving(&mut self, ped: EntityId, ts: f32) {
        let Some((mut lv, m)) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| Some((p.leave.take()?, p.tasks.anims.clone()?)))
        else {
            return;
        };
        let Some(info) = self.car_door_info(lv.veh) else {
            self.set_ped_out(ped, &lv);
            return;
        };
        match lv.stage {
            LeaveStage::Wait => {
                // 809: done when the car can be stepped out of.
                let w = self.body(lv.veh).map_or(Vec3::ZERO, |b| b.phys.turn_speed);
                if can_ped_step_out_car(&info.m, info.move_speed, w, false) {
                    // 813 StartAnim: RemoveCarSitAnim, BlendAnimation(…, 1000).
                    let id = match lv.door {
                        10 => 373,
                        8 => 374,
                        11 => 375,
                        _ => 376,
                    };
                    let grp = anim_group_of(&info.g, id);
                    let uid = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| {
                        let c = p.clump.as_deref_mut()?;
                        for a in &mut c.assocs {
                            a.flags |= af::DELETE_BLENDED_OUT;
                            a.blend_delta = -1000.0;
                        }
                        c.blend_animation(&m, grp, id, 1000.0).map(|i| {
                            c.assocs[i].finish_cb = true;
                            c.assocs[i].uid
                        })
                    });
                    lv.anim = uid;
                    lv.anim_id = id;
                    lv.door_to_open = self.car_door_state_full(lv.veh, lv.door).is_some_and(|(missing, full)| !missing && !full);
                    lv.stage = LeaveStage::GetOut;
                    if let Some(c) = self.body_mut(lv.veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
                        c.doors_in_use |= 1 << e_door(lv.door);
                    }
                }
            }
            LeaveStage::GetOut | LeaveStage::Close => {
                let (done, t, p) = {
                    let Some(b) = self.body_mut(ped) else { return };
                    let Some(pl) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return };
                    let Some(clump) = pl.clump.as_deref_mut() else { return };
                    pl.prev_pose.clone_from(&clump.pose);
                    clump.update(ts * 0.02);
                    let a = lv.anim.and_then(|u| clump.by_uid(u));
                    let t = a.map_or(0.0, |a| a.time);
                    let p = a.map_or(1.0, |a| (a.time / a.hier.total_length.max(1e-6)).clamp(0.0, 1.0));
                    let done = lv.anim.is_none_or(|u| clump.finished.contains(&u) || clump.by_uid(u).is_none_or(|a| a.is_finished()));
                    (done, t, p)
                };
                if lv.stage == LeaveStage::GetOut {
                    if lv.door_to_open {
                        self.process_open_door(lv.veh, lv.door, lv.anim_id, t);
                    }
                    self.line_up_exit(ped, &lv, &info, &m, p, false);
                    if done && lv.jacked_by.is_some() {
                        // 823 done → 206 CTaskComplexGetUpAndStandStill, then the
                        // DRAGGED_OUT_CAR event (stored until the drag task ends).
                        let jacker = lv.jacked_by;
                        let was_driver = self.body(lv.veh).and_then(|b| b.logic.as_any().downcast_ref::<Automobile>()).is_some_and(|c| c.driver == Some(ped));
                        let veh = lv.veh;
                        self.set_ped_out(ped, &lv);
                        let now = self.now_ms;
                        if let Some(p) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                            p.tasks.health.fall = Some(crate::peddamage::FallAndGetUp::Fall { anim: None, down_ms: 0, landed_at: Some(now) });
                            if let Some(n) = p.npc.as_mut() {
                                n.dragged_out = Some((jacker.unwrap(), veh, was_driver));
                                n.last_move_state = 0;
                            }
                        }
                        return;
                    }
                    if done {
                        // 806 CloseDoorFromOutside unless the stick is pushed (the door stays open).
                        let stick = self.body(ped).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).is_some_and(|p| {
                            let pad = &p.tasks.pad;
                            pad.walk_lr != 0.0 || pad.walk_ud != 0.0
                        });
                        let open = self.car_door_state(lv.veh, lv.door).is_some_and(|(_, missing, closed)| !missing && !closed);
                        if open && !stick && info.g.special_flags & 1 == 0 {
                            let id = match lv.door {
                                10 => 380,
                                8 => 381,
                                11 => 382,
                                _ => 383,
                            };
                            let grp = anim_group_of(&info.g, id);
                            let uid = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()).and_then(|p| {
                                let c = p.clump.as_deref_mut()?;
                                c.add_animation(&m, grp, id).map(|i| {
                                    c.assocs[i].finish_cb = true;
                                    c.assocs[i].uid
                                })
                            });
                            lv.anim = uid;
                            lv.anim_id = id;
                            lv.stage = LeaveStage::Close;
                        } else {
                            self.set_ped_out(ped, &lv);
                            return;
                        }
                    }
                } else {
                    self.process_open_door(lv.veh, lv.door, lv.anim_id, t);
                    self.line_up_exit(ped, &lv, &info, &m, p, true);
                    if done {
                        self.set_ped_out(ped, &lv);
                        return;
                    }
                }
            }
        }
        if let Some(p) = self.body_mut(ped).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
            p.leave = Some(lv);
        }
    }

    /// The exit line-up (type 0 during 813, type 2 frozen after it).
    fn line_up_exit(&mut self, ped: EntityId, lv: &LeaveCar, info: &CarDoorInfo, m: &AnimManager, p: f32, frozen: bool) {
        let aimed = heading_of(&info.m);
        let (xy, zb) = if frozen { (1.0, 0.0) } else { (p, 1.0 - z_ramp(info.g.z_times[2], p)) };
        let mut t1 = info.m.pos + info.m.rotate(info.local_target(m, lv.door, xy));
        let mut t2 = info.m.pos + info.m.rotate(info.local_target(m, lv.door, 1.0));
        let lat = (t2 - info.m.pos).dot(info.m.right);
        let base = info.m.pos.z - info.com_to_base + info.m.right.z * lat + 1.0;
        let o = LosOpts { bodies: false, peds: false, ..Default::default() };
        if let Some((_, _, cp)) = self.process_line_of_sight(t2 + Vec3::new(0.0, 0.0, 1.0), t2 - Vec3::new(0.0, 0.0, 4.0), &o) {
            t2.z = cp.point.z + 1.0;
        }
        if base - 0.5 > t2.z {
            t2.z = base;
        }
        let z_ref = t2.z;
        let Some(b) = self.body_mut(ped) else { return };
        let Some(pl) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return };
        if frozen {
            // Type 2: the ped stays where the get-out anim left it.
            t1 = b.phys.matrix.pos;
            t1.z = z_ref;
            b.phys.matrix.pos = t1;
            crate::ped::set_heading(&mut b.phys.matrix, pl.cur_rot);
            return;
        }
        t1.z = z_ref + zb * (t1.z - z_ref);
        b.phys.move_speed = Vec3::ZERO;
        pl.cur_rot = aimed;
        pl.aim_rot = aimed;
        // Get out: slerp from the car's orientation to upright.
        let qv = Quat::from_mat3(&glam::Mat3::from_cols(info.m.right, info.m.fwd, info.m.up));
        let qh = Quat::from_rotation_z(aimed);
        let q = if p <= 0.0 { qv } else if p >= 1.0 { qh } else { qv.slerp(qh, p) };
        let r = glam::Mat3::from_quat(q.normalize());
        b.phys.matrix = Matrix { right: r.x_axis, fwd: r.y_axis, up: r.z_axis, pos: t1 };
    }

    /// 816 CTaskSimpleCarSetPedOut (0x647D10): out of the vehicle, collision on, the car
    /// abandoned, back on foot.
    fn set_ped_out(&mut self, ped: EntityId, lv: &LeaveCar) {
        if lv.jacked_by.is_none() {
            self.release_door(lv.veh, lv.door);
        }
        let was_driver = self.body(lv.veh).and_then(|b| b.logic.as_any().downcast_ref::<Automobile>()).is_some_and(|c| c.driver == Some(ped));
        self.remove_from_seat(ped, lv.veh);
        if let Some(b) = self.body_mut(lv.veh) {
            if b.phys.status != Status::Wrecked && was_driver {
                b.phys.status = Status::Abandoned;
            }
        }
        let Some(b) = self.body_mut(ped) else { return };
        b.phys.eflags = (b.phys.eflags & !ef::IS_STATIC) | ef::USES_COLLISION;
        b.phys.move_speed = Vec3::ZERO;
        let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return };
        p.vehicle = None;
        p.leave = None;
        p.standing = false;
        crate::ped::set_heading(&mut b.phys.matrix, p.cur_rot);
        if let (Some(c), Some(m)) = (p.clump.as_deref_mut(), p.tasks.anims.clone()) {
            for a in &mut c.assocs {
                a.flags |= af::DELETE_BLENDED_OUT;
                a.blend_delta = -4.0;
            }
            c.blend_animation(&m, p.tasks.anim_group, crate::anim::anim_id::IDLE, 4.0);
        }
    }
}

/// `CAutomobile::OpenDoor` (0x6A6AE0): CDoor::Open plus the open / closed damage state.
pub fn open_door(c: &mut Automobile, e: usize, ratio: f32) {
    let d = &mut c.damage.doors[e];
    let was_closed = d.angle == d.closed_angle;
    d.open(ratio);
    let closed = d.angle == d.closed_angle;
    let st = &mut c.damage.dm.doors[e];
    if was_closed && !closed && (*st == 0 || *st == 2) {
        *st += 1;
    } else if !was_closed && ratio == 0.0 && (*st == 1 || *st == 3) {
        *st -= 1;
    }
}
