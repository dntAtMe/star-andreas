//! `CWanted` / `CCrime` (wanted.md §1–§4): the player's chaos and wanted level, crimes and
//! their queue, decay, parole.
//!
//! Not ported: stats, the police scanner audio, the gang-war multipliers, the restricted
//! islands, cops on foot and police cars (pursuit list, arrests), busted / wasted.

use glam::Vec3;

use crate::world::{EntityId, World};

/// `MaximumWantedLevel` / `MaximumChaosLevel` defaults.
pub const MAX_WANTED_LEVEL: i32 = 6;
pub const MAX_CHAOS_LEVEL: i32 = 9200;

/// Chaos K per crime type 2..22 (jump table 0x5622A0).
const CRIME_CHAOS: [i32; 21] = [5, 45, 30, 80, 15, 10, 5, 5, 18, 80, 400, 20, 80, 20, 400, 25, 35, 100, 70, 2, 2];

/// `FindImmediateDetectionRange` (0x531FC0).
pub fn immediate_detection_range(ty: u8) -> f32 {
    match ty {
        12 | 16 | 17 => 60.0,
        20 => 30.0,
        _ => 14.0,
    }
}

/// `CVehicle::IsLawEnforcementVehicle` models (+0x428 & 1) and the police Maverick.
pub fn is_law_enforcement_model(model: u16) -> bool {
    matches!(model, 427 | 430 | 432 | 433 | 490 | 497 | 523 | 528 | 596 | 597 | 598 | 599 | 601)
}

/// `crimesBeingQd` entry.
#[derive(Debug, Clone, Copy, Default)]
pub struct QdCrime {
    pub ty: u8,
    pub victim: Option<EntityId>,
    pub time: u32,
    pub pos: Vec3,
    pub reported: bool,
    pub dont_care: bool,
}

/// `CWanted` (0x29C bytes).
#[derive(Debug, Clone)]
pub struct Wanted {
    pub chaos: i32,
    pub chaos_before_parole: i32,
    pub last_time_decreased: u32,
    pub last_time_level_changed: u32,
    pub time_of_parole: u32,
    pub multiplier: f32,
    pub cops_in_pursuit: u8,
    pub max_cops_in_pursuit: u8,
    pub max_cop_cars: u8,
    pub chance_on_road_block: u16,
    /// 1 PoliceBackOff, 2 PoliceBackOffGarage, 4 EverybodyBackOff, 8 SWAT, 0x10 FBI, 0x20 army.
    pub flags: u8,
    pub level: i32,
    pub level_before_parole: i32,
    pub crimes: [QdCrime; 16],
    /// +0x1F4 pursuitCops[10].
    pub pursuit_cops: [Option<EntityId>; 10],
    last_back_off: bool,
    pub max_level: i32,
    pub max_chaos: i32,
    /// The NeverWanted cheat (byte 0x969171).
    pub never_wanted: bool,
}

impl Default for Wanted {
    fn default() -> Self {
        Self {
            chaos: 0,
            chaos_before_parole: 0,
            last_time_decreased: 0,
            last_time_level_changed: 0,
            time_of_parole: 0,
            multiplier: 1.0,
            cops_in_pursuit: 0,
            max_cops_in_pursuit: 0,
            max_cop_cars: 0,
            chance_on_road_block: 0,
            flags: 0,
            level: 0,
            level_before_parole: 0,
            crimes: [QdCrime::default(); 16],
            pursuit_cops: [None; 10],
            last_back_off: false,
            max_level: MAX_WANTED_LEVEL,
            max_chaos: MAX_CHAOS_LEVEL,
            never_wanted: false,
        }
    }
}

impl Wanted {
    /// `Initialise` (0x562390).
    pub fn reset(&mut self) {
        let keep = self.flags & 0xC0;
        *self = Self { flags: keep, max_level: self.max_level, max_chaos: self.max_chaos, never_wanted: self.never_wanted, ..Self::default() };
    }

