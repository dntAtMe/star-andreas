//! Knockable street props (object.dat): lamp posts, hydrants, signs, bins,
//! fences, cones... They stay fixed until a vehicle hits them harder than
//! their uproot limit, then turn into dynamic bodies.
//!
//! Rapier has already stopped the car against the (fixed) prop by the time
//! the contact event arrives, so the car's pre-impact velocity is restored
//! minus the momentum handed to the prop.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use sa_formats::objdat::ObjectPhysics;

use crate::vehicle::{PrevVelocity, Vehicle, drive_vehicles};

/// Contact force (N) above which prop contacts are reported at all.
pub const PROP_EVENT_FORCE: f32 = 2000.0;
/// SA measures velocities per 1/50 s frame, so its impulses are kg·m/s / 50.
const GAME_FPS: f32 = 50.0;
const MAX_PROP_MASS: f32 = 5000.0;

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(FixedUpdate, smash_props.before(drive_vehicles).before(PhysicsSet::SyncBackend));
    }
}

#[derive(Component)]
pub struct Prop {
    pub physics: ObjectPhysics,
    /// Convex hull used once the prop is loose (trimeshes can't be dynamic).
    pub hull: Collider,
    pub loose: bool,
}

/// Fixed collider child of a not-yet-loose prop.
#[derive(Component)]
pub struct PropPart;

fn smash_props(
    mut commands: Commands,
    time: Res<Time>,
    mut events: MessageReader<ContactForceEvent>,
    parents: Query<&ChildOf, With<PropPart>>,
    parts: Query<(), With<PropPart>>,
    mut props: Query<(&mut Prop, &Children)>,
    mut cars: Query<(&mut Velocity, &PrevVelocity, &Vehicle)>,
) {
    let dt = time.delta_secs().clamp(1.0 / 240.0, 1.0 / 30.0);
    let mut handled = HashSet::new();
    for ev in events.read() {
        let (car_e, part_e) = if cars.contains(ev.collider1) {
            (ev.collider1, ev.collider2)
        } else if cars.contains(ev.collider2) {
            (ev.collider2, ev.collider1)
        } else {
            continue;
        };
        let Ok(child_of) = parents.get(part_e) else { continue };
        let root = child_of.parent();
        if !handled.insert(root) {
            continue;
        }
        let Ok((mut prop, children)) = props.get_mut(root) else { continue };
        if prop.loose {
            continue;
        }
        let impulse = ev.total_force_magnitude * dt / GAME_FPS;
        if impulse < prop.physics.uproot.max(1.0) {
            continue;
        }
        let Ok((mut vel, prev, car)) = cars.get_mut(car_e) else { continue };
        prop.loose = true;
        debug!(
            "prop {root:?} knocked loose: impulse {impulse:.0} > uproot {:.0} (mass {:.0}) at {:.0} km/h",
            prop.physics.uproot,
            prop.physics.mass,
            prev.linear.length() * 3.6
        );

        for c in children.iter().filter(|c| parts.contains(*c)) {
            commands.entity(c).despawn();
        }
        let m_car = car.mass();
        let m_prop = prop.physics.mass.clamp(5.0, MAX_PROP_MASS);
        let share = m_car / (m_car + m_prop);
        let pv = prev.linear;
        let dir = pv.normalize_or_zero();
        let fling = pv * share * (1.0 + prop.physics.elasticity) + Vec3::Y * (1.0 + pv.length() * 0.08);
        let tip = Vec3::Y.cross(dir).normalize_or_zero() * pv.length() * 0.5;
        commands.entity(root).insert((
            RigidBody::Dynamic,
            prop.hull.clone(),
            ColliderMassProperties::Mass(m_prop),
            Velocity { linear: fling, angular: tip },
            Restitution::coefficient(prop.physics.elasticity.clamp(0.0, 0.8)),
            Damping { linear_damping: 0.05, angular_damping: 0.3 },
            Ccd::enabled(),
        ));

        // The car ploughs through, losing a share of its speed.
        vel.linear = pv * (1.0 - 0.5 * m_prop / (m_car + m_prop));
        vel.angular = prev.angular;
    }
}
