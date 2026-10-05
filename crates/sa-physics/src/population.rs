//! `CPopulation` / `CPopCycle` for random civilians (population.md §1.5–§2.9): zone population
//! type from info.zon + main.scm, the popcycle percentages, the 8 streamed-in zone models
//! (StreamZoneModels), AddToPopulation with GeneratePedCreationCoors, and ManagePed removal.
//!
//! Not ported: cops, gangs, dealers, couples, beach sunbathers, skaters' skateable test,
//! attractors, riots, interiors, the in-vehicle creation distance multiplier.

use std::{collections::HashMap, sync::Arc};

use glam::Vec3;
use sa_formats::population::{PedDef, PedStat, PopCycle, ScmZoneSettings, Zone, ped_group_of, ped_type_index};

use crate::paths::PathFind;

/// A ped model (`CPedModelInfo` fields the population uses).
#[derive(Debug, Clone)]
pub struct PedInfo {
    pub id: u32,
    pub model: String,
    pub ped_type: u8,
    pub stat: usize,
    pub anim_group: usize,
    pub race: u8,
    pub cars_mask: u32,
}

/// `CZoneInfo`: popType, race mask, gang densities.
#[derive(Debug, Clone, Copy)]
pub struct ZoneInfo {
    pub pop_type: u8,
    pub race_mask: u8,
    pub gangs: [u8; 10],
}

impl Default for ZoneInfo {
    fn default() -> Self {
        Self { pop_type: 5, race_mask: 0xF, gangs: [0; 10] }
    }
}

pub struct PopData {
    pub peds: HashMap<u32, PedInfo>,
    pub stats: Vec<PedStat>,
    pub popcycle: PopCycle,
    /// `ms_pPedGroups`: model ids per group.
    pub groups: Vec<Vec<u32>>,
    pub zones: Vec<Zone>,
    /// Zone → info index (`AssignZoneInfoForThisZone`).
    zone_info_of: Vec<usize>,
    pub infos: Vec<ZoneInfo>,
    pub paths: Arc<PathFind>,
}

impl PopData {
    pub fn load(
        peds: &[PedDef],
        stats: Vec<PedStat>,
        popcycle: PopCycle,
        groups: &[Vec<String>],
        zones: Vec<Zone>,
        scm: &ScmZoneSettings,
        paths: Arc<PathFind>,
        group_of: impl Fn(&str) -> Option<usize>,
    ) -> Self {
        let stat_index = |s: &str| stats.iter().position(|x| x.name.eq_ignore_ascii_case(s)).unwrap_or(16);
        let mut by_name = HashMap::new();
        let mut map = HashMap::new();
        for p in peds {
            by_name.insert(p.model.clone(), p.id);
            map.insert(
                p.id,
                PedInfo {
                    id: p.id,
                    model: p.model.clone(),
                    ped_type: ped_type_index(&p.ped_type),
                    stat: stat_index(&p.stat_type),
                    anim_group: group_of(&p.anim_group).unwrap_or(crate::anim::group::DEFAULT),
                    race: p.race(),
                    cars_mask: p.cars_mask,
                },
            );
        }
        let groups = groups.iter().map(|g| g.iter().filter_map(|n| by_name.get(n).copied()).collect()).collect();
        // Navigation zones (types 0 / 1); equal labels share one info.
        let zones: Vec<Zone> = zones.into_iter().filter(|z| z.ty <= 1).collect();
        let mut zone_info_of = Vec::new();
        let mut infos: Vec<ZoneInfo> = Vec::new();
        let mut label_info: HashMap<String, usize> = HashMap::new();
        for z in &zones {
            let i = *label_info.entry(z.label.clone()).or_insert_with(|| {
                let mut info = ZoneInfo::default();
                if let Some(&t) = scm.pop_type.get(&z.label) {
                    info.pop_type = t;
                }
                if let Some(&r) = scm.race.get(&z.label) {
                    info.race_mask = r;
                }
                if let Some(g) = scm.gang.get(&z.label) {
                    info.gangs = *g;
                }
                infos.push(info);
                infos.len() - 1
            });
            zone_info_of.push(i);
        }
        Self { peds: map, stats, popcycle, groups, zones, zone_info_of, infos, paths }
    }