    /// `UpdateWantedLevel` (0x561C90).
    pub fn update_wanted_level(&mut self, now: u32) {
        let old = self.level;
        self.chaos = self.chaos.min(self.max_chaos);
        let (l, cars, cops, rb) = match self.chaos {
            c if c >= 4600 => (6, 3, 10, 30),
            c if c >= 2400 => (5, 3, 8, 24),
            c if c >= 1200 => (4, 2, 6, 18),
            c if c >= 550 => (3, 2, 4, 12),
            c if c >= 180 => (2, 2, 3, 0),
            c if c >= 50 => (1, 1, 1, 0),
            _ => (0, 0, 0, 0),
        };
        self.level = l;
        self.max_cop_cars = cars;
        self.max_cops_in_pursuit = cops;
        self.chance_on_road_block = rb;
        if old != l {
            self.last_time_level_changed = now;
        }
        if self.flags & 7 != 0 {
            self.max_cop_cars = 0;
            self.max_cops_in_pursuit = 0;
            self.chance_on_road_block = 0;
        }
    }

    /// `SetWantedLevel` (0x562470).
    pub fn set_wanted_level(&mut self, l: i32, now: u32) {
        if self.never_wanted {
            return;
        }
        let l = l.min(self.max_level);
        self.clear_qd_crimes();
        if let Some(&c) = [0, 70, 200, 570, 1220, 2420, 4620].get(l.max(-1) as usize) {
            if l >= 0 {
                self.chaos = c;
            }
        }
        self.update_wanted_level(now);
    }

    /// `SetWantedLevelNoDrop` (0x562570).
    pub fn set_wanted_level_no_drop(&mut self, l: i32, now: u32) {
        if self.level < self.level_before_parole {
            self.set_wanted_level(self.level_before_parole, now);
        }
        if self.level < l {
            self.set_wanted_level(l, now);
        }
    }

    /// `CheatWantedLevel` (0x562540).
    pub fn cheat_wanted_level(&mut self, l: i32, now: u32) {
        if l > self.max_level {
            self.set_maximum_wanted_level(l);
        }
        self.set_wanted_level(l, now);
        self.update_wanted_level(now);
    }

    /// `SetMaximumWantedLevel` (0x561E70).
    pub fn set_maximum_wanted_level(&mut self, l: i32) {
        let c = match l {
            0 => 0,
            1 => 115,
            2 => 365,
            3 => 875,
            4 => 1800,
            5 => 3500,
            6 => 6900,
            _ => return,
        };
        self.max_level = l;
        self.max_chaos = c;
    }

    pub fn clear_qd_crimes(&mut self) {
        for c in &mut self.crimes {
            c.ty = 0;
        }
    }

    pub fn are_swat_required(&self) -> bool {
        self.level == 4 || self.flags & 8 != 0
    }
    pub fn are_fbi_required(&self) -> bool {
        self.level == 5 || self.flags & 0x10 != 0
    }
    pub fn are_army_required(&self) -> bool {
        self.level == 6 || self.flags & 0x20 != 0
    }
    /// `NumOfHelisRequired` (0x561FA0).
    pub fn num_helis_required(&self) -> u8 {
        if self.flags & 7 != 0 {
            return 0;
        }
        match self.level {
            3 => 1,
            4..=6 => 2,
            _ => 0,
        }
    }

    /// `AddCrimeToQ`: the same crime on the same victim counts once while queued (10 s).
    pub fn add_crime_to_q(&mut self, ty: u8, victim: Option<EntityId>, pos: Vec3, reported: bool, dont_care: bool, now: u32) -> bool {
        for e in &mut self.crimes {
            if e.ty == ty && e.victim == victim {
                if e.reported {
                    return true;
                }
                if reported {
                    e.reported = true;
                }
                return false;
            }
        }
        if let Some(e) = self.crimes.iter_mut().find(|e| e.ty == 0) {
            *e = QdCrime { ty, victim, time: now, pos, reported, dont_care };
        }
        false
    }

    /// `RegisterCrime_Immediately`.
    pub fn register_crime_immediately(&mut self, ty: u8, pos: Vec3, victim: Option<EntityId>, dont_care: bool, now: u32) {
        if !self.add_crime_to_q(ty, victim, pos, true, dont_care, now) {
            self.report_crime_now(ty, dont_care, now);
        }
    }

