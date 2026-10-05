//! Vehicle data: `vehicles.ide` (cars section), `handling.cfg`, `carcols.dat`.

use std::collections::HashMap;

use crate::ide::fields;

#[derive(Debug, Clone)]
pub struct VehicleDef {
    pub id: u32,
    pub model: String,
    pub txd: String,
    /// car, bike, bmx, quad, heli, plane, boat, train, trailer, mtruck
    pub kind: String,
    pub handling: String,
    pub game_name: String,
    /// The anims column (bike ride group name: "bikes", "bmx", ...; "null" for most cars).
    pub anims: String,
    pub wheel_model: i32,
    pub wheel_scale_front: f32,
    pub wheel_scale_rear: f32,
}

pub fn parse_vehicles_ide(text: &str) -> Vec<VehicleDef> {
    let mut out = Vec::new();
    let mut in_cars = false;
    for raw in text.lines() {
        let f = fields(raw);
        if f.len() == 1 {
            in_cars = f[0].eq_ignore_ascii_case("cars");
            continue;
        }
        if !in_cars || f.len() < 11 {
            continue;
        }
        let num = |i: usize| f.get(i).and_then(|s| s.parse::<f32>().ok());
        let Ok(id) = f[0].parse() else { continue };
        out.push(VehicleDef {
            id,
            model: f[1].to_ascii_lowercase(),
            txd: f[2].to_ascii_lowercase(),
            kind: f[3].to_ascii_lowercase(),
            handling: f[4].to_ascii_uppercase(),
            game_name: f[5].to_string(),
            anims: f.get(6).map(|s| s.to_ascii_lowercase()).unwrap_or_default(),
            wheel_model: f.get(11).and_then(|s| s.parse().ok()).unwrap_or(-1),
            wheel_scale_front: num(12).unwrap_or(0.7),
            wheel_scale_rear: num(13).or(num(12)).unwrap_or(0.7),
        });
    }
    out
}

/// One standard (land vehicle) line of handling.cfg. Units are as in the file.
#[derive(Debug, Clone)]
pub struct Handling {
    pub id: String,
    pub mass: f32,
    pub turn_mass: f32,
    pub drag_mult: f32,
    pub centre_of_mass: [f32; 3],
    pub percent_submerged: f32,
    pub traction_mult: f32,
    pub traction_loss: f32,
    pub traction_bias: f32,
    pub gears: u32,
    /// km/h
    pub max_velocity: f32,
    pub engine_accel: f32,
    pub engine_inertia: f32,
    /// 'F', 'R' or '4'
    pub drive_type: char,
    pub engine_type: char,
    pub brake_decel: f32,
    pub brake_bias: f32,
    pub abs: bool,
    /// degrees
    pub steering_lock: f32,
    pub susp_force: f32,
    pub susp_damping: f32,
    pub susp_high_speed_damping: f32,
    pub susp_upper: f32,
    pub susp_lower: f32,
    pub susp_bias: f32,
    pub anti_dive: f32,
    pub seat_offset: f32,
    pub collision_damage: f32,
    pub value: u32,
    pub model_flags: u32,
    pub handling_flags: u32,
    /// Vehicle anim group (`CVehicleAnimGroup` index, handling +0xDE).
    pub anim_group: u8,
}