    /// `CTheZones::GetZoneInfo` (0x572400): the smallest zone containing `p`.
    pub fn zone_info(&self, p: Vec3) -> ZoneInfo {
        if self.zones.is_empty() {
            return ZoneInfo::default();
        }
        let (x, y, z) = (p.x as i32, p.y as i32, p.z as i32);
        let size = |zn: &Zone| (zn.max[0] as i32 - zn.min[0] as i32) + (zn.max[1] as i32 - zn.min[1] as i32);
        let mut best = 0;
        for (i, zn) in self.zones.iter().enumerate().skip(1) {
            let inside = (zn.min[0] as i32..=zn.max[0] as i32).contains(&x)
                && (zn.min[1] as i32..=zn.max[1] as i32).contains(&y)
                && (zn.min[2] as i32..=zn.max[2] as i32).contains(&z);
            if inside && size(zn) < size(&self.zones[best]) {
                best = i;
            }
        }
        self.infos[self.zone_info_of[best]]
    }
}

/// A ped the population wants created (the app loads the model and adds the body).
#[derive(Debug, Clone)]
pub struct SpawnPed {
    pub model: u32,
    pub ped_type: u8,
    pub pos: Vec3,
    /// Wander direction 0..7.
    pub dir: u8,
}

/// The `CPopulation` statics.
pub struct Population {
    pub data: Arc<PopData>,
    /// `CStreaming::ms_pedsLoaded[8]`.
    pub loaded: Vec<u32>,
    stream_frames: u32,
    last_pop_type: Option<u8>,
    group_counter: [u32; 18],
    /// `m_CountDownToPedsAtStart`.
    pub countdown_at_start: u8,
    /// The current numbers (CPopCycle::m_NumOther_Peds …).
    pub num_other: f32,
    pub num_civ: u32,
    pub requests: Vec<SpawnPed>,
}

/// Inputs from the world for one update.
pub struct PopIn<'a> {
    pub player: Vec3,
    pub hours: u8,
    pub day: u8,
    pub rain: f32,
    pub island: usize,
    pub frame: u32,
    pub rand: &'a mut dyn FnMut() -> u32,
    pub frand: &'a mut dyn FnMut() -> f32,
    /// TheCamera.IsSphereVisible(pos, 2.0).
    pub visible: &'a mut dyn FnMut(Vec3) -> bool,
    /// FindGroundZFor3DCoord(x, y, z + 2).
    pub ground: &'a mut dyn FnMut(Vec3) -> Option<f32>,
    /// IsPositionClearForPed(pos, 0.75).
    pub clear: &'a mut dyn FnMut(Vec3) -> bool,
    /// Model usage counts of the live NPCs.
    pub usage: &'a HashMap<u32, u16>,
}

impl Population {
    pub fn new(data: Arc<PopData>) -> Self {
        Self {
            data,
            loaded: Vec::new(),
            stream_frames: 0,
            last_pop_type: None,
            group_counter: [0; 18],
            countdown_at_start: 2,
            num_other: 0.0,
            num_civ: 0,
            requests: Vec::new(),
        }
    }

    fn idx(i: &PopIn, zone_type: u8) -> usize {
        let day = match i.day {
            0 | 7 => 1,
            1 => (i.hours < 20) as usize,
            6 => (i.hours > 19) as usize,
            _ => 0,
        };
        PopCycle::index((i.hours >> 1) as usize, day, zone_type as usize)
    }

    /// `PickPedMIToStreamInForCurrentZone` (0x60FFD0).
    fn pick_model_to_stream(&mut self, i: &mut PopIn, info: &ZoneInfo) -> Option<u32> {
        let idx = Self::idx(i, info.pop_type);
        let perc = self.data.popcycle.perc_group[idx];
        for _ in 0..10 {
            let mut r = ((i.frand)() * 100.0) as i32;
            let mut g = 0;
            while g < 17 && r >= perc[g] as i32 {
                r -= perc[g] as i32;
                g += 1;
            }
            let pg = ped_group_of(g, i.island);
            let Some(models) = self.data.groups.get(pg) else { continue };
            let n = models.len() as u32;
            for _ in 0..n {
                let s = (self.group_counter[g] + 1) % n;
                self.group_counter[g] = s;
                let m = models[s as usize];
                let race = self.data.peds.get(&m).map_or(0, |p| p.race);
                if !self.loaded.contains(&m) && (race == 0 || info.race_mask & (1 << (race - 1)) != 0) {
                    return Some(m);
                }
            }
        }
        None
    }