    /// `ReportCrimeNow` (0x562120).
    pub fn report_crime_now(&mut self, ty: u8, dont_care: bool, now: u32) {
        if self.never_wanted {
            return;
        }
        let mut m = self.multiplier.max(0.0);
        if dont_care {
            m *= 0.333;
        }
        if (2..=22).contains(&ty) {
            self.chaos = (self.chaos as f32 + m * CRIME_CHAOS[ty as usize - 2] as f32) as i32;
        }
        self.chaos = self.chaos.max(self.chaos_before_parole);
        self.update_wanted_level(now);
    }

    /// `UpdateCrimesQ` (0x562760).
    pub fn update_crimes_q(&mut self, now: u32) {
        for i in 0..self.crimes.len() {
            let e = self.crimes[i];
            if e.ty == 0 {
                continue;
            }
            if e.time.wrapping_add(500) < now && !e.reported {
                self.crimes[i].reported = true;
                self.report_crime_now(e.ty, e.dont_care, now);
            }
            if e.time.wrapping_add(10000) < now {
                self.crimes[i].ty = 0;
            }
        }
    }

    /// `ClearWantedLevelAndGoOnParole` (0x5625A0): Pay'n'Spray.
    pub fn clear_and_go_on_parole(&mut self, now: u32) {
        self.chaos_before_parole = self.chaos;
        self.level_before_parole = self.level;
        self.time_of_parole = now;
        self.chaos = 0;
        self.level = 0;
    }

    /// `CWanted::Update` (0x562C90) without the chase-time stats. `open` = weather region
    /// 0 / 4 outdoors; `exempt` = in a law-enforcement vehicle or an aircraft; `police_near` =
    /// WorkOutPolicePresence(player, 18) != 0.
    pub fn update(&mut self, now: u32, open: bool, exempt: bool, police_near: bool) {
        if self.time_of_parole.wrapping_add(20000) < now {
            self.chaos_before_parole = 0;
            self.level_before_parole = 0;
        }
        if now.wrapping_sub(self.last_time_decreased) > 1000 {
            if self.level <= 1 || open {
                if exempt {
                    self.last_time_decreased = now;
                } else if !police_near {
                    self.last_time_decreased = now;
                    self.chaos = (self.chaos - if open { 2 } else { 1 }).max(0);
                    self.update_wanted_level(now);
                }
            } else {
                self.last_time_decreased = now;
            }
            self.update_crimes_q(now);
        }
        let back_off = self.flags & 7 != 0;
        if self.last_back_off != back_off {
            self.update_wanted_level(now);
            self.last_back_off = back_off;
        }
    }
}

impl Wanted {
    pub fn is_in_pursuit(&self, cop: EntityId) -> bool {
        self.pursuit_cops.contains(&Some(cop))
    }

    pub fn remove_pursuit_cop(&mut self, cop: EntityId) {
        for s in &mut self.pursuit_cops {
            if *s == Some(cop) {
                *s = None;
                self.cops_in_pursuit = self.cops_in_pursuit.saturating_sub(1);
            }
        }
    }

    /// `ComputePursuitCopToDisplace`: the farthest cop farther than the newcomer (a dying cop
    /// first). `dist2(c)` is the squared distance from the player.
    fn cop_to_displace(&self, cop: Option<EntityId>, dist2: &dyn Fn(EntityId) -> Option<f32>) -> Option<EntityId> {
        let mut best = cop.and_then(dist2).map_or(0.0, |d| d.max(1.0));
        let mut res = None;
        for c in self.pursuit_cops.iter().flatten() {
            match dist2(*c) {
                None => return Some(*c),
                Some(d) if d > best => {
                    best = d;
                    res = Some(*c);
                }
                _ => {}
            }
        }
        res
    }

    /// `CanCopJoinPursuit` (0x562FB0) on a copy of the list.
    pub fn can_cop_join_pursuit(&self, cop: EntityId, dist2: &dyn Fn(EntityId) -> Option<f32>) -> bool {
        if self.flags & 7 != 0 || self.max_cops_in_pursuit == 0 {
            return false;
        }
        let mut copy = self.clone();
        while copy.cops_in_pursuit >= copy.max_cops_in_pursuit {
            let Some(d) = copy.cop_to_displace(Some(cop), dist2) else { return false };
            copy.remove_pursuit_cop(d);
        }
        true
    }

