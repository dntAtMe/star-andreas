//! Road traffic (traffic.md): car path links (navi links, lanes), `CAutoPilot`,
//! GenerateOneRandomCar with GenerateCarCreationCoors2, SteerAICarWithPhysics →
//! FollowPath with PickNextNodeRandomly, traffic lights, slowing for cars and peds, the
//! stuck temp actions and PossiblyRemoveVehicle.
//!
//! Port choices: cars spawn directly in PHYSICS (the SIMPLE rails mode is not ported);
//! no drivers (the car drives itself), no police / gangs / dealers / boats / bikes / mad
//! drivers, FindAngleToWeaveThroughTraffic and the object slowdown are not ported, the
//! moving-rects test is simplified to a lateral-overlap time-to-gap.

use std::sync::Arc;

use glam::{Vec2, Vec3};
use sa_formats::population::NaviLink;

use crate::{
    automobile::Automobile,
    paths::{NodeAddr, PathFind, find_area_index},
    physical::{EntityType, Status},
    world::{EntityId, World},
};

/// A navi link address (`CCarPathLinkAddress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NaviAddr {
    pub area: u16,
    pub index: u16,
}

impl PathFind {
    pub fn navi(&self, a: NaviAddr) -> Option<&NaviLink> {
        self.areas.get(a.area as usize)?.as_ref()?.navi.get(a.index as usize)
    }

    /// Car links of a node: (neighbour, navi).
    pub fn car_links(&self, a: NodeAddr) -> Vec<(NodeAddr, NaviAddr)> {
        let Some(ar) = self.areas.get(a.area as usize).and_then(|x| x.as_ref()) else { return Vec::new() };
        let Some(n) = ar.nodes.get(a.node as usize) else { return Vec::new() };
        (0..n.num_links())
            .filter_map(|i| {
                let k = n.base_link as usize + i;
                let (area, node) = *ar.links.get(k)?;
                let v = *ar.navi_links.get(k)?;
                Some((NodeAddr { area, node }, NaviAddr { area: v >> 10, index: v & 0x3FF }))
            })
            .collect()
    }

    pub fn link_between(&self, a: NodeAddr, b: NodeAddr) -> Option<NaviAddr> {
        self.car_links(a).into_iter().find(|(n, _)| *n == b).map(|(_, l)| l)
    }

    fn area_loaded(&self, area: u16) -> bool {
        self.areas.get(area as usize).is_some_and(|a| a.is_some())
    }

    /// FindNodeClosestToCoors for car nodes (rings of areas).
    pub fn find_node_closest_car(&self, p: Vec3, max_dist: f32) -> Option<NodeAddr> {
        let start = find_area_index(p.x, p.y) as i32;
        let (sx, sy) = (start % 8, start / 8);
        let mut best = (max_dist, None);
        for ring in 0..2 {
            for ay in (sy - ring).max(0)..=(sy + ring).min(7) {
                for ax in (sx - ring).max(0)..=(sx + ring).min(7) {
                    if (ax - sx).abs().max((ay - sy).abs()) != ring {
                        continue;
                    }
                    let area = (ax + 8 * ay) as u16;
                    let Some(ar) = self.areas[area as usize].as_ref() else { continue };
                    for (i, n) in ar.nodes.iter().enumerate().take(ar.num_veh_nodes) {
                        if n.flags & 0x80 != 0 {
                            continue; // boats
                        }
                        let c = Vec3::new(n.pos[0] as f32, n.pos[1] as f32, n.pos[2] as f32) * 0.125;
                        let d = c - p;
                        let s = d.x.abs() + d.y.abs() + 3.0 * d.z.abs();
                        if s < best.0 {
                            best = (s, Some(NodeAddr { area, node: i as u16 }));
                        }
                    }
                }
            }
        }
        best.1
    }
}

/// Lanes on `link` for travel towards node `to`.
pub fn lanes_towards(link: &NaviLink, to: NodeAddr) -> u8 {
    if link.attached == (to.area, to.node) { link.lanes & 7 } else { (link.lanes >> 3) & 7 }
}

/// `OneWayLaneOffset` (0x44DB00).
fn one_way_lane_offset(link: &NaviLink) -> f32 {
    let (towards, against) = ((link.lanes & 7) as f32, ((link.lanes >> 3) & 7) as f32);
    if towards == 0.0 {
        0.5 - 0.5 * against
    } else if against == 0.0 {
        0.5 - 0.5 * towards
    } else {
        0.5 + link.median_width as f32 * 0.011_574
    }
}