    /// `CStreaming::StreamZoneModels` (0x40A560), simulated: the zone's 8 random models.
    fn stream_zone_models(&mut self, i: &mut PopIn, info: &ZoneInfo) {
        if self.last_pop_type != Some(info.pop_type) {
            self.last_pop_type = Some(info.pop_type);
            let n = self.loaded.len().max(4);
            self.loaded.clear();
            for _ in 0..n.min(8) {
                if let Some(m) = self.pick_model_to_stream(i, info) {
                    self.loaded.push(m);
                }
            }
            return;
        }
        self.stream_frames += 1;
        if self.stream_frames < 300 {
            return;
        }
        self.stream_frames = 0;
        let slot = self.loaded.iter().position(|m| i.usage.get(m).copied().unwrap_or(0) == 0);
        if let Some(m) = self.pick_model_to_stream(i, info) {
            match slot {
                Some(s) if self.loaded.len() >= 8 => self.loaded[s] = m,
                _ if self.loaded.len() < 8 => self.loaded.push(m),
                _ => {}
            }
        }
    }

    /// `ChooseCivilianOccupation` (0x612F90): the least used loaded model allowed here.
    fn choose_civilian(&self, i: &PopIn, info: &ZoneInfo) -> Option<u32> {
        for use_count in 0..3u16 {
            for &m in &self.loaded {
                let Some(p) = self.data.peds.get(&m) else { continue };
                let race_ok = p.race == 0 || info.race_mask & (1 << (p.race - 1)) != 0;
                let beach = matches!(p.stat, 38 | 39);
                if i.usage.get(&m).copied().unwrap_or(0) == use_count && race_ok && !(i.rain >= 0.1 && beach) {
                    return Some(m);
                }
            }
        }
        None
    }

    /// `CPopulation::Update` → `AddToPopulation` for civilians. `num_civ` = live random
    /// civilians (CIVMALE + CIVFEMALE).
    pub fn update(&mut self, i: &mut PopIn, num_civ: u32, total_peds: u32) {
        self.num_civ = num_civ;
        let info = self.data.zone_info(i.player);
        // CPopCycle::UpdatePercentages, the 'other' (civilian) share.
        let idx = Self::idx(i, info.pop_type);
        let pc = &self.data.popcycle;
        // The dealer counter (info[10]) grows with UpdateDealerStrengths, not ported: 0.
        let dealers = 0.0f32;
        let mut gang = info.gangs.iter().map(|&g| g as f32).sum::<f32>() * 0.01;
        gang = gang.min(0.5);
        let mut cops = if gang >= 0.15 { (0.3 - gang).max(0.03) } else { gang.max(0.02) };
        match info.pop_type {
            4 | 14 | 16 => cops = cops.max(0.1),
            5 => cops = cops.max(0.05),
            8 | 17 => cops = 0.0,
            _ => {}
        }
        let s = dealers + gang + cops;
        let other = if s <= 1.0 { 1.0 - s } else { 0.0 };
        let f = 1.0 - i.rain.sqrt() * 0.8;
        let perc_other_peds = (pc.perc_other[idx] as f32 * f) as i32 as f32;
        let max_peds = pc.max_peds[idx] as f32;
        self.num_other = perc_other_peds * other * 0.01 * max_peds;
        let num_cops = pc.perc_cops[idx] as f32 * cops * 0.01 * max_peds;
        let num_gang = pc.perc_gang[idx] as f32 * gang * 0.01 * max_peds;
        let num_dealers = pc.perc_dealers[idx] as f32 * dealers * 0.01 * max_peds;
        self.stream_zone_models(i, &info);

        if self.countdown_at_start > 0 {
            self.countdown_at_start -= 1;
            if self.countdown_at_start == 0 {
                // GeneratePedsAtStartOfGame (0x615C90).
                let mut total = total_peds;
                let mut civ = num_civ;
                for _ in 0..100 {
                    if self.add_to_population(i, &info, 10.0, 50.5, 10.0, 50.5, total, civ, num_cops + num_gang + num_dealers) {
                        total += 1;
                        civ += 1;
                    }
                }
            }
            return;
        }
        self.add_to_population(i, &info, 42.5, 50.5, 15.0, 25.0, total_peds, num_civ, num_cops + num_gang + num_dealers);
    }