    /// `SetPursuitCop`.
    pub fn set_pursuit_cop(&mut self, cop: EntityId, dist2: &dyn Fn(EntityId) -> Option<f32>) -> bool {
        if self.is_in_pursuit(cop) {
            return true;
        }
        if !self.can_cop_join_pursuit(cop, dist2) {
            return false;
        }
        while self.cops_in_pursuit >= self.max_cops_in_pursuit {
            let Some(d) = self.cop_to_displace(Some(cop), dist2) else { return false };
            self.remove_pursuit_cop(d);
        }
        if let Some(s) = self.pursuit_cops.iter_mut().find(|s| s.is_none()) {
            *s = Some(cop);
            self.cops_in_pursuit += 1;
            return true;
        }
        false
    }

    /// `RemoveExcessPursuitCops`.
    pub fn remove_excess_pursuit_cops(&mut self, dist2: &dyn Fn(EntityId) -> Option<f32>) {
        while self.cops_in_pursuit > self.max_cops_in_pursuit {
            let Some(d) = self.cop_to_displace(None, dist2).or_else(|| self.pursuit_cops.iter().flatten().next().copied()) else { break };
            self.remove_pursuit_cop(d);
        }
    }
}

impl World {
    /// `WorkOutPolicePresence` (0x5625F0): living cops and police vehicles (not the player's,
    /// not abandoned / wrecked) within `radius`.
    pub fn police_presence(&self, pos: Vec3, radius: f32) -> u32 {
        use crate::physical::{EntityType, Status};
        let player_veh = self.player_vehicle();
        let mut n = 0;
        for id in self.body_ids() {
            let Some(b) = self.body(id) else { continue };
            if (b.phys.matrix.pos - pos).length() >= radius {
                continue;
            }
            match b.phys.kind {
                EntityType::Ped => {
                    let Some(p) = b.logic.as_any().downcast_ref::<crate::ped::PedLogic>() else { continue };
                    if p.npc.as_ref().is_some_and(|n| n.ped_type == 6) && p.tasks.health.alive() {
                        n += 1;
                    }
                }
                EntityType::Vehicle => {
                    if Some(id) != player_veh
                        && !matches!(b.phys.status, Status::Abandoned | Status::Wrecked)
                        && self.vehicle_model(id).is_some_and(is_law_enforcement_model)
                    {
                        n += 1;
                    }
                }
                _ => {}
            }
        }
        n
    }

    /// The vehicle the player sits in.
    pub fn player_vehicle(&self) -> Option<EntityId> {
        let p = self.player_id()?;
        self.body(p)?.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?.vehicle.as_ref().map(|v| v.veh)
    }

    /// The vehicle's model id.
    pub fn vehicle_model(&self, id: EntityId) -> Option<u16> {
        let any = self.body(id)?.logic.as_any();
        any.downcast_ref::<crate::automobile::Automobile>()
            .map(|c| c.model)
            .or_else(|| any.downcast_ref::<crate::bike::Bike>().map(|c| c.model))
            .or_else(|| any.downcast_ref::<crate::boat::Boat>().map(|c| c.model))
    }