pub fn parse_handling(text: &str) -> HashMap<String, Handling> {
    let mut out = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        // ';' comments; '!' bikes, '$' flying, '%' boats, '^' anim groups use other layouts.
        if line.is_empty() || line.starts_with([';', '!', '$', '%', '^', '#']) {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 33 {
            continue;
        }
        let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        let h = |i: usize| u32::from_str_radix(f[i], 16).unwrap_or(0);
        let c = |i: usize| f[i].chars().next().unwrap_or('R').to_ascii_uppercase();
        let hd = Handling {
            id: f[0].to_ascii_uppercase(),
            mass: n(1),
            turn_mass: n(2),
            drag_mult: n(3),
            centre_of_mass: [n(4), n(5), n(6)],
            percent_submerged: n(7),
            traction_mult: n(8),
            traction_loss: n(9),
            traction_bias: n(10),
            gears: n(11) as u32,
            max_velocity: n(12),
            engine_accel: n(13),
            engine_inertia: n(14),
            drive_type: c(15),
            engine_type: c(16),
            brake_decel: n(17),
            brake_bias: n(18),
            abs: n(19) != 0.0,
            steering_lock: n(20),
            susp_force: n(21),
            susp_damping: n(22),
            susp_high_speed_damping: n(23),
            susp_upper: n(24),
            susp_lower: n(25),
            susp_bias: n(26),
            anti_dive: n(27),
            seat_offset: n(28),
            collision_damage: n(29),
            value: n(30) as u32,
            model_flags: h(31),
            handling_flags: h(32),
            anim_group: f.get(35).and_then(|v| v.parse().ok()).unwrap_or(0),
        };
        out.insert(hd.id.clone(), hd);
    }
    out
}

/// One `^` row of handling.cfg: `CVehicleAnimGroup` (enter_exit.md §1.3).
#[derive(Debug, Clone, Default)]
pub struct VehicleAnimGroup {
    pub id: u8,
    /// Ped anim groups (column + 88).
    pub first_group: u8,
    pub second_group: u8,
    /// Bit k: column D+k uses the second group.
    pub second_mask: u32,
    /// 1 / 2 don't close the door after getting out / in, 4 kart, 8 truck, 16 hover,
    /// 32 special locked door, 64 don't open the door when getting in.
    pub special_flags: u32,
    /// GetIn, JumpOut, GetOut, JackedOut, Fall z-blend times.
    pub z_times: [f32; 5],
    /// Door windows (start, stop) in seconds: OpenOut, CloseIn, OpenIn, CloseOut.
    pub door_start: [f32; 4],
    pub door_stop: [f32; 4],
}

/// The `^` rows of handling.cfg.
pub fn parse_vehicle_anim_groups(text: &str) -> Vec<VehicleAnimGroup> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        let Some(rest) = line.strip_prefix('^') else { continue };
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.len() < 35 {
            continue;
        }
        let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        let mut g = VehicleAnimGroup {
            id: n(0) as u8,
            first_group: n(1) as u8 + 88,
            second_group: n(2) as u8 + 88,
            special_flags: f[34].parse::<i64>().unwrap_or(0) as u32,
            ..Default::default()
        };
        for k in 0..18 {
            if f[3 + k].parse::<i64>().unwrap_or(0) != 0 {
                g.second_mask |= 1 << k;
            }
        }
        for k in 0..5 {
            g.z_times[k] = n(21 + k);
        }
        for k in 0..4 {
            g.door_start[k] = n(26 + 2 * k);
            g.door_stop[k] = n(27 + 2 * k);
        }
        out.push(g);
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct CarColors {
    pub palette: Vec<[u8; 3]>,
    /// Model name -> list of (primary, secondary) palette indices.
    pub cars: HashMap<String, Vec<(usize, usize)>>,
}

pub fn parse_carcols(text: &str) -> CarColors {
    let mut cc = CarColors::default();
    let mut section = String::new();
    for raw in text.lines() {
        let f = fields(raw);
        if f.is_empty() {
            continue;
        }
        if f.len() == 1 && f[0].parse::<u32>().is_err() {
            section = f[0].to_ascii_lowercase();
            continue;
        }
        match section.as_str() {
            "col" if f.len() >= 3 => {
                let c = |i: usize| f[i].parse::<u8>().unwrap_or(0);
                cc.palette.push([c(0), c(1), c(2)]);
            }
            "car" if f.len() >= 3 => {
                let idx: Vec<usize> = f[1..].iter().filter_map(|s| s.parse().ok()).collect();
                let pairs = idx.chunks_exact(2).map(|p| (p[0], p[1])).collect();
                cc.cars.insert(f[0].to_ascii_lowercase(), pairs);
            }
            _ => {}
        }
    }
    cc
}