/// The lane point and travel direction on a link (§1.2).
pub fn lane_point(link: &NaviLink, dir_sign: i8, lane: u8) -> (Vec2, Vec2) {
    let pos = Vec2::new(link.pos[0] as f32, link.pos[1] as f32) * 0.125;
    let d = Vec2::new(link.dir[0] as f32, link.dir[1] as f32) * 0.01 * dir_sign as f32;
    let off = (one_way_lane_offset(link) + lane as f32) * 5.4;
    (pos + Vec2::new(d.y, -d.x) * off, d)
}

fn dir_sign(from: NodeAddr, to: NodeAddr) -> i8 {
    if (from.area, from.node) < (to.area, to.node) { -1 } else { 1 }
}

/// `FindSpeedMultiplierWithSpeedFromNodes` (0x424130).
fn speed_mult_from_nodes(t: i8) -> f32 {
    match t {
        -1 => 0.5,
        0 => 0.65,
        2 => 2.3,
        _ => 1.0,
    }
}

/// `CTrafficLights::LightForCars1/2` (0 green, 1 amber, 2 red).
pub fn light_for_cars(ty: u8, now: u32) -> u8 {
    let t = (now >> 1) & 0x3FFF;
    if ty == 1 {
        if t < 5000 {
            0
        } else if t < 6000 {
            1
        } else {
            2
        }
    } else if t < 6000 {
        2
    } else if t < 11000 {
        0
    } else if t < 12000 {
        1
    } else {
        2
    }
}

/// `CAutoPilot` (veh+0x390).
#[derive(Debug, Clone)]
pub struct AutoPilot {
    pub cur_node: Option<NodeAddr>,
    pub next_node: Option<NodeAddr>,
    pub prev_node: Option<NodeAddr>,
    pub cur_link: Option<NaviAddr>,
    pub next_link: Option<NaviAddr>,
    pub prev_link: Option<NaviAddr>,
    pub cur_dir: i8,
    pub next_dir: i8,
    pub prev_dir: i8,
    pub cur_lane: u8,
    pub next_lane: u8,
    /// 0 STOP_FOR_CARS …
    pub style: u8,
    /// 1 CRUISE.
    pub mission: u8,
    pub temp_action: u8,
    pub temp_action_time: u32,
    pub cruise: u8,
    pub speed_type: i8,
    pub speed_mult: f32,
    pub timer_a: u32,
    pub timer_b: u32,
    last_stuck: u32,
    stuck_count: u8,
    lane_countdown: u8,
    pub seed: u16,
    pub paths: Arc<PathFind>,
}

/// A car the traffic wants created (the app loads the model and adds the body).
#[derive(Debug, Clone)]
pub struct SpawnCar {
    pub model: String,
    pub pos: Vec3,
    pub fwd: Vec3,
    pub speed: f32,
    pub ap: AutoPilot,
}

/// `CCarCtrl` statics.
pub struct Traffic {
    pub paths: Arc<PathFind>,
    /// cargrp.dat model names per group.
    pub groups: Vec<Vec<String>>,
    /// Vehicle model names allowed as random traffic (cars).
    pub car_models: std::collections::HashSet<String>,
    pub requests: Vec<SpawnCar>,
    pub count_at_start: u8,
    pub removed: Vec<EntityId>,
}

impl Traffic {
    pub fn new(paths: Arc<PathFind>, groups: Vec<Vec<String>>, car_models: std::collections::HashSet<String>) -> Self {
        Self { paths, groups, car_models, requests: Vec::new(), count_at_start: 2, removed: Vec::new() }
    }
}

impl World {
    /// `CCarCtrl::GenerateRandomCars` (0x4341C0) + `RemoveDistantCars` (0x42CD10).
    pub(crate) fn update_traffic(&mut self) {
        let Some(mut tr) = self.traffic.take() else { return };
        let Some(player) = self.player_id().and_then(|id| self.body(id)).map(|b| b.phys.matrix.pos) else {
            self.traffic = Some(tr);
            return;
        };
        let planes = self.camera_planes;
        let visible = move |c: Vec3, r: f32| planes.iter().all(|(n, d)| n.dot(c) - d <= r);
        // Removal (PossiblyRemoveVehicle distance rule) and the random car count.
        let mut num_random = 0u32;
        let mut remove = Vec::new();
        for id in self.body_ids() {
            let Some(b) = self.body(id) else { continue };
            let Some(car) = b.logic.as_any().downcast_ref::<Automobile>() else { continue };
            if car.autopilot.is_none() || b.phys.status == Status::Player {
                continue;
            }
            num_random += 1;
            let d = (b.phys.matrix.pos.truncate() - player.truncate()).length();
            let on_screen = visible(b.phys.matrix.pos, b.col.bound_radius);
            let r = if on_screen { 170.0 } else { 45.0 };
            if d > r {
                remove.push(id);
            }
        }
        for id in remove {
            for p in self.vehicle_occupants(id) {
                self.remove(p);
                self.npc_removed.push(p);
            }
            self.remove(id);
            tr.removed.push(id);
            num_random -= 1;
        }
        let num_cars = self.population.as_ref().map_or(6.0, |p| p.num_cars);
        let group_perc = self.population.as_ref().map_or([0u8; 18], |p| p.group_perc);
        if num_random < 45 {
            let tries = if tr.count_at_start > 0 {
                tr.count_at_start -= 1;
                if tr.count_at_start == 0 { 100 } else { 0 }
            } else {
                2
            };
            for _ in 0..tries {
                if (12.0f32).min(num_cars) <= (num_random + tr.requests.len() as u32) as f32 {
                    break;
                }
                if let Some(req) = self.generate_one_random_car(&tr, player, &group_perc, &visible) {
                    // Not on top of a car created this frame.
                    if tr.requests.iter().all(|r| (r.pos.truncate() - req.pos.truncate()).length() > 8.0) {
                        tr.requests.push(req);
                    }
                }
            }
        }
        self.traffic = Some(tr);
    }

