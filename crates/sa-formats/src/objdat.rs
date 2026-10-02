//! `data/object.dat`: physical properties of dynamic / breakable map objects.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct ObjectPhysics {
    pub mass: f32,
    pub turn_mass: f32,
    pub air_resistance: f32,
    pub elasticity: f32,
    /// Collision impulse (game units: kg * units/frame) needed to knock the object loose.
    pub uproot: f32,
    pub damage_mult: f32,
    /// 0 none, 1 change model, 20 smash, 21 change then smash, 200/202 breakable.
    pub damage_effect: u32,
    /// 0 none, 1 lamppost, 2 small box, 3 big box, 4 fence part, ...
    pub special: u32,
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
        let n = |i: usize| f[i].parse::<f32>();
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
                uproot,
                damage_mult: cd_mult,
                damage_effect: cd_eff as u32,
                special: special as u32,
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
        assert_eq!(l.damage_effect, 200);
        assert_eq!(l.special, 1);
    }
}
