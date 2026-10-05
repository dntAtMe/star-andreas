//! `CPathFind` for peds (population.md §1.8, §1.9, §2.3): the 64 path areas of
//! nodes0..63.dat, FindNodeClosestToCoors, FindNextNodeWandering, the width offsets and
//! GeneratePedCreationCoors.
//!
//! Only the ped nodes are used here (vehicle nodes come with traffic).

use glam::{Vec2, Vec3};
use sa_formats::population::{PathArea, PathNode};

/// `CNodeAddress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeAddr {
    pub area: u16,
    pub node: u16,
}

#[derive(Debug, Default)]
pub struct PathFind {
    pub areas: Vec<Option<PathArea>>,
}

/// `FindAreaIndex` (0x44D830).
pub fn find_area_index(x: f32, y: f32) -> usize {
    let ax = (((x + 3000.0) / 750.0) as i32).clamp(0, 7);
    let ay = (((y + 3000.0) / 750.0) as i32).clamp(0, 7);
    (ax + 8 * ay) as usize
}

impl PathFind {
    pub fn new(areas: Vec<Option<PathArea>>) -> Self {
        Self { areas }
    }

    pub fn node(&self, a: NodeAddr) -> Option<&PathNode> {
        self.areas.get(a.area as usize)?.as_ref()?.nodes.get(a.node as usize)
    }

    /// `GetNodeCoors` (0x420A10).
    pub fn coors(&self, a: NodeAddr) -> Vec3 {
        self.node(a).map_or(Vec3::ZERO, |n| Vec3::new(n.pos[0] as f32, n.pos[1] as f32, n.pos[2] as f32) * 0.125)
    }

    fn loaded(&self, area: u16) -> bool {
        self.areas.get(area as usize).is_some_and(|a| a.is_some())
    }

    /// Links of a node: (neighbour, intersection byte).
    pub fn links(&self, a: NodeAddr) -> Vec<(NodeAddr, u8)> {
        let Some(ar) = self.areas.get(a.area as usize).and_then(|x| x.as_ref()) else { return Vec::new() };
        let Some(n) = ar.nodes.get(a.node as usize) else { return Vec::new() };
        (0..n.num_links())
            .filter_map(|i| {
                let k = n.base_link as usize + i;
                let (area, node) = *ar.links.get(k)?;
                Some((NodeAddr { area, node }, *ar.intersections.get(k).unwrap_or(&0)))
            })
            .collect()
    }

    /// `FindNodeClosestToCoors` (0x44F460), ped nodes, rings of areas around the start area.
    pub fn find_node_closest_ped(&self, p: Vec3) -> Option<NodeAddr> {
        let start = find_area_index(p.x, p.y) as i32;
        let (sx, sy) = (start % 8, start / 8);
        let mut best = (f32::MAX, None);
        for ring in 0..4 {
            if ring as f32 * 750.0 > best.0 {
                break;
            }
            for ay in (sy - ring).max(0)..=(sy + ring).min(7) {
                for ax in (sx - ring).max(0)..=(sx + ring).min(7) {
                    if (ax - sx).abs().max((ay - sy).abs()) != ring {
                        continue;
                    }
                    let area = (ax + 8 * ay) as u16;
                    let Some(ar) = self.areas[area as usize].as_ref() else { continue };
                    for (i, n) in ar.nodes.iter().enumerate().skip(ar.num_veh_nodes) {
                        let c = Vec3::new(n.pos[0] as f32, n.pos[1] as f32, n.pos[2] as f32) * 0.125;
                        let d = c - p;
                        let s = 0.3 * (d.x.abs() + d.y.abs() + 3.0 * d.z.abs());
                        if s < best.0 {
                            let a = NodeAddr { area, node: i as u16 };
                            let s2 = s + 0.2 * self.dist_to_links(a, p);
                            if s2 < best.0 {
                                best = (s2, Some(a));
                            }
                        }
                    }
                }
            }
        }
        best.1
    }