    fn frand(&mut self) -> f32 {
        self.rng.next() as f32 * 3.051_850_9e-5
    }

    /// `GenerateOneRandomCar` (0x430056), civilian cars only.
    fn generate_one_random_car(
        &mut self,
        tr: &Traffic,
        player: Vec3,
        perc: &[u8; 18],
        visible: &dyn Fn(Vec3, f32) -> bool,
    ) -> Option<SpawnCar> {
        let paths = tr.paths.clone();
        // ChooseModel 'other': a random model of a popcycle group's car group.
        let mut r = (self.frand() * 100.0) as i32;
        let mut g = 0;
        while g < 17 && r >= perc[g] as i32 {
            r -= perc[g] as i32;
            g += 1;
        }
        let group = tr.groups.get(g)?;
        let models: Vec<&String> = group.iter().filter(|m| tr.car_models.contains(*m)).collect();
        if models.is_empty() {
            return None;
        }
        let model = models[(self.rng.next() as usize) % models.len()].clone();

        // GenerateCarCreationCoors2: walk the roads to the 160 m (visible) or 38 m (hidden) ring.
        let cam_fwd = Vec2::new(self.camera_fwd.x, self.camera_fwd.y).normalize_or(Vec2::Y);
        let (cos_limit, in_cone) = if self.frame & 1 == 0 { (0.707, true) } else { (0.707, false) };
        let (far_r, near_r) = (160.0f32, 38.0f32);
        let mut cur = paths.find_node_closest_car(player, 200.0)?;
        let mut visited = vec![cur];
        let mut walked = 0.0;
        let p2 = player.truncate();
        let (a, b, pos, mut frac) = loop {
            if visited.len() > 30 || walked > 230.0 {
                return None;
            }
            let mut links = paths.car_links(cur);
            let n = links.len();
            if n == 0 {
                return None;
            }
            let start = self.rng.next() as usize % n;
            links.rotate_left(start);
            let nb = links.iter().map(|x| x.0).find(|nb| !visited.contains(nb) && paths.area_loaded(nb.area))?;
            let (cc, cn) = (paths.coors(cur), paths.coors(nb));
            let (dc, dn) = ((cc.truncate() - p2).length(), (cn.truncate() - p2).length());
            let mut accept = None;
            if (dc - far_r) * (dn - far_r) < 0.0 {
                let (ea, eb) = ((dc - far_r).abs(), (dn - far_r).abs());
                let pos = (cc * eb + cn * ea) / (ea + eb);
                if visible(pos, 5.0) {
                    accept = Some((pos, ea / (ea + eb)));
                }
            }
            if accept.is_none() && (dc - near_r) * (dn - near_r) < 0.0 {
                let (ea, eb) = ((dc - near_r).abs(), (dn - near_r).abs());
                let pos = (cc * eb + cn * ea) / (ea + eb);
                if !visible(pos, 5.0) {
                    accept = Some((pos, ea / (ea + eb)));
                }
            }
            if let Some((pos, f)) = accept {
                if (cc.z - cn.z).abs() <= 0.5 * (cc - cn).truncate().length() {
                    let (a, b, f) = if self.rng.next() & 8 == 0 { (nb, cur, 1.0 - f) } else { (cur, nb, f) };
                    let c = (pos.truncate() - p2).normalize_or_zero().dot(cam_fwd);
                    if in_cone != (c > cos_limit) {
                        return None;
                    }
                    break (a, b, pos, f);
                }
            }
            visited.push(nb);
            walked += (cn - cc).length();
            cur = nb;
        };
        let (na, nbn) = (paths.node(a)?, paths.node(b)?);
        if (self.rng.next() & 15) as u8 > na.spawn_prob().min(nbn.spawn_prob()) || (na.flags | nbn.flags) & 0x80 != 0 {
            return None;
        }
        let link_ab = paths.link_between(a, b)?;
        let lanes = lanes_towards(paths.navi(link_ab)?, b);
        if lanes == 0 {
            return None;
        }
        // Nothing within 8 m.
        // FindObjectsKindaColliding(pos, 8, 2D, vehicles + peds).
        let blocked = self.body_ids().into_iter().any(|id| {
            self.body(id).is_some_and(|b| {
                matches!(b.phys.kind, EntityType::Vehicle | EntityType::Ped)
                    && (b.phys.matrix.pos.truncate() - pos.truncate()).length() < 8.0 + b.col.bound_radius
            })
        });
        if blocked {
            return None;
        }
        let cruise = (13.0 + self.frand() * 8.0) as u8;
        let lane = (self.rng.next() % lanes as u32) as u8;
        // The link the car comes from: another link of A.
        let a_links = paths.car_links(a);
        if a_links.len() < 2 {
            return None;
        }
        let (c_node, cur_link) = loop {
            let k = self.rng.next() as usize % a_links.len();
            if a_links[k].1 != link_ab {
                break a_links[k];
            }
        };
        let half = 3.0f32;
        let l = (paths.coors(a) - paths.coors(b)).truncate().length();
        frac = if 0.5 * l < half { 0.5 } else { frac.clamp(half / l, 1.0 - half / l) };
        let (ca, cb) = (paths.coors(a), paths.coors(b));
        let mut pos = ca + (cb - ca) * frac;
        let fwd = (cb - ca).normalize_or(Vec3::Y);
        let speed_type = ((nbn.flags >> 12) & 3) as i8;
        let seed = self.rng.next() as u16;
        let mut ap = AutoPilot {
            cur_node: Some(a),
            next_node: Some(b),
            prev_node: Some(c_node),
            cur_link: Some(cur_link),
            next_link: Some(link_ab),
            prev_link: None,
            cur_dir: dir_sign(c_node, a),
            next_dir: dir_sign(a, b),
            prev_dir: 1,
            cur_lane: lane,
            next_lane: lane,
            style: 0,
            mission: 1,
            temp_action: 0,
            temp_action_time: 0,
            cruise,
            speed_type,
            speed_mult: speed_mult_from_nodes(speed_type),
            timer_a: self.now_ms,
            timer_b: self.now_ms,
            last_stuck: 0,
            stuck_count: 0,
            lane_countdown: (self.rng.next() & 7) as u8 + 2,
            seed,
            paths: paths.clone(),
        };
        // Past the AB midpoint: advance one node.
        if frac > 0.5 {
            self.pick_next_node_randomly(&mut ap);
        }
        // Ground.
        let gz = self.find_ground_z(pos + Vec3::new(0.0, 0.0, 3.0))?;
        if (gz - pos.z).abs() > 7.0 {
            return None;
        }
        pos.z = gz + 1.0;
        Some(SpawnCar { model, pos, fwd, speed: cruise as f32 / 60.0 * ap.speed_mult, ap })
    }

