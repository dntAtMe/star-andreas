//! `CPickups` / `CPickup` (pickups.md): 620 slots with 1/8 m positions, GenerateNewOne, the
//! per-frame streaming (2D camera distance < 100) and collection passes, the on-foot pickup
//! test, goodies (health, armour, adrenaline, bribe), weapons (DoesPlayerWantNewWeapon, slot
//! sharing, the collect button for swapping), money, respawn and timeouts.

use glam::Vec3;

use crate::{ped::PedLogic, world::World};

pub const MAX_PICKUPS: usize = 620;

/// `ePickupType`.
pub mod ty {
    pub const NONE: u8 = 0;
    pub const IN_SHOP: u8 = 1;
    pub const ON_STREET: u8 = 2;
    pub const ONCE: u8 = 3;
    pub const ONCE_TIMEOUT: u8 = 4;
    pub const ONCE_TIMEOUT_SLOW: u8 = 5;
    pub const COLLECTABLE1: u8 = 6;
    pub const IN_SHOP_OUT_OF_STOCK: u8 = 7;
    pub const MONEY: u8 = 8;
    pub const ON_STREET_SLOW: u8 = 15;
    pub const ASSET_REVENUE: u8 = 16;
    pub const PROPERTY_LOCKED: u8 = 17;
    pub const PROPERTY_FORSALE: u8 = 18;
    pub const MONEY_DOESNTDISAPPEAR: u8 = 19;
    pub const SNAPSHOT: u8 = 20;
    pub const ONCE_FOR_MISSION: u8 = 22;
}

/// The model ids from the PC IDEs (model-index globals 0x8CD59C..).
pub mod mi {
    pub const MONEY: u16 = 1212;
    pub const INFO: u16 = 1239;
    pub const HEALTH: u16 = 1240;
    pub const ADRENALINE: u16 = 1241;
    pub const BODYARMOUR: u16 = 1242;
    pub const BRIBE: u16 = 1247;
    pub const BONUS: u16 = 1248;
    pub const CAMERAPICKUP: u16 = 1253;
    pub const KILLFRENZY: u16 = 1254;
    pub const PROPERTY_LOCKED: u16 = 1272;
    pub const PROPERTY_FSALE: u16 = 1273;
    pub const BIGDOLLAR: u16 = 1274;
    pub const CLOTHESP: u16 = 1275;
    pub const PICKUPSAVE: u16 = 1277;
    pub const JETPACK: u16 = 370;
}

/// `AmmoForWeapon_OnStreet` (0x8A5F50).
pub const AMMO_ON_STREET: [u32; 47] = [
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 8, 8, 8, 8, 4, 4, 30, 10, 10, 15, 10, 10, 60, 60, 80, 80, 60, 20, 10, 4, 3,
    100, 500, 5, 1, 500, 500, 36, 0, 0, 1,
];

/// One `CPickup` (0x20 bytes).
#[derive(Clone, Copy, Debug, Default)]
pub struct Pickup {
    pub revenue: f32,
    pub ammo: u32,
    pub regen: u32,
    /// Position × 8 (i16).
    pub pos: [i16; 3],
    pub money_per_day: u16,
    pub model: u16,
    pub ref_index: u16,
    pub ty: u8,
    pub disabled: bool,
    pub empty: bool,
    pub help_shown: bool,
    /// bVisible: within 100 m (2D) of the camera; the object exists.
    pub visible: bool,
    /// Property text index (flags bits 4..6).
    pub text_idx: u8,
}

impl Pickup {
    pub fn position(&self) -> Vec3 {
        Vec3::new(self.pos[0] as f32, self.pos[1] as f32, self.pos[2] as f32) * 0.125
    }
}

/// The `CPickups` statics.
pub struct Pickups {
    pub slots: Vec<Pickup>,
    collected: [i32; 20],
    collected_index: usize,
    collect_button_frames: i32,
    pub help_messages_left: u8,
    /// Help text requests from the collect logic (GXT key, quick) for the HUD.
    pub help_requests: Vec<(Option<String>, bool)>,
}

impl Default for Pickups {
    fn default() -> Self {
        Self {
            slots: vec![Pickup { ref_index: 1, ..Default::default() }; MAX_PICKUPS],
            collected: [0; 20],
            collected_index: 0,
            collect_button_frames: 0,
            help_messages_left: 10,
            help_requests: Vec::new(),
        }
    }
}

