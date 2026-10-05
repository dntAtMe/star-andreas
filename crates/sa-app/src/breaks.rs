//! Render the pieces of broken breakable objects (`BreakManager_c::Render`): each piece is
//! an unlit, alpha-blended triangle soup with baked vertex colours, posed by its piece matrix.

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    mesh::PrimitiveTopology,
    prelude::*,
};

use crate::{
    saphys::{SaPhys, SaStep, transform_from_gta},
    world::g2b,
};

pub struct BreaksPlugin;

impl Plugin for BreaksPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BreakTextures>().add_systems(Update, (debug_break.before(SaStep), sync_breaks.after(SaStep)));
    }
}

/// The piece textures of a model's BreakablePlugin data, keyed by the data's `Arc` pointer
/// (filled by the streamer from the model's TXD chain).
#[derive(Resource, Default)]
pub struct BreakTextures(pub HashMap<usize, Vec<Option<Handle<Image>>>>);

#[derive(Component)]
struct PieceVis {
    idx: usize,
    material: Handle<StandardMaterial>,
    alpha: u8,
}

fn srgb(c: u8) -> f32 {
    let x = c as f32 / 255.0;
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

#[allow(clippy::too_many_arguments)]
fn sync_breaks(
    mut commands: Commands,
    sa: Res<SaPhys>,
    tex: Res<BreakTextures>,
    mut spawned: Local<HashMap<u32, Vec<Entity>>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut pieces: Query<(&mut PieceVis, &mut Transform, &mut Visibility)>,
) {
    let objects = &sa.world.breaks.objects;
    spawned.retain(|uid, ents| {
        let alive = objects.iter().any(|o| o.uid == *uid);
        if !alive {
            for e in ents.drain(..) {
                commands.entity(e).despawn();
            }
        }
        alive
    });
    for o in objects {
        let ents = spawned.entry(o.uid).or_insert_with(|| {
            let texs = tex.0.get(&(std::sync::Arc::as_ptr(&o.data) as usize));
            o.pieces
                .iter()
                .enumerate()
                .map(|(idx, p)| {
                    let mut pos = Vec::with_capacity(p.tris.len() * 3);
                    let mut uv = Vec::with_capacity(p.tris.len() * 3);
                    let mut col = Vec::with_capacity(p.tris.len() * 3);
                    for t in &p.tris {
                        for k in 0..3 {
                            pos.push(g2b(t.pos[k].to_array()));
                            uv.push(t.uv[k]);
                            let c = t.col[k];
                            col.push([srgb(c[0]), srgb(c[1]), srgb(c[2]), 1.0]);
                        }
                    }
                    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
                    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
                    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
                    mesh.compute_flat_normals();
                    let image = p.material.and_then(|m| texs.and_then(|t| t.get(m).cloned().flatten()));
                    let material = materials.add(StandardMaterial {
                        base_color_texture: image,
                        unlit: true,
                        double_sided: true,
                        cull_mode: None,
                        alpha_mode: AlphaMode::Blend,
                        ..default()
                    });
                    commands
                        .spawn((
                            Mesh3d(meshes.add(mesh)),
                            MeshMaterial3d(material.clone()),
                            transform_from_gta(&p.matrix),
                            PieceVis { idx, material, alpha: 255 },
                        ))
                        .id()
                })
                .collect()
        });
        for &e in ents.iter() {
            let Ok((mut vis, mut tf, mut v)) = pieces.get_mut(e) else { continue };
            let Some(p) = o.pieces.get(vis.idx) else { continue };
            *tf = transform_from_gta(&p.matrix);
            let a = p.alpha(o.smash);
            if a != vis.alpha {
                vis.alpha = a;
                if let Some(mut m) = materials.get_mut(&vis.material) {
                    m.base_color = Color::srgba(1.0, 1.0, 1.0, a as f32 / 255.0);
                }
            }
            let want = if a == 0 { Visibility::Hidden } else { Visibility::Inherited };
            if *v != want {
                *v = want;
            }
        }
    }
}

/// Debug `SA_BREAK=1`: every 3 s, smash the nearest intact breakable object in view within
/// 40 m of the player (ObjectDamage 1000 at its position).
pub fn debug_break(time: Res<Time>, mut sa: ResMut<SaPhys>, mut next: Local<f32>) {
    if std::env::var("SA_BREAK").is_err() || time.elapsed_secs() < (*next).max(15.0) {
        return;
    }
    *next = time.elapsed_secs() + 3.0;
    let Some(pid) = sa.world.player_id() else { return };
    let Some(pp) = sa.world.body(pid).map(|b| b.phys.matrix.pos) else { return };
    let cam = sa.world.cam_info();
    let best = sa
        .world
        .body_ids()
        .into_iter()
        .filter_map(|id| {
            let b = sa.world.body(id)?;
            let o = b.logic.as_any().downcast_ref::<sa_physics::objects::ObjectLogic>()?;
            (o.breakable.is_some() && !o.hidden && matches!(o.info.damage_effect, 200 | 202)).then(|| (id, b.phys.matrix.pos.distance(pp), o.model.clone()))
        })
        .filter(|x| x.1 < 40.0 && sa.world.body(x.0).is_some_and(|b| (b.phys.matrix.pos - cam.pos).dot(cam.front) > 0.0))
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((id, d, name)) = best else { return };
    if let Some(b) = sa.world.body_mut(id) {
        let pos = b.phys.matrix.pos;
        if let Some(o) = b.logic.as_any_mut().downcast_mut::<sa_physics::objects::ObjectLogic>() {
            o.object_damage(&mut b.phys, 1000.0, Some(pos), Some(Vec3::Z), None, 51);
            info!("SA_BREAK: smashed {name} at {pos:?} ({d:.1} m)");
        }
    }
}
