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
            .add_systems(Update, (despawn_npcs, spawn_npcs, animate_npcs, log_responses, provoke, debug_wanted).chain().after(SaStep));
    }
}

/// A random NPC's root entity.
#[derive(Component)]
pub struct NpcPed {
    pub sa: EntityId,
    pub(crate) bones: Vec<Entity>,
    pub(crate) node_frames: Vec<usize>,
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
    let data = PopData::load(&peds, stats, popcycle, &groups, zones, &scm, paths.clone(), AnimManager::group_by_name);
    // CCarCtrl: cargrp.dat and the car models (vehicles.ide 'car' entries).
    // CDecisionMakerTypes: PedEvent.txt, the pedstats decision makers and RANDOM.ped.
    {
        use sa_formats::decision::{parse_decision_maker, parse_ped_event_txt};
        let ev2dec = parse_ped_event_txt(&text("data/decision/PedEvent.txt")?);
        let dm = |f: &str| text(&format!("data/decision/allowed/{f}")).map(|t| parse_decision_maker(&t, &ev2dec)).unwrap_or_default();
        let dms = ["GangMbr.ped", "Cop.ped", "R_Norm.ped", "R_Tough.ped", "R_Weak.ped", "Fireman.ped", "m_empty.ped", "Indoors.ped"]
            .iter()
            .map(|f| dm(f))
            .collect();
        sa.world.decisions = Some(Arc::new(sa_physics::pedevents::DecisionData {
            event_to_decision: ev2dec,
            dms,
            random_ped: dm("RANDOM.ped"),
        }));
    }
    let car_groups = pd::parse_cargrp(&text("data/cargrp.dat")?);
    let cars: std::collections::HashSet<String> = sa_formats::vehicle::parse_vehicles_ide(&text("data/vehicles.ide")?)
        .into_iter()
        .filter(|d| d.kind == "car")
        .map(|d| d.model)
        .collect();
    if std::env::var("SA_NOTRAFFIC").is_err() {
        sa.world.traffic = Some(Box::new(sa_physics::traffic::Traffic::new(paths, car_groups, cars)));
    }
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

/// Debug `SA_EVLOG=1`: log NPC event responses as they start.
fn log_responses(sa: Res<SaPhys>, mut last: Local<HashMap<u32, String>>) {
    use sa_physics::pedevents::Resp;
    if std::env::var("SA_EVLOG").is_err() {
        return;
    }
    for id in sa.world.body_ids() {
        let EntityId::Body(i) = id else { continue };
        let Some(n) = sa.logic::<PedLogic>(id).and_then(|p| p.npc.as_ref()) else { continue };
        let kind = n.response.as_ref().map_or(String::new(), |r| {
            let name = match r {
                Resp::SmartFlee(_) => "smart flee",
                Resp::Duck { .. } => "duck",
                Resp::AimedAt { .. } => "react to gun aimed at",
                Resp::HandsUp { .. } => "hands up",
                Resp::Cower { .. } => "cower",
                Resp::ShakeFist { .. } => "shake fist",
                Resp::EvasiveStep { .. } => "evasive step",
                Resp::EvasiveDive { .. } => "evasive dive",
                Resp::KillPedOnFoot(k) => if k.fighting { "kill ped on foot (fighting)" } else { "kill ped on foot (seek)" },
                Resp::Gesture { .. } => "gesture",
                Resp::EnterCar { .. } => "enter car timed",
            };
            format!("{name} (event {:?})", n.cur_event.as_ref().map(|e| (e.kind.ty(), e.task)))
        });
        if last.get(&i).is_none_or(|k| *k != kind) {
            if !kind.is_empty() {
                info!("npc {i}: {kind}");
            }
            last.insert(i, kind);
        }
    }
}

/// Debug `SA_PROVOKE=1`: every 5 s the nearest NPC within 30 m counts as damaged by the player
/// (the DAMAGE event, as if punched).
fn provoke(time: Res<Time>, mut sa: ResMut<SaPhys>, mut next: Local<f32>) {
    if std::env::var("SA_PROVOKE").is_err() || time.elapsed_secs() < (*next).max(12.0) {
        return;
    }
    *next = time.elapsed_secs() + 5.0;
    let Some(pid) = sa.world.player_id() else { return };
    let Some(pp) = sa.world.body(pid).map(|b| b.phys.matrix.pos) else { return };
    let best = sa
        .world
        .body_ids()
        .into_iter()
        .filter(|&id| sa.logic::<PedLogic>(id).is_some_and(|p| p.npc.as_ref().is_some_and(|n| n.response.is_none())))
        .filter_map(|id| sa.world.body(id).map(|b| (id, b.phys.matrix.pos.distance(pp))))
        .filter(|x| x.1 < 30.0)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((id, d)) = best {
        if let Some(n) = sa.logic_mut::<PedLogic>(id).and_then(|p| p.npc.as_mut()) {
            n.damaged_by = Some(Some(pid));
            info!("SA_PROVOKE: npc {id:?} at {d:.1} m");
        }
    }
}

/// Debug `SA_WANTED=<level>`: set the wanted level once (after 12 s) and log the cops.
fn debug_wanted(time: Res<Time>, mut sa: ResMut<SaPhys>, mut done: Local<bool>, mut next_log: Local<f32>) {
    let Some(level) = std::env::var("SA_WANTED").ok().and_then(|v| v.parse::<i32>().ok()) else { return };
    if !*done && time.elapsed_secs() > 12.0 {
        *done = true;
        let now = sa.world.now_ms;
        sa.world.wanted.set_wanted_level(level, now);
    }
    if time.elapsed_secs() > *next_log {
        *next_log = time.elapsed_secs() + 2.0;
        let cops: Vec<String> = sa
            .world
            .body_ids()
            .into_iter()
            .filter_map(|id| {
                let p = sa.logic::<PedLogic>(id)?;
                let n = p.npc.as_ref().filter(|n| n.ped_type == 6)?;
                let state = if n.pursuit.as_ref().is_some_and(|p| p.arresting()) { " arresting" } else if n.pursuit.is_some() { " pursuing" } else { "" };
                let w = p.tasks.active_weapon();
                let armed = n.pursuit.as_ref().and_then(|p| p.kill.armed.as_ref()).map_or(String::new(), |a| a.describe());
                let d = n.resp_in.threat_pos.zip(sa.world.body(id).map(|b| b.phys.matrix.pos)).map_or(-1.0, |(a, b)| (a - b).length());
                Some(format!(
                    "{id:?}{state} w{} ammo {}{} [{armed}] d {d:.1} vis {}",
                    w.ty,
                    w.ammo_in_clip,
                    if p.tasks.gun.is_some() { " gun" } else { "" },
                    n.resp_in.threat_visible
                ) + &p.clump.as_deref().map_or(String::new(), |c| {
                    c.assocs.iter().filter(|a| a.blend > 0.05).map(|a| format!(" {}:{}@{:.2}", a.group, a.id, a.blend)).collect::<String>()
                }))
            })
            .collect();
        let hp = sa.world.player_id().and_then(|id| sa.logic::<PedLogic>(id)).map_or(0.0, |p| p.tasks.health.health);
        info!("wanted {} cops in pursuit {} hp {hp:.0} | {}", sa.world.wanted.level, sa.world.wanted.cops_in_pursuit, cops.join(", "));
    }
}

/// A script ped to create (`CREATE_CHAR` and friends): model name (dff / txd), peds.ide id,
/// ped type, anim group, GTA position and heading, optional seat.
pub(crate) struct ScriptPedReq {
    pub model: String,
    pub id: u32,
    pub ped_type: u8,
    pub anim_group: usize,
    pub pos: Vec3,
    pub heading: f32,
    pub seat: Option<(EntityId, i8)>,
}

/// Build a script ped's model and add it to the physics world as a mission ped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_script_ped(
    In(req): In<ScriptPedReq>,
    mut commands: Commands,
    world: Res<WorldRes>,
    anims: Option<Res<PedAnims>>,
    mut sa: ResMut<SaPhys>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) -> Option<EntityId> {
    let anims = anims?;
    let clump = world.0.file(&format!("{}.dff", req.model)).and_then(|d| dff::parse(d).ok())?;
    let textures: HashMap<String, (Handle<Image>, bool)> = world
        .0
        .file(&format!("{}.txd", req.model))
        .and_then(|d| txd::parse(d).ok())
        .map(|t| {
            t.into_iter()
                .filter_map(|t| convert_texture(t, false))
                .map(|t| (t.name.clone(), t.alpha, make_image(t)))
                .map(|(n, a, img)| (n, (images.add(img), a)))
                .collect()
        })
        .unwrap_or_default();
    let vis = match build_ped_visual(&mut commands, &mut meshes, &mut materials, &mut bindposes, &clump, &textures) {
        Ok(v) => v,
        Err(e) => {
            warn!("script ped {}: {e:#}", req.model);
            return None;
        }
    };
    let Some(id) = sa.world.add_mission_ped(req.id, req.ped_type, req.anim_group, req.pos, req.heading, vis.anim_clump, anims.0.clone(), req.seat) else {
        commands.entity(vis.model_root).despawn();
        return None;
    };
    let tf = Transform::from_translation(g2b(req.pos.to_array()));
    let m = gta_matrix(&tf);
    commands
        .spawn((tf, Visibility::Hidden, NpcPed { sa: id, bones: vis.bones, node_frames: vis.node_frames }, SaBody::new(id, m)))
        .add_child(vis.model_root);
    Some(id)
}
