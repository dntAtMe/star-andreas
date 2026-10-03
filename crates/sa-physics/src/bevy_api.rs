//! Bevy integration (feature `bevy`): the SA physics world as a resource, with
//! a Bevy-space raycast that other crates (e.g. sc2-bevy) can use.
//!
//! Bevy is Y-up, the SA world is GTA Z-up: (x, y, z)_gta = (x, -z, y)_bevy.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::world::{EntityId, World};

pub fn g2b(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y)
}

pub fn b2g(v: Vec3) -> Vec3 {
    Vec3::new(v.x, -v.z, v.y)
}

#[derive(Debug, Clone, Copy)]
pub struct RayHit {
    /// The Bevy entity owning the hit SA entity, if it is registered.
    pub entity: Option<Entity>,
    pub sa_id: EntityId,
    /// Distance from the origin along `dir`.
    pub toi: f32,
    /// Hit point and surface normal, Bevy space.
    pub point: Vec3,
    pub normal: Vec3,
    /// SA surface type of the hit surface.
    pub surface: u8,
}

/// The SA physics world plus the SA-id <-> Bevy-entity mapping.
#[derive(Resource)]
pub struct SaPhysics {
    pub world: World,
    pub entities: HashMap<EntityId, Entity>,
    reverse: HashMap<Entity, EntityId>,
    /// Time accumulated towards the next fixed physics step (seconds).
    pub acc: f32,
}

impl SaPhysics {
    pub fn new(world: World) -> Self {
        Self { world, entities: HashMap::new(), reverse: HashMap::new(), acc: 0.0 }
    }

    pub fn link(&mut self, id: EntityId, e: Entity) {
        self.entities.insert(id, e);
        self.reverse.insert(e, id);
    }

    pub fn unlink(&mut self, e: Entity) -> Option<EntityId> {
        let id = self.reverse.remove(&e)?;
        self.entities.remove(&id);
        Some(id)
    }

    pub fn sa_id(&self, e: Entity) -> Option<EntityId> {
        self.reverse.get(&e).copied()
    }

    /// Cast a ray in Bevy space. `static_only` restricts the test to map
    /// geometry (buildings); `ignore` skips one entity (e.g. the caster).
    pub fn cast_ray(&mut self, origin: Vec3, dir: Vec3, max_dist: f32, static_only: bool, ignore: Option<Entity>) -> Option<RayHit> {
        let dir = dir.normalize_or_zero();
        if dir == Vec3::ZERO || max_dist <= 0.0 {
            return None;
        }
        let ignore = ignore.and_then(|e| self.sa_id(e));
        let start = b2g(origin);
        let end = b2g(origin + dir * max_dist);
        let (id, t, cp) = self.world.line_of_sight(start, end, static_only, ignore)?;
        Some(RayHit {
            entity: self.entities.get(&id).copied(),
            sa_id: id,
            toi: t * max_dist,
            point: g2b(cp.point),
            normal: g2b(cp.normal),
            surface: cp.surface_b,
        })
    }
}