    /// `CalcDistToAnyConnectingLinks` (0x44F190): the distance from `p` to the node's link
    /// segments.
    fn dist_to_links(&self, a: NodeAddr, p: Vec3) -> f32 {
        let c = self.coors(a);
        let mut best = f32::MAX;
        for (nb, _) in self.links(a) {
            if !self.loaded(nb.area) {
                continue;
            }
            let e = self.coors(nb);
            let seg = e - c;
            let t = if seg.length_squared() > 0.0 { ((p - c).dot(seg) / seg.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min((c + seg * t - p).length());
        }
        if best == f32::MAX { 0.0 } else { best }
    }

    /// `FindNextNodeWandering` (0x451B70) for peds: the link best aligned with compass
    /// direction `dir` (0 = +Y, 2 = +X, clockwise). Returns the new direction octant.
    pub fn find_next_node_wandering(&self, pos: Vec3, last: &mut Option<NodeAddr>, next: &mut Option<NodeAddr>, dir: u8) -> u8 {
        let original = *last;
        let mut start = *last;
        match start {
            None => start = self.find_node_closest_ped(pos),
            Some(s) if !self.loaded(s.area) => {
                *next = None;
                return 0;
            }
            Some(s) => {
                let n = self.node(s).copied();
                let w = n.map_or(0.0, |n| n.width as f32 * 0.125);
                if (pos - self.coors(s)).length() > w.max(7.0) {
                    start = self.find_node_closest_ped(pos);
                }
            }
        }
        let Some(s) = start else {
            *next = None;
            return 0;
        };
        let a = dir as f32 * std::f32::consts::FRAC_PI_4;
        let (sn, cs) = a.sin_cos();
        let sflags = self.node(s).map_or(0, |n| n.flags);
        let mut best = -999_999.0f32;
        let mut out_dir = 0;
        *next = None;
        let c0 = self.coors(s);
        for (nb, _) in self.links(s) {
            if !self.loaded(nb.area) {
                continue;
            }
            let nf = self.node(nb).map_or(0, |n| n.flags);
            if !(sflags & 0x20 != 0 || nf & 0x20 == 0) || !(sflags & 0x400 != 0 || nf & 0x400 == 0) {
                continue;
            }
            let d = (self.coors(nb) - c0).truncate().normalize_or_zero();
            let score = d.x * sn + d.y * cs;
            if score >= best {
                best = score;
                *next = Some(nb);
                out_dir = octant(d);
            }
        }
        *last = Some(s);
        if next.is_none() {
            out_dir = 0;
            *next = Some(s);
        }
        // Dead end: back along the first loaded link.
        if *next == original && original.is_some() {
            if let Some((nb, _)) = self.links(original.unwrap()).into_iter().find(|(nb, _)| self.loaded(nb.area)) {
                *next = Some(nb);
            }
        }
        out_dir
    }

    /// `TakeWidthIntoAccountForWandering` (0x4509A0).
    pub fn wander_target(&self, a: NodeAddr, seed: u16) -> Vec3 {
        let mut p = self.coors(a);
        let w = self.node(a).map_or(0.0, |n| n.width as f32);
        p.x += ((seed & 15) as f32 - 7.0) * w * 0.00775;
        p.y += (((seed >> 4) & 15) as f32 - 7.0) * w * 0.00775;
        p
    }

    /// `GeneratePedCreationCoors` (0x44E790). `visible(pos)` = IsSphereVisible(pos, 2.0),
    /// `ground(pos)` = FindGroundZFor3DCoord(x, y, z + 2), `rand()` = the global rand().
    #[allow(clippy::too_many_arguments)]
    pub fn generate_ped_creation_coors(
        &self,
        x: f32,
        y: f32,
        min_dist: f32,
        max_dist: f32,
        min_dist_off: f32,
        max_dist_off: f32,
        allow_restricted: bool,
        rand: &mut dyn FnMut() -> u32,
        frand: &mut dyn FnMut() -> f32,
        visible: &mut dyn FnMut(Vec3) -> bool,
        ground: &mut dyn FnMut(Vec3) -> Option<f32>,
    ) -> Option<(Vec3, NodeAddr, NodeAddr)> {
        let r = (frand() * 15.0) as i32 as u8;
        let max_r2 = (max_dist + 30.0) * (max_dist + 30.0);
        let area = find_area_index(x, y);
        let ar = self.areas.get(area)?.as_ref()?;
        let num_ped = ar.nodes.len() - ar.num_veh_nodes;
        if num_ped == 0 {
            return None;
        }
        let me = Vec2::new(x, y);
        for _ in 0..300 {
            let i = ar.num_veh_nodes + ((rand() >> 6) as usize % num_ped);
            let node = NodeAddr { area: area as u16, node: i as u16 };
            let n = ar.nodes[i];
            let c = self.coors(node);
            let d2 = (c.truncate() - me).length_squared();
            if d2 >= max_r2 || n.spawn_prob() <= r {
                continue;
            }
            for (nb, inter) in self.links(node) {
                if inter & 1 != 0 || nb.area >= 64 || !self.loaded(nb.area) {
                    continue;
                }
                let nbn = *self.node(nb)?;
                if (n.flags | nbn.flags) & 0x20 != 0 && !allow_restricted {
                    continue;
                }
                if nbn.spawn_prob() <= r {
                    continue;
                }
                let cb = self.coors(nb);
                if d2.sqrt() >= max_dist && (cb.truncate() - me).length() >= max_dist {
                    continue;
                }
                for _ in 0..5 {
                    let t = (rand() & 0xFF) as f32 / 256.0;
                    let pos = cb * t + c * (1.0 - t);
                    let d = (pos.truncate() - me).length();
                    let ok = if visible(pos) {
                        min_dist < d && d < max_dist
                    } else {
                        min_dist_off < d && d < max_dist_off && rand() & 1 != 0
                    };
                    if !ok {
                        continue;
                    }
                    let Some(gz) = ground(pos) else { continue };
                    if (gz - pos.z).abs() <= 3.0 {
                        return Some((Vec3::new(pos.x, pos.y, gz), node, nb));
                    }
                    return None;
                }
            }
        }
        None
    }

    /// `TakeWidthIntoAccountForCoors` (0x44DA30).
    pub fn width_offset_coors(&self, n1: NodeAddr, n2: NodeAddr, seed: u32, p: &mut Vec3) {
        let (Some(a), Some(b)) = (self.node(n1), self.node(n2)) else { return };
        let w = a.width.min(b.width) as f32;
        p.x += ((seed & 15) as f32 - 7.0) * w * 0.00775;
        p.y += (((seed >> 4) & 15) as f32 - 7.0) * w * 0.00775;
    }
}

/// The compass octant of a 2D direction (straight when the main axis is > 2× the other).
pub fn octant(d: Vec2) -> u8 {
    if d.x >= 0.0 {
        if d.x > 2.0 * d.y.abs() {
            2
        } else if d.y > 2.0 * d.x {
            0
        } else if d.y < -2.0 * d.x {
            4
        } else if d.y > 0.0 {
            1
        } else {
            3
        }
    } else if -d.x > 2.0 * d.y.abs() {
        6
    } else if d.y > -2.0 * d.x {
        0
    } else if d.y < 2.0 * d.x {
        4
    } else if d.y > 0.0 {
        7
    } else {
        5
    }
}
