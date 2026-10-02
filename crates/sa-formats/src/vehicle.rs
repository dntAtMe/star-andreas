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
        };
        out.insert(hd.id.clone(), hd);
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
