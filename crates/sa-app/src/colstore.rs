//! `CColStore` (streaming_focus.md): building collision streams by COL slot (one per .col
//! archive) around the *player*, not the camera. A slot's rect is the union of its placed
//! models' bounds grown by 120; it is needed while a focus point (the player / his vehicle with
//! a look-ahead, the script's mission cars and peds) is inside, and dropped once none is. The
//! visuals keep streaming around the camera.
//!
//! Also the mission-entity freeze (entity flag 0x40000): cars and peds a mission script creates
//! stay static until `HasCollisionLoaded` holds at their position
//! (`CMissionCleanup::CheckIfCollisionHasLoadedForMissionObjects`), then are placed on the ground.

use std::{collections::HashMap, sync::Arc};

use bevy::prelude::*;
use sa_formats::col;
use sa_physics::{collision::ColModel as SaColModel, physical::ef, ped::PedLogic, world::EntityId};

use crate::{
    player::Ped,
    saphys::{SaPhys, SaPhysExt, gta_matrix},
    world::{WorldRes, b2g},
};

pub struct ColStorePlugin;

impl Plugin for ColStorePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init_col_store)
            .add_systems(Update, update_col_store.before(crate::saphys::SaStep).before(crate::player::player_control));
    }
}

/// One COL slot.
struct Slot {
    /// GTA xy rect of the placed bounds (not grown).
    min: Vec2,
    max: Vec2,
    /// (instance index, collision model name) of its building instances.
    insts: Vec<(usize, String)>,
    loaded: bool,
    buildings: Vec<EntityId>,
}

#[derive(Resource, Default)]
pub struct ColStore {
    slots: Vec<Slot>,
    models: HashMap<String, Option<Arc<SaColModel>>>,
    /// Script entities frozen until their collision is in (flag 0x40000).
    pub waiting: Vec<EntityId>,
    /// Extra focus points this frame (mission cars and peds).
    pub mission_points: Vec<Vec3>,
}

