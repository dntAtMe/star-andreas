//! `CSurfaceInfos`: per-surface adhesion group / grip (surfinfo.dat) and the
//! group-vs-group adhesive limit table (surface.dat).

use crate::colpoint::ColPoint;

pub const NUM_GROUPS: usize = 6;
const GROUP_NAMES: [&str; NUM_GROUPS] = ["RUBBER", "HARD", "ROAD", "LOOSE", "SAND", "WET"];

/// Surface id of WHEELBASE (disk-wheel body contacts).
pub const SURFACE_WHEELBASE: u8 = 60;

#[derive(Debug, Clone, Copy, Default)]
pub struct SurfaceInfo {
    pub adhesion_group: u8,
    /// Stored as ftol(grip * 10), read back * 0.1.
    pub tyre_grip: i8,
    /// Stored as ftol(wet * 100), read back * 0.01.
    pub wet_grip: i8,
    /// 0 none, 1 sparks.
    pub friction_effect: u8,
}

#[derive(Debug, Clone)]
pub struct SurfaceInfos {
    pub adhesive_limits: [[f32; NUM_GROUPS]; NUM_GROUPS],
    pub surfaces: Vec<SurfaceInfo>,
    /// CWeather::WetRoads (0..1).
    pub wet_roads: f32,
}

impl Default for SurfaceInfos {
    /// Without game data: everything is ROAD with the vanilla road/road limit.
    fn default() -> Self {
        let mut t = [[0.0; NUM_GROUPS]; NUM_GROUPS];
        t[2][2] = 6.0;
        Self { adhesive_limits: t, surfaces: Vec::new(), wet_roads: 0.0 }
    }
}

impl SurfaceInfos {
    /// Parse `data/surface.dat` and `data/surfinfo.dat`.
    pub fn parse(surface_dat: &str, surfinfo_dat: &str) -> Self {
        let mut s = Self { adhesive_limits: [[0.0; NUM_GROUPS]; NUM_GROUPS], surfaces: Vec::new(), wet_roads: 0.0 };
        let mut row = 0;
        for line in surface_dat.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') || row >= NUM_GROUPS {
                continue;
            }
            let vals: Vec<f32> = line.split_whitespace().skip(1).map(|v| v.parse().unwrap_or(0.0)).collect();
            for (col, &v) in vals.iter().enumerate().take(row + 1) {
                s.adhesive_limits[row][col] = v;
                s.adhesive_limits[col][row] = v;
            }
            row += 1;
        }
        for line in surfinfo_dat.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() < 6 || t[0].starts_with('#') || t[0].starts_with(';') {
                continue;
            }
            let group = GROUP_NAMES.iter().position(|g| g.eq_ignore_ascii_case(t[1])).unwrap_or(0) as u8;
            let f = |i: usize| t[i].parse::<f32>().unwrap_or(0.0);
            s.surfaces.push(SurfaceInfo {
                adhesion_group: group,
                tyre_grip: (f(2) * 10.0) as i8,
                wet_grip: (f(3) * 100.0) as i8,
                friction_effect: u8::from(t[5].eq_ignore_ascii_case("SPARKS")),
            });
        }
        s
    }

    fn info(&self, id: u8) -> SurfaceInfo {
        self.surfaces.get(id as usize).copied().unwrap_or(SurfaceInfo { adhesion_group: 2, tyre_grip: 10, ..Default::default() })
    }

    pub fn adhesion_group(&self, id: u8) -> u8 {
        self.info(id).adhesion_group
    }

    /// 0x55E5E0
    pub fn tyre_grip(&self, id: u8) -> f32 {
        self.info(id).tyre_grip as f32 * 0.1
    }

    /// 0x55E600
    pub fn wet_multiplier(&self, id: u8) -> f32 {
        self.info(id).wet_grip as f32 * 0.01 * self.wet_roads + 1.0
    }

    /// 0x55EB50
    pub fn adhesive_limit(&self, cp: &ColPoint) -> f32 {
        let a = self.adhesion_group(cp.surface_a) as usize;
        let b = self.adhesion_group(cp.surface_b) as usize;
        self.adhesive_limits[b][a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lower_triangular_table() {
        let s = SurfaceInfos::parse(
            "Rubber 6.0\nHard 3.6 2.0\nRoad 4.5 3.0 6.0\nLoose 3.2 3.5 2.0 1.0\nSand 3.0 4.0 2.0 1.0 1.0\nWet 2.8 2.0 1.0 1.0 1.0 0.5\n",
            "DEFAULT ROAD 1.0 -0.25 DEFAULT SPARKS 0\nGRASS LOOSE 0.8 -0.25 DEFAULT NONE 0\n",
        );
        assert_eq!(s.adhesive_limits[2][2], 6.0);
        assert_eq!(s.adhesive_limits[0][3], 3.2);
        assert_eq!(s.adhesive_limits[3][0], 3.2);
        assert_eq!(s.adhesion_group(1), 3);
        assert!((s.tyre_grip(1) - 0.8).abs() < 1e-6);
        let cp = ColPoint { surface_a: 0, surface_b: 1, ..Default::default() };
        assert_eq!(s.adhesive_limit(&cp), 2.0);
    }
}