    /// `CCarCtrl::JoinCarWithRoadSystem` (0x42F5A0) + `FindLinksToGoWithTheseNodes`: the
    /// closest car node, its neighbour with the shortest link ordered along the car's heading,
    /// lanes 0; then PHYSICS with the given mission / style / cruise (a new CAutoPilot for a
    /// car that had none).
    pub fn join_car_with_road_system(&mut self, veh: EntityId, mission: u8, style: u8, cruise: u8) {
        let Some(paths) = self.traffic.as_ref().map(|t| t.paths.clone()) else { return };
        let Some((pos, fwd)) = self.body(veh).map(|b| (b.phys.matrix.pos, b.phys.matrix.fwd)) else { return };
        let Some(n) = paths.find_node_closest_car(pos, 1.0e6) else { return };
        let links = paths.car_links(n);
        let Some(&(nb, _)) = links.iter().min_by(|a, b| {
            let d = |x: NodeAddr| (paths.coors(x) - paths.coors(n)).length();
            d(a.0).total_cmp(&d(b.0))
        }) else {
            return;
        };
        let ahead = (paths.coors(nb) - paths.coors(n)).truncate().dot(fwd.truncate()) >= 0.0;
        let (cur, next) = if ahead { (n, nb) } else { (nb, n) };
        let Some(next_link) = paths.link_between(cur, next) else { return };
        let other = paths.car_links(cur).into_iter().find(|&(x, _)| x != next);
        let seed = self.rng.next() as u16;
        let now = self.now_ms;
        let speed_type = paths.node(next).map_or(1, |x| ((x.flags >> 12) & 3) as i8);
        let Some(b) = self.body_mut(veh) else { return };
        if b.phys.status != Status::Wrecked {
            b.phys.status = Status::Physics;
        }
        let Some(car) = b.logic.as_any_mut().downcast_mut::<Automobile>() else { return };
        let mut ap = car.autopilot.take().unwrap_or_else(|| AutoPilot {
            cur_node: None,
            next_node: None,
            prev_node: None,
            cur_link: None,
            next_link: None,
            prev_link: None,
            cur_dir: 1,
            next_dir: 1,
            prev_dir: 1,
            cur_lane: 0,
            next_lane: 0,
            style: 0,
            mission: 1,
            temp_action: 0,
            temp_action_time: 0,
            cruise: 0,
            speed_type,
            speed_mult: speed_mult_from_nodes(speed_type),
            timer_a: now,
            timer_b: now,
            last_stuck: 0,
            stuck_count: 0,
            lane_countdown: 2,
            seed,
            paths: paths.clone(),
        });
        ap.prev_node = None;
        ap.cur_node = Some(cur);
        ap.next_node = Some(next);
        ap.next_link = Some(next_link);
        ap.next_dir = dir_sign(cur, next);
        ap.cur_link = other.map(|o| o.1);
        ap.cur_dir = other.map_or(1, |o| if (o.0.area, o.0.node) < (cur.area, cur.node) { -1 } else { 1 });
        ap.cur_lane = 0;
        ap.next_lane = 0;
        ap.mission = mission;
        ap.style = style;
        ap.cruise = cruise;
        ap.temp_action = 0;
        car.autopilot = Some(ap);
        car.engine_on = true;
    }