    /// `CCrime::ReportCrime(type, victim, criminal)` (0x532010): only the player commits crimes.
    pub fn report_crime(&mut self, ty: u8, victim: Option<EntityId>, criminal: Option<EntityId>) {
        let Some(player) = self.player_id() else { return };
        if criminal != Some(player) || ty == 0 {
            return;
        }
        let now = self.now_ms;
        // IsGangOrCriminal victims: the police don't really care.
        let dont_care = victim
            .and_then(|v| self.body(v))
            .and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>())
            .and_then(|p| p.npc.as_ref())
            .is_some_and(|n| matches!(n.ped_type, 7..=17 | 20));
        let Some(pos) = self.body(player).map(|b| b.phys.matrix.pos) else { return };
        if self.wanted.multiplier >= 0.0 {
            let r = immediate_detection_range(ty);
            if self.police_presence(pos, r) != 0 {
                self.wanted.register_crime_immediately(ty, pos, victim, dont_care, now);
                self.wanted.set_wanted_level_no_drop(1, now);
            } else {
                self.wanted.add_crime_to_q(ty, victim, pos, false, dont_care, now);
            }
        }
        match ty {
            3 => self.wanted.set_wanted_level_no_drop(1, now),
            5 | 19 => self.wanted.set_wanted_level_no_drop(2, now),
            _ => {}
        }
    }

    /// The player's `CWanted::Update` (from CPlayerPed::ProcessControl).
    pub(crate) fn update_wanted(&mut self) {
        let Some(player) = self.player_id() else { return };
        let Some(pos) = self.body(player).map(|b| b.phys.matrix.pos) else { return };
        let now = self.now_ms;
        let open = matches!(self.weather.region, 0 | 4);
        // In a law-enforcement vehicle (no aircraft in the port yet).
        let exempt = self.player_vehicle().is_some_and(|v| self.vehicle_model(v).is_some_and(is_law_enforcement_model));
        let near = self.police_presence(pos, 18.0) != 0;
        self.wanted.update(now, open, exempt, near);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chaos_levels_and_set_level() {
        let mut w = Wanted::default();
        for (c, l) in [(49, 0), (50, 1), (179, 1), (180, 2), (550, 3), (1200, 4), (2400, 5), (4600, 6), (99999, 6)] {
            w.chaos = c;
            w.update_wanted_level(0);
            assert_eq!(w.level, l, "chaos {c}");
        }
        w.set_wanted_level(2, 0);
        assert_eq!((w.chaos, w.level, w.max_cops_in_pursuit), (200, 2, 3));
    }

    #[test]
    fn queued_crime_reports_after_500ms_once_per_victim() {
        let mut w = Wanted::default();
        let v = Some(EntityId::Body(3));
        w.add_crime_to_q(4, v, Vec3::ZERO, false, false, 1000);
        w.update_crimes_q(1400);
        assert_eq!(w.chaos, 0);
        w.update_crimes_q(1501);
        assert_eq!(w.chaos, 30);
        // Same crime on the same victim within 10 s: no new chaos.
        assert!(w.add_crime_to_q(4, v, Vec3::ZERO, false, false, 2000));
        w.update_crimes_q(3000);
        assert_eq!(w.chaos, 30);
    }

    #[test]
    fn one_star_decays_in_cities() {
        let mut w = Wanted::default();
        w.set_wanted_level(1, 0);
        let mut t = 0;
        while w.level > 0 && t < 60_000 {
            t += 20;
            w.update(t, false, false, false);
        }
        // 70 → 49 chaos at 1 per second.
        assert!((21_000..=22_500).contains(&t), "t = {t}");
        // Two stars in a city never decay.
        w.set_wanted_level(2, t);
        for _ in 0..1000 {
            t += 20;
            w.update(t, false, false, false);
        }
        assert_eq!(w.level, 2);
    }
}