impl World {
    /// `CPickups::WeaponForModel` (0x454AE7): 48 armour/adrenaline, 47 health/jetpack, the
    /// weapon whose model1 is the model, else 0.
    pub fn weapon_for_model(&self, m: u16) -> u32 {
        match m {
            mi::BODYARMOUR | mi::ADRENALINE => 48,
            mi::HEALTH | mi::JETPACK => 47,
            _ => self.weapon_infos.as_ref().and_then(|w| (1..47).find(|&t| w.get(t, 1).model1 == m as i32)).unwrap_or(0),
        }
    }

    /// `CPickups::GenerateNewOne` (0x456F20): handle = refIndex << 16 | slot, or -1.
    pub fn generate_pickup(&mut self, pos: Vec3, model: u16, ty_: u8, ammo: u32, money_per_day: u16, empty: bool, text_idx: u8) -> i32 {
        let now = self.now_ms;
        let p = &mut self.pickups;
        let mut slot = None;
        if ty_ == 13 || ty_ == 11 || empty {
            slot = (0..MAX_PICKUPS).rev().find(|&i| p.slots[i].ty == ty::NONE);
        }
        if slot.is_none() {
            slot = (0..MAX_PICKUPS).find(|&i| p.slots[i].ty == ty::NONE);
        }
        if slot.is_none() {
            slot = (0..MAX_PICKUPS)
                .find(|&i| p.slots[i].ty == ty::MONEY)
                .or_else(|| (0..MAX_PICKUPS).find(|&i| matches!(p.slots[i].ty, ty::ONCE_TIMEOUT | ty::ONCE_TIMEOUT_SLOW)));
        }
        let Some(i) = slot else { return -1 };
        let s = &mut p.slots[i];
        s.ammo = ammo;
        s.money_per_day = money_per_day;
        s.ty = ty_;
        s.revenue = 0.0;
        s.regen = now;
        s.disabled = false;
        s.help_shown = false;
        s.empty = empty;
        match ty_ {
            4 => s.regen = now + 20000,
            5 => s.regen = now + 120000,
            8 => s.regen = now + 30000,
            9 | 10 => {
                s.ty = 9;
                s.regen = now + 1500;
            }
            11 | 12 => {
                s.ty = 11;
                s.regen = now + 1500;
            }
            _ => {}
        }
        s.model = model;
        s.text_idx = text_idx;
        s.pos = [(pos.x * 8.0) as i16, (pos.y * 8.0) as i16, (pos.z * 8.0) as i16];
        s.visible = false;
        s.ref_index = if s.ref_index < 0xFFFE { s.ref_index + 1 } else { 1 };
        ((s.ref_index as i32) << 16) | i as i32
    }

    fn pickup_index(&self, h: i32) -> Option<usize> {
        if h == -1 {
            return None;
        }
        let i = (h & 0xFFFF) as usize;
        (self.pickups.slots.get(i)?.ref_index as i32 == (h >> 16) & 0xFFFF).then_some(i)
    }

    /// `CPickups::RemovePickUp`.
    pub fn remove_pickup(&mut self, h: i32) {
        if let Some(i) = self.pickup_index(h) {
            let s = &mut self.pickups.slots[i];
            s.ty = ty::NONE;
            s.disabled = true;
            s.visible = false;
        }
    }

    /// `CPickups::IsPickUpPickedUp`: true once, for a handle in the collected ring.
    pub fn is_pickup_picked_up(&mut self, h: i32) -> bool {
        match self.pickups.collected.iter().position(|&c| c == h) {
            Some(k) => {
                self.pickups.collected[k] = 0;
                true
            }
            None => false,
        }
    }