    /// `PickNextNodeRandomly` (0x42DE80).
    pub(crate) fn pick_next_node_randomly(&mut self, ap: &mut AutoPilot) {
        let paths = ap.paths.clone();
        let (Some(prev), Some(cur), Some(next_link)) = (ap.cur_node, ap.next_node, ap.next_link) else { return };
        let Some(nl) = paths.navi(next_link).copied() else { return };
        let here = lanes_towards(&nl, cur);
        let opposite = if nl.attached == (cur.area, cur.node) { (nl.lanes >> 3) & 7 } else { nl.lanes & 7 };
        let mut allowed = 0u8;
        if ap.next_lane == 0 {
            allowed |= 4;
        }
        if here > 0 && ap.next_lane == here - 1 {
            allowed |= 2;
        }
        if here < 3 || allowed == 0 {
            allowed |= 1;
        }
        ap.prev_node = Some(prev);
        ap.cur_node = Some(cur);
        let links = paths.car_links(cur);
        let n = links.len();
        if n == 0 {
            return;
        }
        let start = self.rng.next() as usize % n;
        let fwd = (self.rng.next() >> 4) & 1 == 1;
        let order: Vec<usize> = (0..n).map(|i| (start + if fwd { i } else { n - i }) % n).collect();
        let pc = paths.coors(prev).truncate();
        let cc = paths.coors(cur).truncate();
        let usable = |cand: NodeAddr| -> bool {
            if cand == prev {
                return false;
            }
            let (pf, cf) = (paths.node(cur).map_or(0, |x| x.flags), paths.node(cand).map_or(0, |x| x.flags));
            if (pf & 0x80) != (cf & 0x80) {
                return false;
            }
            if cf & 0x30 != 0 && pf & 0x30 == 0 {
                return false;
            }
            true
        };
        let mut chosen = None;
        for &k in &order {
            let (cand, link) = links[k];
            let Some(l) = paths.navi(link) else { continue };
            let a = (cc - pc).normalize_or_zero();
            let b = (paths.coors(cand).truncate() - cc).normalize_or_zero();
            let dot = a.dot(b);
            let turn = if dot > 0.4 {
                1
            } else if a.x * b.y - b.x * a.y <= 0.0 {
                2
            } else {
                4
            };
            let out = lanes_towards(l, cand);
            let back = lanes_towards(l, cur);
            if usable(cand) && out > 0 && turn & allowed != 0 && (opposite != 0 || back > 0) {
                chosen = Some((cand, link));
                break;
            }
        }
        if chosen.is_none() {
            for &k in &order {
                let (cand, link) = links[k];
                let Some(l) = paths.navi(link) else { continue };
                if cand != prev && lanes_towards(l, cand) > 0 {
                    chosen = Some((cand, link));
                    break;
                }
            }
        }
        let (nn, nlink) = chosen.unwrap_or((prev, next_link));
        ap.prev_dir = ap.cur_dir;
        ap.prev_link = ap.cur_link;
        ap.cur_link = ap.next_link;
        ap.cur_dir = ap.next_dir;
        ap.cur_lane = ap.next_lane;
        ap.next_node = Some(nn);
        ap.next_link = Some(nlink);
        ap.next_dir = dir_sign(cur, nn);
        let num_lanes = paths.navi(nlink).map_or(1, |l| lanes_towards(l, nn)).max(1);
        if (paths.coors(nn).truncate() - cc).length_squared() > 256.0 {
            ap.lane_countdown = ap.lane_countdown.saturating_sub(1);
            if ap.lane_countdown == 0 {
                ap.lane_countdown = (self.rng.next() & 3) as u8 + 4;
                if self.rng.next() < 0x3FFF {
                    ap.next_lane = ap.next_lane.saturating_add(1);
                } else {
                    ap.next_lane = ap.next_lane.saturating_sub(1);
                }
            }
        }
        ap.next_lane = ap.next_lane.min(num_lanes - 1);
        ap.speed_type = paths.node(nn).map_or(1, |x| ((x.flags >> 12) & 3) as i8);
    }

