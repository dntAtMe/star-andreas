//! Random pedestrians (population.md): loads the population data into the SA world's
//! `CPopulation`, creates the models of the peds it asks for, animates them and removes the
//! ones it drops.

use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use bevy::{mesh::skinning::SkinnedMeshInverseBindposes, prelude::*};
use sa_formats::{dff, population as pd, txd};
use sa_physics::{
    anim::AnimManager,
    npc::NpcState,
    paths::PathFind,
    ped::PedLogic,
    population::{PopData, Population},
    world::EntityId,
};

use crate::{
    player::{GameRoot, Ped, PedAnims, build_ped_visual},
    saphys::{SaBody, SaPhys, SaPhysExt, SaStep, gta_matrix},
    stream::{convert_texture, make_image},
    world::{WorldRes, g2b},
};

pub struct NpcPlugin;

impl Plugin for NpcPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NpcModels>()
            .add_systems(Startup, load_population)
            .add_systems(Update, (despawn_npcs, spawn_npcs, animate_npcs).chain().after(SaStep));
    }
}

/// A random NPC's root entity.
#[derive(Component)]
pub struct NpcPed {
    pub sa: EntityId,
    bones: Vec<Entity>,
    node_frames: Vec<usize>,
}

/// Parsed ped models (dff + textures) by model id.
#[derive(Resource, Default)]
struct NpcModels(HashMap<u32, Option<Arc<(dff::Clump, HashMap<String, (Handle<Image>, bool)>)>>>);

fn load_population(root: Res<GameRoot>, mut sa: ResMut<SaPhys>) -> Result<(), BevyError> {
    let read = |p: &str| std::fs::read(root.0.join(p)).with_context(|| p.to_string());
    let text = |p: &str| -> anyhow::Result<String> { Ok(String::from_utf8_lossy(&read(p)?).into_owned()) };
    let peds = pd::parse_peds_ide(&text("data/peds.ide")?);
    let stats = pd::parse_pedstats(&text("data/pedstats.dat")?);
    let popcycle = pd::parse_popcycle(&text("data/popcycle.dat")?);
    let groups = pd::parse_pedgrp(&text("data/pedgrp.dat")?);
    let zones = pd::parse_zones(&text("data/info.zon")?);
    let scm = read("data/script/main.scm").map(|d| pd::scan_scm_zone_settings(&d)).unwrap_or_default();
    let areas = (0..64)
        .map(|i| read(&format!("data/Paths/NODES{i}.DAT")).ok().and_then(|d| pd::parse_nodes(&d)))
        .collect();
    let paths = Arc::new(PathFind::new(areas));
    let data = PopData::load(&peds, stats, popcycle, &groups, zones, &scm, paths, AnimManager::group_by_name);
    info!("population: {} ped models, {} groups, {} zones", data.peds.len(), data.groups.len(), data.zones.len());
    if std::env::var("SA_NOPEDS").is_err() {
        sa.world.population = Some(Box::new(Population::new(Arc::new(data))));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_npcs(
    mut commands: Commands,
    world: Res<WorldRes>,
    anims: Option<Res<PedAnims>>,
    mut sa: ResMut<SaPhys>,
    mut cache: ResMut<NpcModels>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let Some(anims) = anims else { return };
    let Some(pop) = sa.world.population.as_mut() else { return };
    let reqs = std::mem::take(&mut pop.requests);
    let data = pop.data.clone();
    for req in reqs {
        let Some(info) = data.peds.get(&req.model) else { continue };
        let entry = cache.0.entry(req.model).or_insert_with(|| {
            let clump = world.0.file(&format!("{}.dff", info.model)).and_then(|d| dff::parse(d).ok())?;
            let textures = world
                .0
                .file(&format!("{}.txd", info.model))
                .and_then(|d| txd::parse(d).ok())
                .map(|t| {
                    t.into_iter()
                        .filter_map(|t| convert_texture(t, false))
                        .map(|t| (t.name.clone(), t.alpha, make_image(t)))
                        .map(|(n, a, img)| (n, (images.add(img), a)))
                        .collect()
                })
                .unwrap_or_default();
            Some(Arc::new((clump, textures)))
        });
        let Some(model) = entry.clone() else { continue };
        let vis = match build_ped_visual(&mut commands, &mut meshes, &mut materials, &mut bindposes, &model.0, &model.1) {
            Ok(v) => v,
            Err(e) => {
                warn!("ped {}: {e:#}", info.model);
                cache.0.insert(req.model, None);
                continue;
            }
        };
        let seed = (sa.world.rng.next() & 0xFFFF) as u16;
        let Some(id) = sa.world.add_npc(&req, vis.anim_clump, anims.0.clone(), seed) else {
            commands.entity(vis.model_root).despawn();
            continue;
        };
        let tf = Transform::from_translation(g2b(req.pos.to_array()));
        let m = gta_matrix(&tf);
        commands
            .spawn((
                tf,
                Visibility::Hidden,
                NpcPed { sa: id, bones: vis.bones, node_frames: vis.node_frames },
                SaBody::new(id, m),
            ))
            .add_child(vis.model_root);
    }
}

fn despawn_npcs(mut commands: Commands, mut sa: ResMut<SaPhys>, npcs: Query<(Entity, &NpcPed)>) {
    let removed = std::mem::take(&mut sa.world.npc_removed);
    if removed.is_empty() {
        return;
    }
    for (e, n) in &npcs {
        if removed.contains(&n.sa) {
            commands.entity(e).despawn();
        }
    }
}

/// The NPC skeletons from their SA clumps; hidden while faded in less than half.
fn animate_npcs(
    sa: Res<SaPhys>,
    mut npcs: Query<(&NpcPed, &mut Visibility)>,
    mut bones: Query<&mut Transform, (Without<NpcPed>, Without<Ped>)>,
) {
    let alpha = sa.alpha();
    for (n, mut vis) in &mut npcs {
        let Some(logic) = sa.logic::<PedLogic>(n.sa) else { continue };
        let a = logic.npc.as_ref().map_or(255, |s: &NpcState| s.alpha);
        let want = if a >= 128 { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
        let Some(clump) = logic.clump.as_deref() else { continue };
        for (k, &(q, t)) in clump.pose.iter().enumerate() {
            let (pq, pt) = logic.prev_pose.get(k).copied().unwrap_or((q, t));
            let Some(&e) = n.node_frames.get(k).and_then(|&f| n.bones.get(f)) else { continue };
            if let Ok(mut tf) = bones.get_mut(e) {
                tf.rotation = pq.slerp(q, alpha);
                tf.translation = pt.lerp(t, alpha);
            }
        }
    }
}