    /// `AddToPopulation` (0x614720), civilians only.
    #[allow(clippy::too_many_arguments)]
    fn add_to_population(
        &mut self,
        i: &mut PopIn,
        info: &ZoneInfo,
        min_dist: f32,
        max_dist: f32,
        min_off: f32,
        max_off: f32,
        total_peds: u32,
        num_civ: u32,
        others: f32,
    ) -> bool {
        let max_peds = 25f32.min(self.num_other + others);
        if total_peds as f32 >= max_peds {
            return false;
        }
        // FindNewPedType: the 'other' deficit only.
        let mut d_o = self.num_other - num_civ as f32;
        if d_o < 2.0 {
            d_o *= ((i.rand)() & 0x7FFF) as f32 * (1.0 / 32767.0);
        }
        if d_o <= 0.0 {
            return false;
        }
        let Some(model) = self.choose_civilian(i, info) else { return false };
        let Some(p) = self.data.peds.get(&model).cloned() else { return false };
        let paths = self.data.paths.clone();
        let (px, py) = (i.player.x, i.player.y);
        let Some((mut pos, n1, n2)) =
            paths.generate_ped_creation_coors(px, py, min_dist, max_dist, min_off, max_off, false, i.rand, i.frand, i.visible, i.ground)
        else {
            return false;
        };
        let sp = paths.node(n1).map_or(0, |n| n.spawn_prob()).min(paths.node(n2).map_or(0, |n| n.spawn_prob()));
        if ((i.rand)() & 15) as u8 > sp {
            return false;
        }
        let seed = (i.rand)();
        paths.width_offset_coors(n1, n2, seed, &mut pos);
        pos.z += 0.7;
        if !(i.clear)(pos) {
            return false;
        }
        if (i.visible)(pos) && (pos.truncate() - i.player.truncate()).length() < 42.5 {
            return false;
        }
        if matches!(p.stat, 38 | 39) && (i.hours < 8 || i.hours > 19) {
            return false;
        }
        let dir = ((i.frand)() * 8.0) as u8 % 8;
        self.requests.push(SpawnPed { model, ped_type: p.ped_type, pos, dir });
        true
    }
}

/// `ManagePed` (0x611FC0) for a random NPC: what to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manage {
    Keep,
    FadeOut,
    Remove,
}

/// ManagePed's distance / visibility rule. `remove_at` is ped+0x54C (refreshed here).
#[allow(clippy::too_many_arguments)]
pub fn manage_ped(
    dist: f32,
    on_screen: bool,
    is_cop: bool,
    dead_for_ms: Option<u32>,
    fading: bool,
    alpha: u8,
    now: u32,
    remove_at: &mut u32,
) -> Manage {
    if dead_for_ms.is_some_and(|t| t > 30000) && !fading {
        return Manage::FadeOut;
    }
    if fading && alpha == 0 {
        return Manage::Remove;
    }
    let grace = if is_cop { 10000 } else { 4000 };
    if dist <= 54.5 {
        if dist <= 25.0 {
            *remove_at = now + grace;
            return Manage::Keep;
        }
        if *remove_at < now {
            if !on_screen {
                return Manage::Remove;
            }
        } else if !on_screen {
            return Manage::Keep;
        }
        *remove_at = now + grace;
        return Manage::Keep;
    }
    if on_screen { Manage::FadeOut } else { Manage::Remove }
}

