//! A ped seated in a vehicle: `CCarEnterExit::AddInCarAnim` (0x64F720),
//! `CPed::SetPedPositionInCar` (0x5DF910), `CVehicle::ProcessDrivingAnims` (0x6DF4A0, cars
//! and boats) and `CBike::ProcessRiderAnims` (0x6B7280).
//!
//! Stage A of the enter/exit port: the ped is warped into the driver seat
//! (`SetPedInCarDirect`) and out again; the enter/exit task graphs are not ported yet.
//! Not ported: drive-by and radio-tune anims, bopping, BMX hop seat drop, the van rear
//! benches, the rider's wheelie/stoppie lean targets and wind shake.

use glam::{Mat3, Vec3};

use crate::{
    anim::{AnimManager, Clump, af},
    automobile::Automobile,
    bike::Bike,
    boat::Boat,
    ped::PedLogic,
    physical::{Matrix, Status, ef},
    world::{EntityId, World},
};

/// handling.cfg model flag 0x4: low car (`veh+0x429 & 8`).
pub const MF_IS_LOW: u32 = 0x4;
/// handling.cfg model flag 0x400: boat crew sit (CAR_sit instead of DRIVE_BOAT).
pub const MF_SIT_IN_BOAT: u32 = 0x400;
/// `CVehicleAnimGroup` special flags: kart / truck drive anims.
pub const AG_KART: u32 = 4;
pub const AG_TRUCK: u32 = 8;