/// A `%` (boat) line of handling.cfg: `tBoatHandlingData` (no unit conversion).
#[derive(Debug, Clone, PartialEq)]
pub struct BoatHandling {
    pub id: String,
    pub thrust_y: f32,
    pub thrust_z: f32,
    pub thrust_app_z: f32,
    pub aq_plane_force: f32,
    pub aq_plane_limit: f32,
    pub aq_plane_offset: f32,
    pub wave_audio_mult: f32,
    pub move_res: [f32; 3],
    pub turn_res: [f32; 3],
    pub look_lr_behind_cam_height: f32,
}

pub fn parse_boat_handling(text: &str) -> HashMap<String, BoatHandling> {
    let mut out = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if !line.starts_with('%') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 16 {
            continue;
        }
        let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        let b = BoatHandling {
            id: f[1].to_ascii_uppercase(),
            thrust_y: n(2),
            thrust_z: n(3),
            thrust_app_z: n(4),
            aq_plane_force: n(5),
            aq_plane_limit: n(6),
            aq_plane_offset: n(7),
            wave_audio_mult: n(8),
            move_res: [n(9), n(10), n(11)],
            turn_res: [n(12), n(13), n(14)],
            look_lr_behind_cam_height: n(15),
        };
        out.insert(b.id.clone(), b);
    }
    out
}

#[cfg(test)]
mod boat_tests {
    #[test]
    fn parses_boat_line() {
        let m = super::parse_boat_handling("%\tPREDATOR\t0.79\t0.5\t\t0.6\t\t7.0\t\t0.60\t-1.9\t4.0\t\t\t0.8\t\t0.998\t0.998\t\t0.85\t0.98\t0.97\t4.0\n");
        let b = &m["PREDATOR"];
        assert_eq!(b.thrust_y, 0.79);
        assert_eq!(b.aq_plane_offset, -1.9);
        assert_eq!(b.move_res, [0.8, 0.998, 0.998]);
        assert_eq!(b.turn_res, [0.85, 0.98, 0.97]);
        assert_eq!(b.look_lr_behind_cam_height, 4.0);
    }
}

/// A `!` (bike) line of handling.cfg: `tBikeHandlingData` before ConvertBikeDataToGameUnits.
#[derive(Debug, Clone, PartialEq)]
pub struct BikeHandling {
    pub id: String,
    pub lean_fwd_com: f32,
    pub lean_fwd_force: f32,
    pub lean_bak_com: f32,
    pub lean_bak_force: f32,
    /// Degrees.
    pub max_lean: f32,
    pub full_anim_lean: f32,
    pub des_lean: f32,
    pub speed_steer: f32,
    pub slip_steer: f32,
    pub no_player_com_z: f32,
    pub wheelie_ang: f32,
    pub stoppie_ang: f32,
    pub wheelie_steer: f32,
    pub wheelie_stab_mult: f32,
    pub stoppie_stab_mult: f32,
}

pub fn parse_bike_handling(text: &str) -> HashMap<String, BikeHandling> {
    let mut out = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if !line.starts_with('!') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 17 {
            continue;
        }
        let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        let b = BikeHandling {
            id: f[1].to_ascii_uppercase(),
            lean_fwd_com: n(2),
            lean_fwd_force: n(3),
            lean_bak_com: n(4),
            lean_bak_force: n(5),
            max_lean: n(6),
            full_anim_lean: n(7),
            des_lean: n(8),
            speed_steer: n(9),
            slip_steer: n(10),
            no_player_com_z: n(11),
            wheelie_ang: n(12),
            stoppie_ang: n(13),
            wheelie_steer: n(14),
            wheelie_stab_mult: n(15),
            stoppie_stab_mult: n(16),
        };
        out.insert(b.id.clone(), b);
    }
    out
}