/// The bounding sphere (centre, radius) from a COL model header.
fn col_bounds(data: &[u8]) -> Option<(Vec3, f32)> {
    let f = |o: usize| data.get(o..o + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
    if data.get(0..4)? == b"COLL" {
        Some((Vec3::new(f(36)?, f(40)?, f(44)?), f(32)?))
    } else {
        Some((Vec3::new(f(56)?, f(60)?, f(64)?), f(68)?))
    }
}

fn init_col_store(mut commands: Commands, world: Res<WorldRes>) {
    let w = &world.0;
    let mut slots: Vec<Slot> = w
        .col_slot_names
        .iter()
        .map(|_| Slot { min: Vec2::splat(f32::MAX), max: Vec2::splat(f32::MIN), insts: Vec::new(), loaded: false, buildings: Vec::new() })
        .collect();
    for (i, inst) in w.instances.iter().enumerate() {
        // Full-detail instances only; object.dat models are CObjects that stream with their
        // visuals.
        if inst.near != 0.0 {
            continue;
        }
        let Some(obj) = w.objects.get(&inst.id) else { continue };
        let name = obj.model.to_ascii_lowercase();
        if w.physics.get(&obj.model).is_some_and(|p| !matches!(p.special, 6 | 7)) {
            continue;
        }
        let Some(&slot) = w.col_slot.get(&name) else { continue };
        let Some((c, r)) = w.col(&name).and_then(col_bounds) else { continue };
        let p = Vec3::from(b2g(inst.pos));
        let reach = r + c.length();
        let s = &mut slots[slot];
        s.min = s.min.min(p.truncate() - Vec2::splat(reach));
        s.max = s.max.max(p.truncate() + Vec2::splat(reach));
        s.insts.push((i, name));
    }
    info!("col store: {} slots, {} building instances", slots.len(), slots.iter().map(|s| s.insts.len()).sum::<usize>());
    commands.insert_resource(ColStore { slots, ..default() });
}

impl ColStore {
    /// `CColStore::HasCollisionLoaded(pos)` (0x410CE0): every slot whose rect shrunk by 110
    /// (its bounds + 10) contains the point is loaded.
    pub fn has_collision_loaded(&self, p: Vec3) -> bool {
        self.slots.iter().all(|s| s.loaded || !(p.x >= s.min.x - 10.0 && p.x <= s.max.x + 10.0 && p.y >= s.min.y - 10.0 && p.y <= s.max.y + 10.0))
    }
}

/// `CColStore::LoadCollision` + `EnsureCollisionIsInMemory` around the focus points, and the
/// mission-entity collision freeze.
fn update_col_store(world: Res<WorldRes>, store: Option<ResMut<ColStore>>, mut sa: ResMut<SaPhys>, ped: Single<&Ped>) {
    let Some(mut store) = store else { return };
    let store = &mut *store;
    // FindPlayerCoors: the vehicle when in one; the look-ahead moveSpeed.xy × 20.
    let pid = ped.sa;
    let veh = sa.logic::<PedLogic>(pid).and_then(|l| l.vehicle.as_ref().map(|v| v.veh));
    let mut focus: Vec<Vec3> = Vec::new();
    if let Some(b) = sa.world.body(veh.unwrap_or(pid)) {
        let ahead = if veh.is_some() { (b.phys.move_speed * 20.0).truncate().extend(0.0) } else { Vec3::ZERO };
        focus.push(b.phys.matrix.pos + ahead);
    }
    focus.extend(store.mission_points.drain(..));
    let mut waiting_pts: Vec<Vec3> = store.waiting.iter().filter_map(|&id| sa.world.body(id).map(|b| b.phys.matrix.pos)).collect();
    focus.append(&mut waiting_pts);
    for si in 0..store.slots.len() {
        let s = &store.slots[si];
        if s.insts.is_empty() {
            continue;
        }
        let needed = focus.iter().any(|p| p.x >= s.min.x - 120.0 && p.x <= s.max.x + 120.0 && p.y >= s.min.y - 120.0 && p.y <= s.max.y + 120.0);
        if needed && !s.loaded {
            let insts = std::mem::take(&mut store.slots[si].insts);
            let mut ids = Vec::with_capacity(insts.len());
            for (i, name) in &insts {
                let col = store
                    .models
                    .entry(name.clone())
                    .or_insert_with(|| world.0.col(name).and_then(|d| col::parse_model(d).ok()).map(|m| Arc::new(SaColModel::from_col(&m))))
                    .clone();
                let Some(col) = col else { continue };
                let inst = &world.0.instances[*i];
                let m = gta_matrix(&Transform::from_translation(inst.pos).with_rotation(inst.rot));
                ids.push(sa.world.add_building(m, col));
            }
            let s = &mut store.slots[si];
            s.insts = insts;
            s.buildings = ids;
            s.loaded = true;
        } else if !needed && s.loaded {
            let s = &mut store.slots[si];
            for id in s.buildings.drain(..) {
                sa.world.remove(id);
            }
            s.loaded = false;
        }
    }
    // CheckIfCollisionHasLoadedForMissionObjects: unfreeze and put on the ground.
    let mut still = Vec::new();
    for id in std::mem::take(&mut store.waiting) {
        let Some(pos) = sa.world.body(id).map(|b| b.phys.matrix.pos) else { continue };
        if !store.has_collision_loaded(pos) {
            still.push(id);
            continue;
        }
        let base = sa.world.body(id).map_or(1.0, |b| -b.col.bbox_min.z);
        let ground = sa.world.find_ground_z(pos + Vec3::Z * 2.0);
        if let Some(b) = sa.world.body_mut(id) {
            b.phys.eflags &= !ef::IS_STATIC;
            if let Some(gz) = ground {
                b.phys.matrix.pos.z = gz + base;
            }
            b.phys.move_speed = Vec3::ZERO;
        }
    }
    store.waiting = still;
}