    /// `CPickups::Update` (0x458DE0): 1/32 of the slots' visibility per frame (2D camera
    /// distance < 100), then 1/6 of them tested for collection by the player.
    pub(crate) fn update_pickups(&mut self, collect_just_down: bool) {
        let frame = self.frame as usize;
        let cam = self.camera_pos;
        let s = ((frame & 31) * MAX_PICKUPS) >> 5;
        let e = ((frame & 31) * MAX_PICKUPS + MAX_PICKUPS) >> 5;
        for p in &mut self.pickups.slots[s..e] {
            if p.ty != ty::NONE {
                p.visible = (p.position().truncate() - cam.truncate()).length() < 100.0 && !p.disabled;
            }
        }
        let pk = &mut self.pickups;
        pk.collect_button_frames = if collect_just_down { 6 } else { (pk.collect_button_frames - 1).max(0) };
        let Some(pid) = self.player_id() else { return };
        let busy = self.body(pid).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).is_none_or(|l| l.enter.is_some());
        if busy {
            return;
        }
        let s = (frame % 6) * MAX_PICKUPS / 6;
        let e = ((frame % 6) * MAX_PICKUPS + MAX_PICKUPS) / 6;
        for i in s..e {
            let p = self.pickups.slots[i];
            if p.ty == ty::NONE || (!p.visible && !p.disabled) {
                continue;
            }
            if self.update_pickup(i, pid) {
                let h = ((self.pickups.slots[i].ref_index as i32) << 16) | i as i32;
                let k = self.pickups.collected_index;
                self.pickups.collected[k] = h;
                self.pickups.collected_index = (k + 1) % 20;
            }
        }
    }

    /// `CPickup::Update` (0x457410): respawn, timeout, the collection test and the effects.
    fn update_pickup(&mut self, i: usize, pid: crate::world::EntityId) -> bool {
        let now = self.now_ms;
        let player_pos = self.body(pid).map_or(Vec3::ZERO, |b| b.phys.matrix.pos);
        let mut p = self.pickups.slots[i];
        let pos = p.position();
        if p.ty == ty::ASSET_REVENUE {
            let dt = now.wrapping_sub(p.regen);
            p.regen = now;
            if (player_pos - pos).length() > 10.0 {
                p.revenue += (p.money_per_day as u32).wrapping_mul(dt) as f32 * 6.944_444_5e-7;
            }
            p.revenue = p.revenue.min(p.ammo as i32 as f32);
        }
        if p.disabled {
            // Respawn once the player is > 10 m (2D) away (IN_SHOP: √2.4).
            if now > p.regen {
                let d2 = (player_pos.truncate() - pos.truncate()).length_squared();
                if !(d2 <= 100.0 && (p.ty != ty::IN_SHOP || d2 <= 2.4)) {
                    p.disabled = false;
                }
            }
            self.pickups.slots[i] = p;
            return false;
        }
        let mut collected = false;
        // The on-foot test: |dz| < 2, dx² + dy² < 1.8; only bribes from a vehicle.
        let (in_veh, alive) = self
            .body(pid)
            .and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>())
            .map_or((false, false), |l| (l.vehicle.is_some(), l.tasks.health.health > 0.0));
        let d = player_pos - pos;
        let on_foot = d.z.abs() < 2.0 && d.x * d.x + d.y * d.y < 1.8;
        let can = match p.model {
            mi::BRIBE => {
                if in_veh {
                    d.length() < 2.0 + 1.5
                } else {
                    on_foot
                }
            }
            mi::CAMERAPICKUP => false,
            _ => !in_veh && alive && on_foot,
        };
        'collect: {
            if !can {
                break 'collect;
            }
            let w = self.weapon_for_model(p.model);
            let infos = self.weapon_infos.clone();
            let collect_frames = self.pickups.collect_button_frames;
            // Weapon slot rules (§3.5).
            if !matches!(p.ty, 6 | 8..=11 | 13 | 14 | 16..=19) && (1..47).contains(&w) {
                let Some(infos) = infos.as_ref() else { break 'collect };
                let slot = infos.get(w, 1).slot;
                let cur = self.body(pid).and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>()).and_then(|l| l.tasks.weapons.get(slot as usize).map(|w| (w.ty, w.total_ammo)));
                if let Some((cur_ty, cur_ammo)) = cur.filter(|&(t, _)| t != w) {
                    let sharing = matches!(slot, 3..=5);
                    if !(cur_ty == 0 || (cur_ammo == 0 && !sharing)) {
                        if sharing && matches!(p.ty, 2 | 3 | 4 | 22) {
                            // ExtractAmmoFromPickup: the slot's weapon gets the ammo.
                            let a = if p.ammo != 0 { p.ammo } else if p.empty { 0 } else { AMMO_ON_STREET[w as usize] };
                            if a > 0 {
                                if let Some(l) = self.body_mut(pid).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                                    l.tasks.give_weapon(cur_ty, a);
                                }
                            }
                            p.ammo = 0;
                            p.empty = true;
                        }
                        if self.pickups.help_messages_left > 0 && !p.help_shown {
                            self.pickups.help_requests.push((Some("PU_CF1".into()), false));
                            self.pickups.help_messages_left -= 1;
                            p.help_shown = true;
                        }
                        if collect_frames == 0 {
                            break 'collect;
                        }
                    }
                }
            }
            // Goodies gates.
            let (health, max_health, armour) = self
                .body(pid)
                .and_then(|b| b.logic.as_any().downcast_ref::<PedLogic>())
                .map_or((0.0, 100.0, 0.0), |l| (l.tasks.health.health, l.tasks.health.max_health, l.tasks.health.armour));
            let max_armour = 100.0;
            if p.model == mi::BODYARMOUR && armour > max_armour - 0.2 {
                break 'collect;
            }
            if p.model == mi::HEALTH && health > max_health - 0.2 {
                break 'collect;
            }
            if p.model == mi::BRIBE && self.wanted.level == 0 {
                break 'collect;
            }
            if p.ty == ty::ASSET_REVENUE && p.revenue < 10.0 {
                break 'collect;
            }
            match p.ty {
                2 | 15 => {
                    if !self.give_pickup_goodies(p.model, pid) {
                        if w != 0 {
                            let a = if p.ammo != 0 { p.ammo } else if p.empty { 0 } else { AMMO_ON_STREET[w as usize] };
                            if let Some(l) = self.body_mut(pid).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                                l.tasks.give_weapon(w, a);
                            }
                        }
                    }
                    p.regen = now + if p.ty == 2 { 30000 } else if p.model == mi::BRIBE { 300000 } else { 360000 };
                    p.disabled = true;
                    p.visible = false;
                    collected = true;
                }
                3 | 4 | 5 | 22 => {
                    if !self.give_pickup_goodies(p.model, pid) && w != 0 {
                        let a = if p.ammo != 0 { p.ammo } else if p.empty { 0 } else { AMMO_ON_STREET[w as usize] };
                        if let Some(l) = self.body_mut(pid).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                            l.tasks.give_weapon(w, a);
                        }
                    }
                    p.ty = ty::NONE;
                    p.disabled = true;
                    p.visible = false;
                    collected = true;
                }
                8 | 19 => {
                    self.money += p.ammo as i32;
                    p.ty = ty::NONE;
                    p.disabled = true;
                    p.visible = false;
                    collected = true;
                }
                16 => {
                    self.money += p.revenue as i32;
                    p.revenue = 0.0;
                }
                17 => {
                    if !p.help_shown {
                        p.help_shown = true;
                        let key = match p.text_idx {
                            1 => "PROP_3",
                            2 => "PROP_4",
                            _ => "FESZ_CA",
                        };
                        self.pickups.help_requests.push((Some(key.into()), false));
                    }
                }
                _ => {}
            }
        }
        // Timeouts of the ONCE_TIMEOUT / MONEY types.
        if !p.disabled && matches!(p.ty, 4 | 5 | 8) && now > p.regen {
            p.ty = ty::NONE;
            p.disabled = true;
            p.visible = false;
        }
        self.pickups.slots[i] = p;
        collected
    }

    /// `GivePlayerGoodiesWithPickUpMI` (0x4564F6): false for models that are not goodies.
    fn give_pickup_goodies(&mut self, model: u16, pid: crate::world::EntityId) -> bool {
        match model {
            mi::BODYARMOUR | mi::HEALTH | mi::ADRENALINE => {
                if let Some(l) = self.body_mut(pid).and_then(|b| b.logic.as_any_mut().downcast_mut::<PedLogic>()) {
                    let h = &mut l.tasks.health;
                    match model {
                        mi::BODYARMOUR => h.armour = 100.0,
                        mi::HEALTH => h.health = h.max_health,
                        _ => {}
                    }
                }
                true
            }
            mi::INFO | mi::BONUS | mi::KILLFRENZY => true,
            mi::BRIBE => {
                let now = self.now_ms;
                let l = (self.wanted.level - 1).max(0);
                self.wanted.set_wanted_level(l, now);
                true
            }
            _ => false,
        }
    }
}