impl crate::world::World {
    /// `CPopulation::Update` (0x616650): ManagePopulation for the random NPCs, then
    /// AddToPopulation. Spawns go to `population.requests` for the app (models, clumps).
    pub(crate) fn update_population(&mut self) {
        use crate::{ped::PedLogic, peddamage::Life};
        let Some(mut pop) = self.population.take() else { return };
        let Some(player) = self.player_id().and_then(|id| self.body(id)).map(|b| b.phys.matrix.pos) else {
            self.population = Some(pop);
            return;
        };
        let now = self.now_ms;
        let planes = self.camera_planes;
        let visible = move |c: Vec3, r: f32| planes.iter().all(|(n, d)| n.dot(c) - d <= r);
        // ManagePopulation / ManagePed and the counters (UpdatePedCount).
        let mut usage: HashMap<u32, u16> = HashMap::new();
        let (mut num_civ, mut total) = (0u32, 0u32);
        let mut remove = Vec::new();
        for id in self.body_ids() {
            let Some(b) = self.body_mut(id) else { continue };
            let pos = b.phys.matrix.pos;
            let Some(ped) = b.logic.as_any_mut().downcast_mut::<PedLogic>() else { continue };
            let dead_for = match ped.tasks.health.life {
                Life::Wasted { since_ms } => Some(now.wrapping_sub(since_ms)),
                _ => None,
            };
            let Some(npc) = ped.npc.as_mut() else { continue };
            let d = (pos.truncate() - player.truncate()).length();
            match manage_ped(d, visible(pos, 1.0), npc.ped_type == 6, dead_for, npc.fading_out, npc.alpha, now, &mut npc.remove_at_ms) {
                Manage::Keep => {}
                Manage::FadeOut => npc.fading_out = true,
                Manage::Remove => {
                    remove.push(id);
                    continue;
                }
            }
            total += 1;
            if matches!(npc.ped_type, 4 | 5) {
                num_civ += 1;
            }
            *usage.entry(npc.model).or_insert(0) += 1;
        }
        for id in remove {
            self.remove(id);
            self.npc_removed.push(id);
        }
        // AddToPopulation.
        let mut rng = self.rng.clone();
        let hours = self.clock.hours;
        let day = self.clock.current_day;
        let rain = self.weather.rain;
        let frame = self.frame;
        let w = std::cell::RefCell::new(&mut *self);
        let rng_cell = std::cell::RefCell::new(&mut rng);
        let mut rand = || rng_cell.borrow_mut().next();
        let mut frand = || {
            // frand01 = rand() / 32767.
            (rng_cell.borrow_mut().next() as f32) * 3.051_850_9e-5
        };
        let mut vis = |p: Vec3| visible(p, 2.0);
        let mut ground = |p: Vec3| w.borrow_mut().find_ground_z(p + Vec3::new(0.0, 0.0, 2.0));
        let mut clear = |p: Vec3| w.borrow().is_position_clear_for_ped(p, 0.75);
        let mut i = PopIn {
            player,
            hours,
            day,
            rain,
            island: 0,
            frame,
            rand: &mut rand,
            frand: &mut frand,
            visible: &mut vis,
            ground: &mut ground,
            clear: &mut clear,
            usage: &usage,
        };
        pop.update(&mut i, num_civ, total);
        drop(i);
        self.rng = rng;
        self.population = Some(pop);
    }

    /// `CWorld::IsPositionClearForPed` [S]: no body's bounding sphere within `r`.
    pub fn is_position_clear_for_ped(&self, p: Vec3, r: f32) -> bool {
        use crate::physical::EntityType;
        for id in self.body_ids() {
            let Some(b) = self.body(id) else { continue };
            if !matches!(b.phys.kind, EntityType::Ped | EntityType::Vehicle | EntityType::Object) {
                continue;
            }
            let c = b.phys.matrix.transform(b.col.bound_center);
            if (c - p).length() < r + b.col.bound_radius {
                return false;
            }
        }
        true
    }

    /// `CPopulation::AddPed` (0x612716) for a random civilian: the body with its clump, the
    /// pedstats heading rate and `CTaskComplexWanderStandard`.
    pub fn add_npc(&mut self, req: &SpawnPed, mut clump: crate::anim::Clump, anims: Arc<crate::anim::AnimManager>, seed: u16) -> Option<crate::world::EntityId> {
        use crate::ped::{PedLogic, ped_col_model, ped_physical};
        let pop = self.population.as_ref()?;
        let info = pop.data.peds.get(&req.model)?.clone();
        let rate = pop.data.stats.get(info.stat).map_or(15.0, |s| s.heading_change_rate);
        let paths = pop.data.paths.clone();
        let mut m = crate::physical::Matrix::IDENTITY;
        m.pos = req.pos;
        let phys = ped_physical(m);
        let mut logic = PedLogic::new(false, 0.0);
        logic.turn_rate = rate;
        clump.blend_animation(&anims, info.anim_group, crate::anim::anim_id::IDLE, 1000.0);
        logic.prev_pose = clump.pose.clone();
        logic.clump = Some(Box::new(clump));
        logic.tasks.anims = Some(anims);
        logic.tasks.anim_group = info.anim_group;
        logic.npc = Some(crate::npc::NpcState::new(req.model, req.ped_type, seed, info.anim_group, req.dir, paths, self.now_ms));
        Some(self.add_body(phys, ped_col_model(), Box::new(logic)))
    }
}