/// Group-0 anim ids of the seated poses.
pub mod ids {
    pub const CAR_SIT: i16 = 60;
    pub const CAR_LSIT: i16 = 61;
    pub const DRIVE_BOAT: i16 = 81;
    /// Ride group: BIKE_Ride, Still, Left, Right, Back, Fwd, pushes.
    pub const BIKE_RIDE: i16 = 194;
    pub const BIKE_STILL: i16 = 195;
    pub const BIKE_LEFT: i16 = 196;
    pub const BIKE_RIGHT: i16 = 197;
    pub const BIKE_BACK: i16 = 198;
    pub const BIKE_FWD: i16 = 199;
    pub const BIKE_PUSHES: i16 = 200;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatKind {
    Car,
    Boat,
    Bike,
}

/// ped+0x58C (the vehicle) and the seat state of `CTaskSimpleCarDrive`.
#[derive(Debug, Clone)]
pub struct InVehicle {
    pub veh: EntityId,
    pub kind: SeatKind,
    /// Seat offset in vehicle model space (the driver's x is already mirrored).
    pub seat: Vec3,
    /// `GetRideAnimData()` ride group (bikes).
    pub ride_group: Option<usize>,
    /// Rider anim L/R and fwd/back blends (bike +0x654 / +0x658).
    pub rider_lr: f32,
    pub rider_fb: f32,
    /// CStats 160 driving skill (0 at a new game: the "weak" set).
    pub driving_skill: f32,
}

/// What the seat and the anims read from the vehicle this frame.
#[derive(Debug, Clone, Copy)]
pub struct VehState {
    pub matrix: Matrix,
    pub move_speed: Vec3,
    /// +0x494 steer angle (+ = left).
    pub steer: f32,
    pub gas: f32,
    pub brake: f32,
    pub model_flags: u32,
    /// `CVehicleAnimGroup::specialFlags` of handling +0xDE.
    pub anim_flags: u32,
    /// Bikes: render lean +0x648, lean stick +0x650, FullAnimLean, colModel min z,
    /// handling +0x88 / +0x8C (forward / reverse caps).
    pub lean: f32,
    pub lean_stick: f32,
    pub full_anim_lean: f32,
    pub col_min_z: f32,
    pub max_forward: f32,
    pub max_reverse: f32,
    /// A second ped sits on the bike (no fwd/back rider anims).
    pub has_passenger: bool,
}

/// `CBike::CalculateLeanMatrix` (0x6B7150): `matrix · (RotY(lean)·RotX(|lean|·−0.05))`, lowered
/// along its up axis by `(1 − cos lean)·colMin.z`.
pub fn bike_lean_matrix(m: &Matrix, lean: f32, col_min_z: f32) -> Matrix {
    let l = Mat3::from_rotation_y(lean) * Mat3::from_rotation_x(lean.abs() * -0.05);
    let r = Mat3::from_cols(m.right, m.fwd, m.up) * l;
    let mut out = Matrix { right: r.x_axis, fwd: r.y_axis, up: r.z_axis, pos: m.pos };
    out.pos += out.up * ((1.0 - lean.cos()) * col_min_z);
    out
}

/// `CPed::SetPedPositionInCar` for the driver: the ped matrix is the vehicle's (or the bike's
/// lean matrix) with the seat offset; the heading is the vehicle heading.
pub fn ped_position_in_car(iv: &InVehicle, v: &VehState) -> (Matrix, f32) {
    let mut m = if iv.kind == SeatKind::Bike { bike_lean_matrix(&v.matrix, v.lean, v.col_min_z) } else { v.matrix };
    m.pos += m.rotate(iv.seat);
    let h = (-v.matrix.fwd.x).atan2(v.matrix.fwd.y);
    (m, h)
}

/// 0x64F6E0: partial anims fade out at once.
fn kill_partial_anims(clump: &mut Clump) {
    for a in &mut clump.assocs {
        if a.has(af::PARTIAL) {
            a.flags |= af::DELETE_BLENDED_OUT;
            a.blend_delta = -1000.0;
        }
    }
}

/// `CPed::StopNonPartialAnims` (0x5DED10).
fn stop_non_partial_anims(clump: &mut Clump) {
    for a in &mut clump.assocs {
        if !a.has(af::PARTIAL) {
            a.set_playing(false);
        }
    }
}

/// `CCarEnterExit::AddInCarAnim(veh, ped, bAsDriver=true)` after 0x64F6E0.
pub fn add_in_car_anim(clump: &mut Clump, man: &AnimManager, iv: &InVehicle, model_flags: u32) {
    kill_partial_anims(clump);
    let (group, id) = match (iv.ride_group, iv.kind) {
        (Some(g), _) => (g, ids::BIKE_RIDE),
        (None, SeatKind::Boat) if model_flags & MF_SIT_IN_BOAT == 0 => (0, ids::DRIVE_BOAT),
        (None, SeatKind::Car) if model_flags & MF_IS_LOW != 0 => (0, ids::CAR_LSIT),
        _ => (0, ids::CAR_SIT),
    };
    clump.blend_animation(man, group, id, 1000.0);
    stop_non_partial_anims(clump);
}

/// `CVehicle::ProcessDrivingAnims(ped, bTuneRadio=false)` for the player (NPC car drivers keep
/// the static sit).
pub fn process_driving_anims(clump: &mut Clump, man: &AnimManager, iv: &InVehicle, v: &VehState) {
    let low = iv.kind == SeatKind::Car && v.model_flags & MF_IS_LOW != 0;
    let speed = v.move_speed.length();
    let set: [i16; 4] = if low {
        [61, 68, 69, 78]
    } else if iv.kind == SeatKind::Boat && v.model_flags & MF_SIT_IN_BOAT == 0 {
        [81, 82, 83, 84]
    } else if v.anim_flags & AG_KART != 0 {
        [95, 96, 97, 98]
    } else if v.anim_flags & AG_TRUCK != 0 {
        [91, 92, 93, 94]
    } else if iv.driving_skill >= 100.0 {
        if speed > 0.4 { [63, 72, 73, 80] } else { [63, 89, 90, 80] }
    } else if iv.driving_skill < 50.0 {
        if speed > 0.4 { [62, 70, 71, 79] } else { [62, 87, 88, 79] }
    } else if speed > 0.4 {
        [60, 66, 67, 78]
    } else {
        [60, 85, 86, 78]
    };
    let Some(sit) = clump.get(set[0]) else {
        if clump.get(ids::CAR_SIT).is_some() {
            clump.blend_animation(man, 0, set[0], 4.0);
        }
        return;
    };
    if sit.blend < 1.0 {
        return;
    }
    let drive_by = (74..=77).any(|id| clump.get(id).is_some());
    if !low && v.gas < 0.0 && !drive_by {
        // Reversing: look back.
        if let Some(lb) = clump.get(set[3]) {
            if lb.blend >= 1.0 || lb.blend_delta > 0.0 {
                return;
            }
        }
        clump.blend_animation(man, 0, set[3], 4.0);
        return;
    }
    let st = v.steer;
    if st == 0.0 || drive_by {
        for id in [set[1], set[2], set[3]] {
            if let Some(a) = clump.get_mut(id) {
                a.blend_delta = -4.0;
            }
        }
        return;
    }
    let b = (st.abs() * 1.639_344_2).clamp(0.0, 1.0);
    let (on, off) = if st > 0.0 { (set[1], set[2]) } else { (set[2], set[1]) };
    if let Some(a) = clump.get_mut(off) {
        a.blend = 0.0;
        a.blend_delta = 0.0;
    }
    match clump.get_mut(on) {
        Some(a) => {
            a.blend = b;
            a.blend_delta = 0.0;
        }
        None => {
            clump.blend_animation(man, 0, on, 4.0);
        }
    }
    if let Some(a) = clump.get_mut(set[3]) {
        a.blend_delta = -4.0;
    }
}

/// Get the assoc, or `CAnimManager::AddAnimation` it.
fn get_or_add(clump: &mut Clump, man: &AnimManager, group: usize, id: i16) -> Option<usize> {
    clump.index_of(id).or_else(|| clump.add_animation(man, group, id))
}

/// Pose a partial rider anim by its time: `blend = w`, `SetCurrentTime(dur·t)`, not playing.
fn pose(clump: &mut Clump, man: &AnimManager, group: usize, on: i16, off: i16, w: f32, t: f32) {
    let (Some(_), Some(_)) = (get_or_add(clump, man, group, on), get_or_add(clump, man, group, off)) else { return };
    if let Some(a) = clump.get_mut(on) {
        a.blend = w;
        let len = a.hier.total_length;
        a.set_current_time(len * t);
        a.set_playing(false);
    }
    if let Some(a) = clump.get_mut(off) {
        a.blend = 0.0;
    }
}

/// `CBike::ProcessRiderAnims(ped, veh, rideData, bikeHandling)` (0x6B7280), main path.
pub fn process_rider_anims(clump: &mut Clump, man: &AnimManager, iv: &mut InVehicle, v: &VehState, is_player: bool, ts: f32) {
    let Some(group) = iv.ride_group else { return };
    let fwd_speed = v.move_speed.dot(v.matrix.fwd);
    let drive_by = [202, 203, 204].into_iter().any(|id| clump.get(id).is_some());
    let mut fb_cap = 0.0;
    if drive_by || fwd_speed.abs() >= 0.02 {
        let reversing = fwd_speed < 0.0 && v.gas < 0.0 && fwd_speed > v.max_reverse * 1.5;
        if reversing {
            let push = clump.get(ids::BIKE_PUSHES);
            if push.is_none_or(|a| a.blend < 1.0 && a.blend_delta <= 0.0) {
                clump.blend_animation(man, group, ids::BIKE_PUSHES, 4.0);
            }
        } else {
            if is_player && fwd_speed < 0.0 && fwd_speed < v.max_reverse * 1.5 {
                fb_cap = -1.0;
            }
            for id in [ids::BIKE_STILL, ids::BIKE_PUSHES] {
                if let Some(a) = clump.get_mut(id) {
                    if a.blend_delta >= 0.0 {
                        a.blend_delta = -4.0;
                    }
                }
            }
        }
    } else if clump.get(ids::BIKE_STILL).is_none_or(|a| a.blend < 1.0 && a.blend_delta <= 0.0) {
        clump.blend_animation(man, group, ids::BIKE_STILL, 2.0);
    }

    // The share left for the lean poses (ts_nc = ts here).
    let share = |a: Option<&crate::anim::Assoc>| a.map_or(0.0, |a| ts * 0.02 * a.blend_delta + a.blend);
    let mut w = 1.0f32;
    if clump.get(ids::BIKE_STILL).is_some() {
        w = 1.0 - share(clump.get(ids::BIKE_STILL)).min(1.0);
    }
    for id in [202, 203, 204] {
        if clump.get(id).is_some() {
            w -= share(clump.get(id)).min(w);
            break;
        }
    }
    if clump.get(ids::BIKE_PUSHES).is_some() {
        w -= share(clump.get(ids::BIKE_PUSHES)).min(w);
    }

    let lr_t = if fb_cap == -1.0 {
        0.0
    } else {
        (v.lean / v.full_anim_lean).clamp(-1.0, 1.0)
    };
    let p = 0.86f32.powf(ts);
    iv.rider_lr = p * iv.rider_lr + (1.0 - p) * lr_t;

    let mut fb_t = 0.0;
    if is_player && !v.has_passenger && fb_cap > -1.0 {
        fb_t = v.lean_stick;
        if v.brake > 0.5 && fwd_speed > 0.01 {
            fb_t = fb_t.max(0.1);
        } else if v.gas > 0.5 && fb_t <= 0.0 && fwd_speed < v.max_forward * 0.3 && fb_t >= -0.3 {
            fb_t = -0.3;
        }
        if lr_t.abs() > 0.3 {
            fb_t *= (1.0 - (lr_t.abs() - 0.3) / (0.56 - 0.3)).max(0.0);
        }
    } else if is_player && fb_cap == -1.0 {
        fb_t = -1.0;
    }
    iv.rider_fb = if is_player {
        let p = 0.89f32.powf(ts);
        p * iv.rider_fb + (1.0 - p) * fb_t
    } else {
        0.0
    };

    let (lr, fb) = (iv.rider_lr, iv.rider_fb);
    let (mut w_lr, mut w_fb) = (1.0, 0.0);
    if lr.abs() <= 0.56 && is_player {
        (w_lr, w_fb) = (0.0, 1.0);
        if fb.abs() <= 0.56 {
            let m = (lr * lr + fb * fb).sqrt();
            (w_lr, w_fb) = if m <= 0.01 { (lr.abs(), fb.abs()) } else { ((lr / m).abs(), (fb / m).abs()) };
        }
    }
    if is_player {
        if fb < 0.0 {
            pose(clump, man, group, ids::BIKE_BACK, ids::BIKE_FWD, w_fb * w, -fb);
        } else {
            pose(clump, man, group, ids::BIKE_FWD, ids::BIKE_BACK, w_fb * w, fb);
        }
    }
    if lr < 0.0 {
        pose(clump, man, group, ids::BIKE_LEFT, ids::BIKE_RIGHT, w_lr * w, -lr);
    } else {
        pose(clump, man, group, ids::BIKE_RIGHT, ids::BIKE_LEFT, w_lr * w, lr);
    }
}

impl World {
    /// The vehicle as the seated ped sees it, with its seat kind and ride group.
    pub fn veh_state(&self, veh: EntityId) -> Option<(SeatKind, VehState, Option<usize>)> {
        let b = self.body(veh)?;
        let mut v = VehState {
            matrix: b.phys.matrix,
            move_speed: b.phys.move_speed,
            steer: 0.0,
            gas: 0.0,
            brake: 0.0,
            model_flags: 0,
            anim_flags: 0,
            lean: 0.0,
            lean_stick: 0.0,
            full_anim_lean: 1.0,
            col_min_z: 0.0,
            max_forward: 1.0,
            max_reverse: -0.2,
            has_passenger: false,
        };
        let any = b.logic.as_any();
        let flags_of = |g: u8| self.veh_anim_flags.get(g as usize).copied().unwrap_or(0);
        if let Some(c) = any.downcast_ref::<Automobile>() {
            v.steer = c.steer_angle;
            v.gas = c.gas;
            v.brake = c.brake;
            v.model_flags = c.h.model_flags;
            v.anim_flags = flags_of(c.h.anim_group);
            Some((SeatKind::Car, v, None))
        } else if let Some(c) = any.downcast_ref::<Boat>() {
            v.steer = c.steer_angle;
            v.gas = c.gas;
            v.brake = c.brake;
            v.model_flags = c.h.model_flags;
            v.anim_flags = flags_of(c.h.anim_group);
            Some((SeatKind::Boat, v, None))
        } else if let Some(c) = any.downcast_ref::<Bike>() {
            v.steer = c.steer;
            v.gas = c.gas;
            v.brake = c.brake;
            v.model_flags = c.h.model_flags;
            v.anim_flags = flags_of(c.h.anim_group);
            v.lean = c.lean_render;
            v.lean_stick = c.lean_in;
            v.full_anim_lean = c.bh.full_anim_lean.max(1e-3);
            v.col_min_z = c.col_min_z;
            v.max_forward = c.h.trans.max_forward;
            v.max_reverse = c.h.trans.max_reverse;
            Some((SeatKind::Bike, v, Some(c.ride_group)))
        } else {
            None
        }
    }

