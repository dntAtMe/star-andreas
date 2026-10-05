//! `CPlayerPed::DrawTriangleForMouseRecruitPed` (0x60BA80, lock_on.md §5.2): the PC mouse
//! player's health triangle over `ped+0x79C` (the free-aim mouse target). Untextured, one
//! opaque vertex fading to transparent, coloured red→green by the target's health (black when
//! dead), 1 m above the ped origin and pulled 1 m toward the camera, depth-tested without
//! depth writes. Ballas (ped type 7) get it point-down.

use bevy::{asset::RenderAssetUsages, light::NotShadowCaster, mesh::PrimitiveTopology, prelude::*};
use sa_physics::ped::PedLogic;

use crate::{
    saphys::{SaPhys, SaPhysExt},
    world::g2b,
};

pub struct TargetTrianglePlugin;

impl Plugin for TargetTrianglePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(PostUpdate, draw);
    }
}

#[derive(Component)]
struct TargetTriangle(Handle<Mesh>);

fn tri_mesh(p: [[f32; 3]; 3], c: [[f32; 4]; 3]) -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, p.to_vec())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, c.to_vec())
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mesh = meshes.add(tri_mesh([[0.0; 3]; 3], [[0.0; 4]; 3]));
    let mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        double_sided: true,
        fog_enabled: false,
        ..default()
    });
    commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), Transform::IDENTITY, Visibility::Hidden, NotShadowCaster, TargetTriangle(mesh)));
}

fn srgb_to_linear(v: u8) -> f32 {
    let x = v as f32 / 255.0;
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn draw(sa: Res<SaPhys>, tri: Single<(&TargetTriangle, &mut Visibility)>, mut meshes: ResMut<Assets<Mesh>>) {
    let (tri, mut vis) = tri.into_inner();
    let world = &sa.world;
    let target = world.mouse_target.zip(world.player_id());
    let Some((t, pl)) = target else {
        *vis = Visibility::Hidden;
        return;
    };
    let (Some(tb), Some(pb)) = (world.body(t), world.body(pl)) else {
        *vis = Visibility::Hidden;
        return;
    };
    let Some(tl) = sa.logic::<PedLogic>(t) else {
        *vis = Visibility::Hidden;
        return;
    };
    let tp = tb.phys.matrix.pos;
    let h = (tl.tasks.health.health / tl.tasks.health.max_health.max(1.0)).min(1.0);
    let (r, g) = if h > 0.0 { (((1.0 - h) * 255.0) as i32 as u8, (h * 255.0) as i32 as u8) } else { (0, 0) };
    let s = (((tp - pb.phys.matrix.pos).length() - 10.0).max(0.0) * 0.02).min(1.0) * 0.825 + 0.175;
    let cam = world.cam_info();
    let right = cam.front.cross(cam.up).normalize_or(Vec3::X);
    let rt = right * s;
    let u = Vec3::new(0.0, 0.0, s);
    let p = tp + Vec3::new(0.0, 0.0, 1.0);
    let gang1 = tl.npc.as_ref().is_some_and(|n| n.ped_type == 7);
    let mut v = if gang1 { [p, p - rt + u, p + rt + u] } else { [p + u, p + rt, p - rt] };
    for x in &mut v {
        *x += (cam.pos - *x).normalize_or_zero();
    }
    let (lr, lg) = (srgb_to_linear(r), srgb_to_linear(g));
    let pos = v.map(|x| g2b(x.to_array()).to_array());
    let col = [[lr, lg, 0.0, 1.0], [lr, lg, 0.0, 0.0], [lr, lg, 0.0, 0.0]];
    if let Some(mut m) = meshes.get_mut(&tri.0) {
        *m = tri_mesh(pos, col);
    }
    *vis = Visibility::Inherited;
}
