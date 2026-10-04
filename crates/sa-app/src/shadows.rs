//! `CShadows::RenderStaticShadows` (0x708300): the world's static shadows (explosion
//! scorches, fire glow) as triangle fans, +0.06 above the ground, no fog, z-write off.
//! Type 1 blends SRCALPHA/INVSRCALPHA, type 2 is additive ONE/ONE (premultiplied with
//! alpha 0 here, since the fire glow's vertex alpha is 0).

use std::collections::HashMap;

use bevy::{asset::RenderAssetUsages, mesh::{Indices, PrimitiveTopology}, prelude::*, transform::TransformSystems};
use sa_physics::shadows::{ShadowTex, shadow_colour};

use crate::{
    player::GameRoot,
    saphys::{SaPhys, SaSync},
    stream::{convert_texture, make_image},
    world::g2b,
};

pub struct ShadowsPlugin;

impl Plugin for ShadowsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init).add_systems(PostUpdate, draw.after(SaSync).after(TransformSystems::Propagate));
    }
}

/// One mesh per (type, texture) batch.
#[derive(Resource)]
struct ShadowMeshes(HashMap<(u8, ShadowTex), (Entity, Handle<Mesh>)>);

fn empty_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn init(
    mut commands: Commands,
    root: Res<GameRoot>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let txd = match std::fs::read(root.0.join("models/particle.txd")).map_err(anyhow::Error::from).and_then(|d| sa_formats::txd::parse(&d)) {
        Ok(t) => t,
        Err(e) => {
            warn!("shadows disabled: particle.txd: {e:#}");
            return;
        }
    };
    let mut tex = HashMap::new();
    for t in txd.into_iter().filter_map(|t| convert_texture(t, false)) {
        tex.insert(t.name.clone(), images.add(make_image(t)));
    }
    let mut out = HashMap::new();
    for (ty, st, mode, bias) in [
        // Positive biases only: Bevy packs `depth_bias as i32` into the pipeline key.
        (1u8, ShadowTex::Heli, AlphaMode::Blend, 0.0),
        (2u8, ShadowTex::Exp, AlphaMode::Premultiplied, 0.5),
        (2u8, ShadowTex::Headlight, AlphaMode::Premultiplied, 0.5),
        (2u8, ShadowTex::Headlight1, AlphaMode::Premultiplied, 0.5),
    ] {
        let mesh = meshes.add(empty_mesh());
        let mat = materials.add(StandardMaterial {
            base_color_texture: tex.get(st.name()).cloned(),
            unlit: true,
            alpha_mode: mode,
            fog_enabled: false,
            double_sided: true,
            cull_mode: None,
            depth_bias: bias,
            ..default()
        });
        let e = commands
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat),
                Transform::default(),
                bevy::camera::visibility::NoFrustumCulling,
            ))
            .id();
        out.insert((ty, st), (e, mesh));
    }
    commands.insert_resource(ShadowMeshes(out));
}

fn draw(
    sa: Res<SaPhys>,
    batches: Option<Res<ShadowMeshes>>,
    camera: Single<&GlobalTransform, With<Camera3d>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tfs: Query<&mut Transform>,
    dbg: Res<crate::debug::DebugUi>,
) {
    let Some(batches) = batches else { return };
    let origin = camera.translation();
    let dn = sa.world.clock.dn_balance();
    let wet = sa.world.weather.wet_roads;
    for (&(ty, st), (e, mesh_h)) in &batches.0 {
        let (mut pos, mut uv, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::<u32>::new());
        for s in sa.world.shadows.statics.iter().flatten() {
            if s.ty != ty || s.tex != st || !dbg.shadows {
                continue;
            }
            let rgb = shadow_colour(s.ty, s.light, s.rgb, dn);
            // Type 2 is ONE/ONE (premultiplied here): alpha must not darken the ground.
            let a = if s.ty == 2 { 0 } else { ((1.0 - wet * 0.5) * s.intensity as f32) as i32 as u8 };
            let c = Color::srgba_u8(rgb[0], rgb[1], rgb[2], a).to_linear();
            for poly in &s.polys {
                let base = pos.len() as u32;
                for (p, t) in &poly.verts {
                    let w = g2b([p.x, p.y, p.z + 0.06]) - origin;
                    pos.push(w.to_array());
                    uv.push(t.to_array());
                    col.push([c.red, c.green, c.blue, c.alpha]);
                }
                // Fan table {0,2,1, 0,3,2, ...}.
                for k in 1..poly.verts.len() as u32 - 1 {
                    idx.extend([base, base + k + 1, base + k]);
                }
            }
        }
        if let Ok(mut tf) = tfs.get_mut(*e) {
            tf.translation = origin;
        }
        let Some(mut m) = meshes.get_mut(mesh_h) else { continue };
        if idx.is_empty() {
            *m = empty_mesh();
            continue;
        }
        let n = pos.len();
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
        m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        m.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
        m.insert_indices(Indices::U32(idx));
    }
}
