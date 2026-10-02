//! Bridge to `sa-physics`: owns the SA physics world, steps it at the game's
//! own rate and mirrors bodies onto Bevy entities (interpolated).
//!
//! The SA world works in GTA space (Z up); conversion happens only here.

use bevy::{prelude::*, transform::TransformSystems};
use sa_physics::{
    physical::Matrix as GMatrix,
    surface::SurfaceInfos,
    world::{BodyLogic, EntityId, World as PhysWorld},
};

use crate::{
    player::GameRoot,
    world::{b2g, g2b},
};

/// Physics rate. SA was designed around ~30 fps (timestep 1.667 frames).
pub const SA_HZ: f32 = 30.0;
const MAX_STEPS_PER_FRAME: u32 = 4;

pub struct SaPhysPlugin;

impl Plugin for SaPhysPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, init)
            .add_systems(Update, step.in_set(SaStep))
            .add_systems(PostUpdate, sync_transforms.before(TransformSystems::Propagate))
            .add_observer(remove_building)
            .add_observer(remove_body);
    }
}

/// Systems that feed inputs to SA bodies run `.before(SaStep)`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SaStep;

#[derive(Resource)]
pub struct SaPhys {
    pub world: PhysWorld,
    acc: f32,
}

impl SaPhys {
    /// Fraction of the way from the last physics step to the next.
    pub fn alpha(&self) -> f32 {
        (self.acc * SA_HZ).clamp(0.0, 1.0)
    }

    pub fn logic_mut<T: BodyLogic>(&mut self, id: EntityId) -> Option<&mut T> {
        self.world.body_mut(id)?.logic.as_any_mut().downcast_mut::<T>()
    }

    pub fn logic<T: BodyLogic>(&self, id: EntityId) -> Option<&T> {
        self.world.body(id)?.logic.as_any().downcast_ref::<T>()
    }
}

/// A map instance registered as static SA geometry.
#[derive(Component)]
pub struct SaBuilding(pub EntityId);

/// An entity whose Transform follows an SA body.
#[derive(Component)]
pub struct SaBody {
    pub id: EntityId,
    prev: GMatrix,
    cur: GMatrix,
}

impl SaBody {
    pub fn new(id: EntityId, m: GMatrix) -> Self {
        Self { id, prev: m, cur: m }
    }
}

fn init(mut commands: Commands, root: Res<GameRoot>) {
    let read = |p: &str| std::fs::read(root.0.join(p)).map(|b| String::from_utf8_lossy(&b).into_owned());
    let surfaces = match (read("data/surface.dat"), read("data/surfinfo.dat")) {
        (Ok(a), Ok(b)) => SurfaceInfos::parse(&a, &b),
        _ => {
            warn!("surface.dat / surfinfo.dat missing; using default adhesion");
            SurfaceInfos::default()
        }
    };
    commands.insert_resource(SaPhys { world: PhysWorld::new(surfaces), acc: 0.0 });
}

fn step(time: Res<Time>, mut sa: ResMut<SaPhys>, mut bodies: Query<&mut SaBody>) {
    let dt = 1.0 / SA_HZ;
    let ts = dt * 50.0;
    sa.acc += time.delta_secs().min(0.25);
    let mut steps = 0;
    while sa.acc >= dt {
        if steps == MAX_STEPS_PER_FRAME {
            sa.acc = 0.0;
            break;
        }
        sa.world.process(ts);
        for mut b in &mut bodies {
            if let Some(body) = sa.world.body(b.id) {
                b.prev = b.cur;
                b.cur = body.phys.matrix;
            }
        }
        sa.acc -= dt;
        steps += 1;
    }
}

fn sync_transforms(sa: Res<SaPhys>, mut q: Query<(&SaBody, &mut Transform)>) {
    let alpha = sa.alpha();
    for (b, mut tf) in &mut q {
        if b.prev == b.cur && sa.world.body(b.id).is_some_and(|x| x.phys.is_static()) {
            continue;
        }
        let a = transform_from_gta(&b.prev);
        let c = transform_from_gta(&b.cur);
        tf.translation = a.translation.lerp(c.translation, alpha);
        tf.rotation = a.rotation.slerp(c.rotation, alpha);
    }
}

fn remove_building(ev: On<Remove, SaBuilding>, q: Query<&SaBuilding>, sa: Option<ResMut<SaPhys>>) {
    if let (Ok(b), Some(mut sa)) = (q.get(ev.entity), sa) {
        sa.world.remove(b.0);
    }
}

fn remove_body(ev: On<Remove, SaBody>, q: Query<&SaBody>, sa: Option<ResMut<SaPhys>>) {
    if let (Ok(b), Some(mut sa)) = (q.get(ev.entity), sa) {
        sa.world.remove(b.id);
    }
}

/// Bevy (Y-up) transform -> GTA (Z-up) matrix.
pub fn gta_matrix(tf: &Transform) -> GMatrix {
    let axis = |gta: [f32; 3]| Vec3::from(b2g(tf.rotation * g2b(gta)));
    GMatrix {
        right: axis([1.0, 0.0, 0.0]),
        fwd: axis([0.0, 1.0, 0.0]),
        up: axis([0.0, 0.0, 1.0]),
        pos: Vec3::from(b2g(tf.translation)),
    }
}

/// GTA matrix -> Bevy transform.
pub fn transform_from_gta(m: &GMatrix) -> Transform {
    // Bevy local X = GTA right, Y = GTA up, Z = -GTA forward.
    let r = g2b(m.right.to_array());
    let u = g2b(m.up.to_array());
    let f = g2b(m.fwd.to_array());
    let rot = Quat::from_mat3(&Mat3::from_cols(r, u, -f)).normalize();
    Transform { translation: g2b(m.pos.to_array()), rotation: rot, scale: Vec3::ONE }
}
