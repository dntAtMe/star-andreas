//! Render-side interpolation for bodies simulated in the fixed timestep.
//!
//! Physics runs at a fixed rate in `FixedUpdate`, so a frame may see zero or
//! several steps. Instead of moving the rigid body (Rapier treats its
//! Transform as authoritative), we offset its visual child so it renders at
//! the pose blended between the last two physics steps.

use bevy::{prelude::*, transform::TransformSystems};
use bevy_rapier3d::prelude::PhysicsSet;

pub struct InterpPlugin;

impl Plugin for InterpPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(FixedUpdate, record.after(PhysicsSet::Writeback))
            .add_systems(PostUpdate, apply.before(TransformSystems::Propagate));
    }
}

/// On a physics body: interpolate the `visual` child, whose resting local
/// transform is `base`.
#[derive(Component)]
pub struct Interp {
    pub visual: Entity,
    pub base: Transform,
    prev: Transform,
    cur: Transform,
}

impl Interp {
    pub fn new(visual: Entity, base: Transform, body: Transform) -> Self {
        Self { visual, base, prev: body, cur: body }
    }

    /// Body pose blended between the last two physics steps.
    pub fn pose(&self, alpha: f32) -> Transform {
        Transform {
            translation: self.prev.translation.lerp(self.cur.translation, alpha),
            rotation: self.prev.rotation.slerp(self.cur.rotation, alpha),
            scale: Vec3::ONE,
        }
    }
}

fn record(mut bodies: Query<(&Transform, &mut Interp)>) {
    for (tf, mut i) in &mut bodies {
        i.prev = i.cur;
        i.cur = *tf;
    }
}

fn apply(
    fixed: Res<Time<Fixed>>,
    mut bodies: Query<(&Transform, &mut Interp)>,
    mut visuals: Query<&mut Transform, Without<Interp>>,
) {
    let alpha = fixed.overstep_fraction();
    for (body, mut i) in &mut bodies {
        // Moved outside physics (teleport, enter/exit car): snap, don't smear.
        if body.translation.distance_squared(i.cur.translation) > 1e-6 {
            i.prev = *body;
            i.cur = *body;
        }
        let pose = i.pose(alpha);
        let inv = i.cur.rotation.inverse();
        let rot = inv * pose.rotation;
        let offset = inv * (pose.translation - i.cur.translation);
        if let Ok(mut v) = visuals.get_mut(i.visual) {
            v.translation = offset + rot * i.base.translation;
            v.rotation = rot * i.base.rotation;
        }
    }
}