    /// `CCarEnterExit::SetPedInCarDirect` + `CTaskSimpleCarSetPedInAsDriver` (the warp path):
    /// `front_seat` is the vehicle's front-seat dummy in model space.
    pub fn set_ped_in_car_direct(&mut self, ped: EntityId, veh: EntityId, front_seat: Vec3) -> bool {
        let Some((kind, v, ride_group)) = self.veh_state(veh) else { return false };
        let mut seat = front_seat;
        if kind == SeatKind::Car {
            // The dummy is on the +x side: the driver sits mirrored.
            seat.x = -seat.x;
        }
        let iv = InVehicle { veh, kind, seat, ride_group, rider_lr: 0.0, rider_fb: 0.0, driving_skill: 0.0 };
        let is_player = {
            let Some(b) = self.body_mut(ped) else { return false };
            // Collision off; the seat step moves the ped.
            b.phys.eflags = (b.phys.eflags | ef::IS_STATIC) & !ef::USES_COLLISION;
            b.phys.move_speed = Vec3::ZERO;
            b.phys.turn_speed = Vec3::ZERO;
            let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return false };
            p.standing = false;
            p.anim_velocity = glam::Vec2::ZERO;
            p.ground_entity = None;
            p.tasks.swim = None;
            p.tasks.fight = None;
            p.tasks.gun = None;
            if let (Some(clump), Some(man)) = (p.clump.as_deref_mut(), p.tasks.anims.clone()) {
                add_in_car_anim(clump, &man, &iv, v.model_flags);
            }
            p.vehicle = Some(iv);
            p.is_player
        };
        if let Some(b) = self.body_mut(veh) {
            if b.phys.status != Status::Wrecked {
                b.phys.status = if is_player { Status::Player } else { Status::Physics };
            }
        }
        // CCrime 6 (steal car) the first time the player drives it (`+0x42A & 2`).
        if is_player && self.stolen.insert(veh) {
            self.report_crime(6, Some(veh), Some(ped));
        }
        self.process_peds_in_vehicles(0.0);
        true
    }