    /// The AI of the random cars (CCarAI::UpdateCarAI + SteerAICarWithPhysics), before
    /// their ProcessControl.
    pub(crate) fn traffic_ai(&mut self) {
        let now = self.now_ms;
        let ts = self.last_ts;
        let ids: Vec<EntityId> = self
            .body_ids()
            .into_iter()
            .filter(|&id| {
                self.body(id).is_some_and(|b| {
                    b.phys.status == Status::Physics
                        && b.logic.as_any().downcast_ref::<Automobile>().is_some_and(|c| c.autopilot.is_some() && (c.driver.is_some() || c.awaiting_occupants))
                })
            })
            .collect();
        for id in ids {
            let Some(mut ap) = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()).and_then(|c| c.autopilot.take())
            else {
                continue;
            };
            let (pos, fwd, mv, up_z, max_y, max_x) = {
                let b = self.body(id).unwrap();
                (b.phys.matrix.pos, b.phys.matrix.fwd, b.phys.move_speed, b.phys.matrix.up.z, b.col.bbox_max.y, b.col.bbox_max.x)
            };
            // UpdateCarAI tail: stuck detection and the node speed multiplier.
            let v2 = mv.truncate().length_squared();
            if v2 > 0.0025 {
                ap.timer_a = now;
                ap.timer_b = now;
            }
            if ap.temp_action == 0 && ap.cruise != 0 && v2 < 0.000_144 {
                let t = if matches!(ap.style, 0 | 4) { ((ap.seed as u32 & 15) + 40) * 500 } else { 1000 };
                if now.wrapping_sub(ap.timer_a) > t {
                    ap.stuck_count = if now < ap.last_stuck + 10000 { (ap.stuck_count + 1) & 3 } else { 0 };
                    ap.last_stuck = now;
                    ap.temp_action = 3;
                    ap.temp_action_time = now + 700;
                    ap.timer_a = now;
                    ap.style = 2;
                }
            }
            if up_z < -0.7 {
                ap.temp_action = 1;
                ap.temp_action_time = now + 1000;
            }
            let target = speed_mult_from_nodes(ap.speed_type);
            let step = ts * 0.01;
            ap.speed_mult = if (target - ap.speed_mult).abs() < step { target } else { ap.speed_mult + step * (target - ap.speed_mult).signum() };

            // SteerAICarWithPhysics.
            let (mut steer, mut gas, mut brake, mut handbrake) = (0.0f32, 0.0f32, 0.0f32, false);
            // CTaskComplexDieInCar: PreparePedVehicleForPedDeath (2 s handbrake straight), then
            // mission NONE (brake 0.5 + handbrake).
            let driver = self.body(id).and_then(|b| b.logic.as_any().downcast_ref::<Automobile>()).and_then(|c| c.driver);
            let driver_dead = driver
                .and_then(|d| self.body(d))
                .and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>())
                .is_some_and(|p| !p.tasks.health.alive());
            if driver_dead {
                let since = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()).map_or(now, |c| *c.driver_died_at.get_or_insert(now));
                ap.cruise = 0;
                ap.mission = 0;
                if now < since + 2000 {
                    ap.temp_action = 6;
                    ap.temp_action_time = since + 2000;
                }
            }
            let expired = now > ap.temp_action_time;
            match ap.temp_action {
                6 if !expired => {
                    // HANDBRAKE STRAIGHT.
                    handbrake = true;
                }
                _ if ap.mission == 0 => {
                    brake = 0.5;
                    handbrake = true;
                }
                1 | 24 => {
                    brake = if ap.temp_action == 1 { 0.2 } else { 1.0 };
                    if expired {
                        ap.temp_action = 0;
                        ap.timer_a = now;
                        ap.timer_b = now;
                    }
                }
                3 => {
                    let (s, _, _) = self.follow_path(id, &mut ap, pos, fwd, mv, max_x, max_y);
                    steer = -s;
                    if mv.dot(fwd) > 0.04 {
                        brake = 0.5;
                    } else {
                        gas = -0.5;
                    }
                    if expired {
                        ap.temp_action = 0;
                    }
                }
                _ => {
                    let (s, g, b) = self.follow_path(id, &mut ap, pos, fwd, mv, max_x, max_y);
                    steer = s;
                    gas = g;
                    brake = b;
                    handbrake = false;
                }
            }
            if let Some(car) = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
                car.steer_angle = steer;
                car.gas = gas;
                car.brake = brake;
                car.handbrake = handbrake;
                car.engine_on = true;
                car.autopilot = Some(ap);
            }
        }
    }

    /// `SteerAICarWithPhysicsFollowPath` (0x434900): (steer, gas, brake).
    #[allow(clippy::too_many_arguments)]
    fn follow_path(&mut self, id: EntityId, ap: &mut AutoPilot, pos: Vec3, fwd: Vec3, mv: Vec3, max_x: f32, max_y: f32) -> (f32, f32, f32) {
        let paths = ap.paths.clone();
        let fwd2 = fwd.truncate().normalize_or(Vec2::Y);
        let lane = |ap: &AutoPilot| -> Option<(Vec2, Vec2, Vec2, Vec2)> {
            let (cl, nl) = (paths.navi(ap.cur_link?)?, paths.navi(ap.next_link?)?);
            let (pc, dc) = lane_point(cl, ap.cur_dir, ap.cur_lane);
            let (pn, dn) = lane_point(nl, ap.next_dir, ap.next_lane);
            Some((pc, dc, pn, dn))
        };
        let Some((mut p_cur, mut d_cur, mut p_next, mut d_next)) = lane(ap) else { return (0.0, 0.0, 1.0) };
        let car = pos.truncate();
        let mut to_car = car - p_cur;
        let mut dist = to_car.length();
        let seg = p_next - p_cur;
        let proj = seg.dot(to_car);
        let reached = !(dist >= 5.0 && (proj <= 0.0 || dist >= 8.0) && proj / (seg.length() * dist).max(1e-6) <= 0.7 && ap.next_link != ap.cur_link);
        if reached {
            self.pick_next_node_randomly(ap);
            if let Some(l) = lane(ap) {
                (p_cur, d_cur, p_next, d_next) = l;
                to_car = car - p_cur;
                dist = to_car.length();
            }
        }
        let _ = p_next;
        let target = if dist > 40.0 { p_cur } else { p_cur - d_cur * dist * 0.35 };
        let at = |v: Vec2| v.y.atan2(v.x);
        let a_t = at(target - car);
        let a_c = at(fwd2);
        let mut d = a_t - a_c;
        while d > std::f32::consts::PI {
            d -= std::f32::consts::TAU;
        }
        while d < -std::f32::consts::PI {
            d += std::f32::consts::TAU;
        }
        let v = mv.length();
        let max_steer = if v > 0.7 { 0.2 } else { (0.9 - v).min(0.7) };
        let steer = d.clamp(-max_steer, max_steer);
        let fwd_speed = mv.dot(fwd) * 60.0;
        let cruise = ap.cruise as f32;
        let mut traffic = if matches!(ap.style, 0 | 1 | 4 | 6) { self.max_speed_in_traffic(id, ap, pos, fwd, max_x, max_y) / cruise.max(1.0) } else { 1.0 };
        if matches!(ap.style, 0 | 1 | 5 | 6) && self.should_car_stop_for_light(ap, pos) {
            ap.timer_a = self.now_ms;
            traffic = 0.0;
        }
        let speed_mult = |a: f32, lo: f32, hi: f32, k: f32| {
            let mut a = a;
            while a > std::f32::consts::PI {
                a -= std::f32::consts::TAU;
            }
            while a < -std::f32::consts::PI {
                a += std::f32::consts::TAU;
            }
            let x = (a.abs() - lo).max(0.0);
            if x > hi - lo { k } else { 1.0 - x / (hi - lo) * (1.0 - k) }
        };
        let m_lane = speed_mult(at(p_cur - car) - a_c, 0.4, 1.2, 0.4);
        let m_bend = speed_mult(at(d_cur) - at(d_next), 0.1, 1.2, 0.4);
        let m_dist = if dist <= 40.0 && cruise > 11.0 { 1.0 - (1.0 - m_bend) * (1.0 - dist * 0.025) } else { 1.0 };
        let desired = cruise * ap.speed_mult * m_lane.min(m_dist).min(traffic);
        let dv = desired - fwd_speed;
        let (gas, brake) = if desired < 0.05 && dv < 0.03 {
            (0.0, 1.0)
        } else if dv <= 0.0 {
            (0.0, (dv * (-1.0 / 12.0)).min(0.5))
        } else {
            ((dv * if fwd_speed >= 2.0 { 0.125 } else { 0.25 }).min(1.0), 0.0)
        };
        (steer, gas, brake)
    }

    /// `ShouldCarStopForLight` (bAlwaysStop = false).
    fn should_car_stop_for_light(&self, ap: &AutoPilot, pos: Vec3) -> bool {
        let paths = &ap.paths;
        for (link, node, dir, range) in [(ap.cur_link, ap.cur_node, ap.cur_dir, 12.0f32), (ap.next_link, ap.next_node, ap.next_dir, 12.0), (ap.prev_link, ap.prev_node, ap.prev_dir, 6.0)] {
            let (Some(link), Some(node)) = (link, node) else { continue };
            let Some(l) = paths.navi(link) else { continue };
            let ty = l.flags & 3;
            if ty == 0 {
                continue;
            }
            let bit6 = (l.lanes >> 6) & 1 == 1;
            let at = l.attached == (node.area, node.node);
            if bit6 != at {
                continue;
            }
            if light_for_cars(ty, self.now_ms) == 0 {
                return false;
            }
            let lp = Vec2::new(l.pos[0] as f32, l.pos[1] as f32) * 0.125;
            let d = Vec2::new(l.dir[0] as f32, l.dir[1] as f32) * 0.01;
            let s = (pos.truncate() - lp).dot(d);
            return if dir == -1 { 0.0 < s && s < range } else { -range < s && s < 0.0 };
        }
        false
    }

    /// `FindMaximumSpeedForThisCarInTraffic` (0x434400): cars and peds ahead in a 14 m box.
    fn max_speed_in_traffic(&self, id: EntityId, ap: &mut AutoPilot, pos: Vec3, fwd: Vec3, max_x: f32, max_y: f32) -> f32 {
        let cruise = ap.cruise as f32 * ap.speed_mult;
        let mut max = cruise;
        let fwd2 = fwd.truncate().normalize_or(Vec2::Y);
        let right2 = Vec2::new(fwd2.y, -fwd2.x);
        for oid in self.body_ids() {
            if oid == id {
                continue;
            }
            let Some(o) = self.body(oid) else { continue };
            if !o.phys.has_e(crate::physical::ef::USES_COLLISION) {
                continue;
            }
            let d3 = o.phys.matrix.pos - pos;
            if d3.x.abs() > 14.0 || d3.y.abs() > 14.0 || d3.z.abs() > 10.0 {
                continue;
            }
            let d = d3.truncate();
            let along = d.dot(fwd2);
            if along < 0.0 {
                continue;
            }
            match o.phys.kind {
                EntityType::Vehicle => {
                    // SlowCarDownForOtherCar with a lateral-overlap gap test.
                    let side = d.dot(right2).abs();
                    let ow = o.col.bbox_max.x.max(-o.col.bbox_min.x);
                    let gap = along - max_y - (-o.col.bbox_min.y).max(0.0);
                    let rel = o.phys.move_speed.truncate().dot(fwd2) * 60.0 - cruise;
                    let t = if side < max_x + ow && rel < 0.0 { (gap / -rel).clamp(0.0, 1.0) } else { 1.0 };
                    if t < 1.5 {
                        max = if t < 1.0 / cruise.max(0.01) {
                            0.0
                        } else if t < 3.0 / cruise.max(0.01) {
                            max.min(1.0)
                        } else {
                            max.min((t - 0.2).max(0.0) * 0.769_23 * cruise)
                        };
                    }
                }
                EntityType::Ped => {
                    let side = d.dot(right2).abs();
                    let gap = along - max_y;
                    let vf = o.phys.move_speed.dot(fwd);
                    let _ = vf;
                    if gap > 0.0 && side < max_x + 0.5 && gap < 13.0 {
                        max = max.min((1.0f32).max((gap - 1.0) * (1.0 / 13.0) * cruise));
                        if gap < 2.5 {
                            ap.temp_action = 24;
                            ap.temp_action_time = self.now_ms + 4000;
                        } else if gap < 4.0 {
                            ap.temp_action = 1;
                            ap.temp_action_time = self.now_ms + 4000;
                        }
                    }
                }
                _ => {}
            }
        }
        if matches!(ap.style, 0 | 4) { max } else { (cruise + max) * 0.5 }
    }
}

