//! `data/object.dat`: physical properties of dynamic / breakable map objects.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct ObjectPhysics {
    pub mass: f32,
    pub turn_mass: f32,
    pub air_resistance: f32,
    pub elasticity: f32,
    /// Column F: percent submerged (buoyancy = (100 / this)·mass·0.008).
    pub percent_submerged: f32,
    /// Collision impulse (game units: kg * units/frame) needed to knock the object loose.
    pub uproot: f32,
    pub damage_mult: f32,
    /// 0 none, 1 change model, 20 smash, 21 change then smash, 200/202 breakable.
    pub damage_effect: u32,
    /// 0 none, 1 lamppost, 2 small box, 3 big box, 4 fence part, ...
    pub special: u32,
    pub camera_avoid: u8,
    pub causes_explosion: bool,
    /// 0 none, 1 on hit (dmg > 30), 2 on destroy, 3 always.
    pub fx_type: u8,
    /// x <= -500: at the hit position.
    pub fx_offset: [f32; 3],
    pub fx_name: Option<&'static str>,
    pub smash_multiplier: f32,
    pub break_velocity: [f32; 3],
    pub break_velocity_rand: f32,
    /// 1: bullets do 151, 2: bullets do smashMult·151.
    pub gun_break_mode: i32,
    pub sparks_on_impact: bool,
}

impl ObjectPhysics {
    /// Objects that stay put when hit: effectively immovable entries, plus
    /// swing/lock doors (special 6/7), which hinge rather than fly off.
    pub fn is_static(&self) -> bool {
        self.mass >= 50000.0 || self.uproot >= 9999.0 || matches!(self.special, 6 | 7)
    }
}

/// Keys are lowercased model names.
pub fn parse(text: &str) -> HashMap<String, ObjectPhysics> {
    let mut out = HashMap::new();
    for raw in text.lines() {
        let line = raw.split(';').next().unwrap_or("");
        let f: Vec<&str> = line.split([',', ' ', '\t']).filter(|s| !s.is_empty()).collect();
        if f.len() < 11 {
            continue;
        }
        let n = |i: usize| f.get(i).map(|v| v.parse::<f32>()).unwrap_or(Ok(0.0));
        let (Ok(mass), Ok(turn_mass), Ok(air), Ok(elasticity), Ok(uproot), Ok(cd_mult), Ok(cd_eff), Ok(special)) =
            (n(1), n(2), n(3), n(4), n(6), n(7), n(8), n(9))
        else {
            continue;
        };
        out.insert(
            f[0].to_ascii_lowercase(),
            ObjectPhysics {
                mass,
                turn_mass,
                air_resistance: air,
                elasticity,
                percent_submerged: n(5).unwrap_or(0.0),
                uproot,
                damage_mult: cd_mult,
                damage_effect: cd_eff as u32,
                special: special as u32,
                camera_avoid: n(10).unwrap_or(0.0) as u8,
                causes_explosion: n(11).unwrap_or(0.0) != 0.0,
                fx_type: n(12).unwrap_or(0.0) as u8,
                fx_offset: [n(13).unwrap_or(0.0), n(14).unwrap_or(0.0), n(15).unwrap_or(0.0)],
                fx_name: f
                    .get(16)
                    .filter(|s| n(12).unwrap_or(0.0) > 0.0 && !s.eq_ignore_ascii_case("none"))
                    .map(|s| &*Box::leak(s.to_ascii_lowercase().into_boxed_str())),
                smash_multiplier: n(17).unwrap_or(1.0),
                break_velocity: [n(18).unwrap_or(0.0), n(19).unwrap_or(0.0), n(20).unwrap_or(0.0)],
                break_velocity_rand: n(21).unwrap_or(0.0),
                gun_break_mode: n(22).unwrap_or(0.0) as i32,
                sparks_on_impact: n(23).unwrap_or(0.0) != 0.0,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_lines() {
        let m = super::parse(
            ";comment\n\
             lamppost2\t1250.0,\t2000.0\t0.99,\t0.2,\t50.0,\t240.0,\t1.0,\t200,\t1,\t0,\t0,\t0,\t0.0, 0.0, 0.0,\tnone\n",
        );
        let l = m["lamppost2"];
        assert_eq!(l.mass, 1250.0);
        assert_eq!(l.uproot, 240.0);
        assert_eq!(l.percent_submerged, 50.0);
        assert_eq!(l.damage_effect, 200);
        assert_eq!(l.special, 1);
    }
}
