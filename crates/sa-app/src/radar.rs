//! `CRadar` blips (hud.md §7.1, §7.6-7.8): the 175 `tRadarTrace` slots the scripts fill
//! (coord / contact-point / entity blips), their colours (GetRadarTraceColour with the
//! GetIntColour quirk), and DrawBlips' sprite and height-marker passes.

use bevy::prelude::*;
use sa_physics::world::EntityId;

/// Blip types (trace +0x26 bits 2-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlipType {
    Car = 1,
    Char = 2,
    Object = 3,
    Coord = 4,
    Contact = 5,
    Pickup = 7,
}

/// One `tRadarTrace` (0x28 bytes).
#[derive(Clone, Debug)]
pub struct Trace {
    pub colour: u32,
    pub entity: Option<EntityId>,
    pub pos: Vec3,
    pub counter: u16,
    pub size: u16,
    pub sprite: u8,
    pub bright: bool,
    pub short_range: bool,
    pub friendly: bool,
    pub fade: bool,
    /// Coord blip appearance (flags bits 6-7): 0 trace colour, 1 blue, 2 red.
    pub appearance: u8,
    /// 1 marker only, 2 blip only, 3 both.
    pub display: u8,
    pub ty: BlipType,
}

/// `CRadar::ms_RadarTrace` (175 slots, handle = counter << 16 | index).
#[derive(Resource, Default)]
pub struct Radar {
    pub traces: Vec<Option<Trace>>,
    counters: Vec<u16>,
}

const MAX_BLIPS: usize = 175;

impl Radar {
    fn alloc(&mut self, t: Trace) -> i32 {
        if self.traces.len() < MAX_BLIPS {
            self.traces.resize(MAX_BLIPS, None);
            self.counters.resize(MAX_BLIPS, 0);
        }
        let Some(i) = self.traces.iter().position(|t| t.is_none()) else { return -1 };
        self.counters[i] = self.counters[i].wrapping_add(1);
        let counter = self.counters[i];
        self.traces[i] = Some(Trace { counter, ..t });
        ((counter as i32) << 16) | i as i32
    }

    /// `GetActualBlipArrayIndex`: the slot of a live handle.
    fn index(&self, handle: i32) -> Option<usize> {
        if handle < 0 {
            return None;
        }
        let i = (handle & 0xFFFF) as usize;
        let t = self.traces.get(i)?.as_ref()?;
        (t.counter as i32 == (handle >> 16) & 0xFFFF).then_some(i)
    }

    pub fn get_mut(&mut self, handle: i32) -> Option<&mut Trace> {
        let i = self.index(handle)?;
        self.traces[i].as_mut()
    }

    /// `SetCoordBlip(type, pos, colour, display)` (0x583820): radius 1, size 1, sprite 0, bright.
    pub fn set_coord_blip(&mut self, ty: BlipType, pos: Vec3, colour: u32, display: u8) -> i32 {
        self.alloc(Trace {
            colour,
            entity: None,
            pos,
            counter: 0,
            size: 1,
            sprite: 0,
            bright: true,
            short_range: false,
            friendly: false,
            fade: false,
            appearance: 0,
            display,
            ty,
        })
    }

    /// `SetEntityBlip(type, entity, colour, display)` (0x5839A0).
    pub fn set_entity_blip(&mut self, ty: BlipType, entity: EntityId, colour: u32, display: u8) -> i32 {
        self.alloc(Trace {
            colour,
            entity: Some(entity),
            pos: Vec3::ZERO,
            counter: 0,
            size: 1,
            sprite: 0,
            bright: true,
            short_range: false,
            friendly: false,
            fade: false,
            appearance: 0,
            display,
            ty,
        })
    }

    /// `ClearBlip` (0x587CE0).
    pub fn clear_blip(&mut self, handle: i32) {
        if let Some(i) = self.index(handle) {
            self.traces[i] = None;
        }
    }
}

/// `CHudColours::GetIntColour` with its add-instead-of-or quirk: (r, g, b + 2, 253).
fn int_colour(idx: usize) -> [u8; 4] {
    let c = crate::hud::HUD_COLOURS.get(idx).copied().unwrap_or([255, 255, 255]);
    [c[0], c[1], c[2].saturating_add(2), 253]
}

/// `GetRadarTraceColour(colour, bright, friendly)` (0x584770).
pub fn trace_colour(colour: u32, bright: bool, friendly: bool) -> [u8; 4] {
    let idx = match (colour, bright) {
        (0 | 5, false) => 9,
        (0 | 5, true) => 0,
        (1, false) => 10,
        (1, true) => 1,
        (2 | 6, false) => 13,
        (2 | 6, true) => 3,
        (3, false) => 8,
        (3, true) => 4,
        (4, false) => 6,
        (4, true) => 11,
        (7, _) => {
            if friendly {
                13
            } else {
                0
            }
        }
        (8, _) => 11,
        _ => return colour.to_be_bytes(),
    };
    int_colour(idx)
}

/// `DisplayThisBlip(sprite, priority)` (0x583B40) outdoors, with the map-legend flags on: the
/// priority pass a sprite is drawn in.
pub fn sprite_priority(sprite: u8) -> u8 {
    match sprite {
        0..=4 => 0,
        5 | 6 | 7 | 9 | 10 | 11 | 14 | 17 | 22 | 27 | 29 | 30 | 33 | 35 | 36 | 39 | 45 | 48..=55 | 63 => 1,
        8 | 12 | 13 | 15 | 16 | 18 | 21 | 23..=26 | 28 | 34 | 37 | 38 | 40 | 42..=44 | 46 | 47 | 58..=62 => 3,
        _ => 2,
    }
}

/// The blip sprite names in hud.txd (table 0x8D0720).
pub const SPRITE_NAMES: [&str; 64] = [
    "", "", "radar_centre", "arrow", "radar_north", "radar_airyard", "radar_ammugun", "radar_barbers", "radar_bigsmoke",
    "radar_boatyard", "radar_burgershot", "radar_bulldozer", "radar_catalinapink", "radar_cesarviapando", "radar_chicken",
    "radar_cj", "radar_crash1", "radar_diner", "radar_emmetgun", "radar_enemyattack", "radar_fire", "radar_girlfriend",
    "radar_hostpital", "radar_locosyndicate", "radar_maddog", "radar_mafiacasino", "radar_mcstrap", "radar_modgarage",
    "radar_ogloc", "radar_pizza", "radar_police", "radar_propertyg", "radar_propertyr", "radar_race", "radar_ryder",
    "radar_savegame", "radar_school", "radar_qmark", "radar_sweet", "radar_tattoo", "radar_thetruth", "radar_waypoint",
    "radar_torenoranch", "radar_triads", "radar_triadscasino", "radar_tshirt", "radar_woozie", "radar_zero",
    "radar_datedisco", "radar_datedrink", "radar_datefood", "radar_truck", "radar_cash", "radar_flag", "radar_gym",
    "radar_impound", "radar_light", "radar_runway", "radar_gangb", "radar_gangp", "radar_gangy", "radar_gangn",
    "radar_gangg", "radar_spray",
];