    /// Stage A exit (`CTaskSimpleCarSetPedOut` without the get-out sequence): collision back on,
    /// the in-car anims replaced by the idle, at `pos` facing `heading`.
    pub fn set_ped_out_of_car(&mut self, ped: EntityId, pos: Vec3, heading: f32) {
        let Some(b) = self.body_mut(ped) else { return };
        b.phys.eflags = (b.phys.eflags & !ef::IS_STATIC) | ef::USES_COLLISION;
        b.phys.move_speed = Vec3::ZERO;
        b.phys.matrix.pos = pos;
        crate::ped::set_heading(&mut b.phys.matrix, heading);
        let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { return };
        p.vehicle = None;
        p.standing = false;
        p.cur_rot = heading;
        p.aim_rot = heading;
        if let (Some(clump), Some(man)) = (p.clump.as_deref_mut(), p.tasks.anims.clone()) {
            for a in &mut clump.assocs {
                a.flags |= af::DELETE_BLENDED_OUT;
                a.blend_delta = -1000.0;
            }
            clump.blend_animation(&man, p.tasks.anim_group, crate::anim::anim_id::IDLE, 1000.0);
        }
    }

    /// Seated peds: anims (UpdateAnim + CTaskSimpleCarDrive::ProcessPed) and
    /// `SetPedPositionInCar` after the vehicles moved and collided.
    pub(crate) fn process_peds_in_vehicles(&mut self, ts: f32) {
        let seated: Vec<(EntityId, EntityId)> = self
            .body_ids()
            .into_iter()
            .filter_map(|id| {
                let p = self.body(id)?.logic.as_any().downcast_ref::<PedLogic>()?;
                Some((id, p.vehicle.as_ref()?.veh))
            })
            .collect();
        for (ped, veh) in seated {
            let state = self.veh_state(veh);
            let Some(b) = self.body_mut(ped) else { continue };
            let Some((_, v, _)) = state else {
                // The vehicle is gone: drop out where the ped is.
                b.phys.eflags = (b.phys.eflags & !ef::IS_STATIC) | ef::USES_COLLISION;
                if let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() {
                    p.vehicle = None;
                }
                continue;
            };
            let phys = &mut b.phys;
            let Some(p) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { continue };
            let Some(iv) = p.vehicle.as_mut() else { continue };
            if ts > 0.0 {
                if let Some(clump) = p.clump.as_deref_mut() {
                    p.prev_pose.clone_from(&clump.pose);
                    if let Some(man) = p.tasks.anims.clone() {
                        match iv.kind {
                            SeatKind::Bike => process_rider_anims(clump, &man, iv, &v, p.is_player, ts),
                            _ if p.is_player => process_driving_anims(clump, &man, iv, &v),
                            _ => {}
                        }
                    }
                    clump.update(ts * 0.02);
                }
            }
            let (m, h) = ped_position_in_car(iv, &v);
            phys.matrix = m;
            phys.move_speed = v.move_speed;
            p.cur_rot = h;
            p.aim_rot = h;
            p.anim_velocity = glam::Vec2::ZERO;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lean_matrix_rolls_about_forward_axis_and_drops() {
        let m = Matrix { pos: Vec3::new(0.0, 0.0, 1.0), ..Matrix::IDENTITY };
        let l = bike_lean_matrix(&m, 0.5, -0.6);
        // RotY(+lean) tips the up axis towards +x (lean right).
        assert!(l.up.x > 0.4);
        assert!((l.fwd.y - 1.0).abs() < 0.01);
        assert!(l.pos.z < 1.0);
    }

    #[test]
    fn driver_seat_follows_the_vehicle() {
        let iv = InVehicle {
            veh: EntityId::Body(0),
            kind: SeatKind::Car,
            seat: Vec3::new(-0.5, 0.2, 0.1),
            ride_group: None,
            rider_lr: 0.0,
            rider_fb: 0.0,
            driving_skill: 0.0,
        };
        // Vehicle facing west (heading +90 degrees).
        let m = Matrix { right: Vec3::Y, fwd: -Vec3::X, up: Vec3::Z, pos: Vec3::new(10.0, 0.0, 0.0) };
        let v = VehState {
            matrix: m,
            move_speed: Vec3::ZERO,
            steer: 0.0,
            gas: 0.0,
            brake: 0.0,
            model_flags: 0,
            anim_flags: 0,
            lean: 0.0,
            lean_stick: 0.0,
            full_anim_lean: 1.0,
            col_min_z: 0.0,
            max_forward: 1.0,
            max_reverse: -0.2,
            has_passenger: false,
        };
        let (pm, h) = ped_position_in_car(&iv, &v);
        assert!((h - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!((pm.pos - Vec3::new(9.8, -0.5, 0.1)).length() < 1e-5, "{:?}", pm.pos);
    }
}