impl World {
    /// CTaskComplexWanderCop (0x674D80) / CTaskComplexPolicePursuit / PersistPursuit for the
    /// cops on foot, and the arrests their CTaskSimpleArrestPed made.
    pub(crate) fn update_police(&mut self, _views: &[impl Sized]) {
        let Some(player) = self.player_id() else { return };
        let Some(pp) = self.body(player).map(|b| b.phys.matrix.pos) else { return };
        let now = self.now_ms;
        let level = self.wanted.level;
        let cops: Vec<(EntityId, bool)> = self
            .body_ids()
            .into_iter()
            .filter_map(|id| {
                let p = self.body(id)?.logic.as_any().downcast_ref::<crate::ped::PedLogic>()?;
                let n = p.npc.as_ref()?;
                (n.ped_type == 6).then_some((id, p.tasks.health.alive()))
            })
            .collect();
        // Squared distances from the player (None = dying / gone).
        let dist: std::collections::HashMap<EntityId, f32> = cops
            .iter()
            .filter(|c| c.1)
            .filter_map(|&(id, _)| self.body(id).map(|b| (id, (b.phys.matrix.pos - pp).length_squared())))
            .collect();
        let d2 = |c: EntityId| dist.get(&c).copied();
        // PersistPursuit for every cop already chasing; dead ones leave the list.
        for &(id, alive) in &cops {
            if !alive {
                self.wanted.remove_pursuit_cop(id);
            }
        }
        self.wanted.remove_excess_pursuit_cops(&d2);
        let player_in_veh_fast = self
            .body(player)
            .and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>())
            .and_then(|p| p.vehicle.as_ref().map(|v| v.veh))
            .and_then(|v| self.body(v))
            .is_some_and(|b| b.phys.move_speed.length_squared() * (b.phys.matrix.pos - pp).length_squared() >= 16.0);
        // The player holds an instant-hit or projectile weapon (SetWeapon's 1-star test).
        let player_gun = self
            .body(player)
            .and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>())
            .and_then(|p| p.tasks.infos.as_deref().map(|i| i.get(p.tasks.active_weapon().ty, 1).fire_type))
            .is_some_and(|ft| matches!(ft, crate::weapon::fire::INSTANT_HIT | crate::weapon::fire::PROJECTILE));
        for &(id, alive) in &cops {
            let (has, responding, rejoin) = {
                let Some(n) = self.body(id).and_then(|b| b.logic.as_any().downcast_ref::<crate::ped::PedLogic>()).and_then(|p| p.npc.as_ref())
                else {
                    continue;
                };
                (n.pursuit.is_some(), n.response.is_some(), n.rejoin_after)
            };
            if has {
                let keep = alive && self.wanted.is_in_pursuit(id);
                if !keep {
                    if let Some(n) = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>()).and_then(|p| p.npc.as_mut()) {
                        n.pursuit = None;
                        n.rejoin_after = now + 3000;
                        n.last_move_state = 0;
                    }
                } else if let Some(p) = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>()) {
                    // ArrestPed ControlSubTask: SetWeapon every frame (not while arresting).
                    if p.souls_enemy.is_none() && !p.npc.as_ref().and_then(|n| n.pursuit.as_ref()).is_some_and(|pu| pu.arresting()) {
                        set_weapon(&mut p.tasks, level, player_gun);
                    }
                }
            } else if alive && level > 0 && !responding && now >= rejoin && !player_in_veh_fast {
                // ShouldPursuePlayer → SetPursuit (the player on foot or slow).
                if self.wanted.set_pursuit_cop(id, &d2) {
                    let w = level;
                    if let Some(p) = self.body_mut(id).and_then(|b| b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>()) {
                        set_weapon(&mut p.tasks, w, player_gun);
                        if let Some(n) = p.npc.as_mut() {
                            n.pursuit = Some(crate::pedevents::Pursuit::new(player));
                            n.resp_in.threat_pos = Some(pp);
                            n.resp_in.threat_alive = true;
                            if let Some(wd) = n.wander.as_mut() {
                                wd.sub = None;
                            }
                        }
                    }
                }
            }
        }
        // CTaskSimpleArrestPed::StartAnim: the target becomes ARRESTED and stays down.
        let arrests: Vec<EntityId> = cops
            .iter()
            .filter_map(|&(id, _)| {
                self.body_mut(id)
                    .and_then(|b| b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>())
                    .and_then(|p| p.npc.as_mut())
                    .and_then(|n| n.arrest_request.take())
            })
            .collect();
        for t in arrests {
            if let Some(p) = self.body_mut(t).and_then(|b| b.logic.as_any_mut().downcast_mut::<crate::ped::PedLogic>()) {
                if p.tasks.health.alive() && !p.tasks.arrested {
                    p.tasks.arrested = true;
                    if let Some(crate::peddamage::FallAndGetUp::Fall { down_ms, .. }) = p.tasks.health.fall.as_mut() {
                        *down_ms = 100_000;
                    }
                }
            }
        }
    }
}

/// `PolicePursuit::SetWeapon(cop)` (0x68BAD0): at 2+ stars an unarmed cop draws its gun; at
/// 1 star the nightstick unless the player holds a gun.
fn set_weapon(t: &mut crate::pedtask::PedTasks, level: i32, player_gun: bool) {
    if !(level >= 2 && t.active_weapon().ty == 0) {
        if level != 1 {
            return;
        }
        if !player_gun {
            if let Some(slot) = (0..t.weapons.len()).find(|&s| t.weapons[s].ty == 3) {
                if t.active_slot != slot {
                    t.set_current_weapon(slot);
                }
                return;
            }
        }
    }
    let gun = [25, 22].into_iter().find_map(|ty| (0..t.weapons.len()).find(|&s| t.weapons[s].ty == ty));
    if let Some(slot) = gun {
        if t.active_slot != slot {
            t.set_current_weapon(slot);
        }
    }
}
