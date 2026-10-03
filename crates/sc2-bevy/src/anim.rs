//! Per-unit skeletons and M3 sequence playback.
//!
//! Each unit with a skinned model gets a bone entity hierarchy under a model
//! root (see [`model_root_transform`]). The playing sequence follows the unit's
//! SC2 state: moving -> Walk, engaged and standing -> Attack, otherwise Stand;
//! units that die stay around to play Death.

use std::sync::Arc;

use bevy::{camera::visibility::NoFrustumCulling, mesh::skinning::SkinnedMesh, prelude::*};
use sc2_formats::m3;

use crate::{
    Sc2Link,
    models::{ModelAsset, model_root_transform},
    units::Sc2Unit,
};

pub(crate) struct AnimPlugin;

impl Plugin for AnimPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, animate.before(bevy::transform::TransformSystems::Propagate));
    }
}

/// How long a corpse stays after its death animation, seconds.
const CORPSE_SECS: f32 = 2.5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnimState {
    Stand,
    Walk,
    Attack,
    Death,
}

#[derive(Component)]
pub struct Rig {
    asset: Arc<ModelAsset>,
    joints: Vec<Entity>,
    state: Option<AnimState>,
    seq: Option<usize>,
    /// Playback time in ms from the sequence start.
    t: f32,
}

/// Unit removed from the simulation, playing its death before despawning.
#[derive(Component)]
pub struct Dying {
    pub elapsed: f32,
}

/// Spawns the model under `unit` (skeleton + parts) and returns its rig, if skinned.
pub fn spawn_model(commands: &mut Commands, unit: Entity, asset: &Arc<ModelAsset>, scale: f32) -> Option<Rig> {
    let root = commands.spawn((model_root_transform(scale), Visibility::default())).id();
    commands.entity(unit).add_child(root);

    let joints: Vec<Entity> = asset
        .bones
        .iter()
        .map(|b| commands.spawn((b.rest, Visibility::default(), Name::new(b.name.clone()))).id())
        .collect();
    for (i, b) in asset.bones.iter().enumerate() {
        let parent = b.parent.map_or(root, |p| joints[p]);
        commands.entity(parent).add_child(joints[i]);
    }

    for (mesh, mat) in &asset.parts {
        let mut part = commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone())));
        if let Some(skin) = &asset.skin {
            part.insert((SkinnedMesh { inverse_bindposes: skin.clone(), joints: joints.clone() }, NoFrustumCulling));
        }
        let part = part.id();
        commands.entity(root).add_child(part);
    }

    asset.skin.is_some().then(|| Rig { asset: asset.clone(), joints, state: None, seq: None, t: 0.0 })
}

impl Rig {
    pub fn has_death(&self) -> bool {
        self.asset.clips.death.is_some()
    }

    /// Death animation length in seconds (0 if none).
    pub fn death_secs(&self) -> f32 {
        self.asset.clips.death.map_or(0.0, |i| duration_ms(&self.asset.sequences[i]) / 1000.0)
    }
}

fn duration_ms(s: &m3::Sequence) -> f32 {
    (s.end_ms - s.start_ms).max(1) as f32
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    link: Option<Res<Sc2Link>>,
    mut rigs: Query<(Entity, &Sc2Unit, &mut Rig, Option<&mut Dying>)>,
    mut joints: Query<&mut Transform, Without<Sc2Unit>>,
) {
    let dt = time.delta_secs();
    let step_secs = link.as_ref().map_or(0.1, |l| l.step_secs);
    for (e, unit, mut rig, dying) in &mut rigs {
        // Pick the state.
        let speed = unit.speed_cells(step_secs);
        let state = if let Some(mut d) = dying {
            d.elapsed += dt;
            if d.elapsed > rig.death_secs() + CORPSE_SECS {
                commands.entity(e).despawn();
                continue;
            }
            AnimState::Death
        } else if speed > 0.15 {
            AnimState::Walk
        } else if unit.engaged.is_some() {
            AnimState::Attack
        } else {
            AnimState::Stand
        };
        let clips = rig.asset.clips;
        if rig.state != Some(state) {
            rig.state = Some(state);
            rig.t = 0.0;
            rig.seq = match state {
                AnimState::Stand => clips.stand,
                AnimState::Walk => clips.walk,
                AnimState::Attack => clips.attack,
                AnimState::Death => clips.death,
            }
            .or(clips.stand);
        }
        let Some(si) = rig.seq else { continue };
        let asset = rig.asset.clone();
        let seq = &asset.sequences[si];

        // Advance; walk cycles follow the ground speed when the model says how fast it walks.
        let rate = match state {
            // movement_speed is in cells per 100 s.
            AnimState::Walk if seq.movement_speed > 0.0 => (speed * 100.0 / seq.movement_speed).clamp(0.4, 2.5),
            _ => 1.0,
        };
        let len = duration_ms(seq);
        rig.t += dt * 1000.0 * rate;
        rig.t = if state == AnimState::Death { rig.t.min(len) } else { rig.t % len };

        for (i, b) in asset.bones.iter().enumerate() {
            let Ok(mut tf) = joints.get_mut(rig.joints[i]) else { continue };
            let track = seq.bones.get(i);
            tf.translation = track.and_then(|t| t.translation.as_ref()).map_or(b.rest.translation, |t| Vec3::from_array(m3::sample_vec3(t, rig.t)));
            tf.rotation = track.and_then(|t| t.rotation.as_ref()).map_or(b.rest.rotation, |t| Quat::from_array(m3::sample_quat(t, rig.t)));
            tf.scale = track.and_then(|t| t.scale.as_ref()).map_or(b.rest.scale, |t| Vec3::from_array(m3::sample_vec3(t, rig.t)));
        }
    }
}
