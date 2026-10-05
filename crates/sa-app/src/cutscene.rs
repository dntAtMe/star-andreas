//! `CCutsceneMgr` (cutscene.md): cuts.img cutscenes with their actors, ANPK anims, flyby
//! camera (`CCam::Process_FlyBy`), subtitles, widescreen bars, audio track and fades.
//!
//! Driven by the script opcodes (02E4 LOAD_CUTSCENE, 06B9 HAS_CUTSCENE_LOADED, 02E7
//! START_CUTSCENE, 02E9 HAS_CUTSCENE_FINISHED, 02EA CLEAR_CUTSCENE) through [`Cutscene`];
//! `SA_CUTSCENE=<name>` plays one on its own for testing.

use std::{collections::HashMap, f32::consts::FRAC_PI_2};

use anyhow::{Context, Result};
use bevy::{
    camera::Projection,
    input::mouse::MouseButton,
    mesh::skinning::SkinnedMeshInverseBindposes,
    prelude::*,
    transform::TransformSystems,
};
use sa_formats::{
    cutscene::{self, CamSplines, Cut, Flyby},
    dff, ifp,
    img::Img,
    txd,
};
use sa_physics::ped::PedLogic;

use crate::{
    hud::Overlay,
    player::{build_ped_visual, frame_transform, GameRoot, OrbitCam, Ped, PedVisual},
    saphys::{SaPhys, SaPhysExt, SaSync},
    stream::{convert_texture, make_image},
    world::{g2b, WorldRes},
};

pub struct CutscenePlugin;

impl Plugin for CutscenePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cutscene>()
            .add_systems(Update, (test_driver, load_cutscene).chain())
            .add_systems(Update, freeze_player.after(crate::player::player_control).before(crate::saphys::SaStep))
            .add_systems(
                PostUpdate,
                update_cutscene
                    .after(SaSync)
                    .after(crate::camera::sa_camera)
                    .after(crate::player::orbit_camera)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// `ms_cutsceneLoadStatus` / `ms_cutscenePlayStatus` collapsed into one state.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    #[default]
    Idle,
    /// 02E4 issued; loaded on the next Update.
    Loading,
    Loaded,
    /// 02E7 issued; SetupCutsceneToStart runs on the next frame.
    Starting,
    Running,
}

/// One cutscene object (`CCutsceneObject`) and its anim tracks bound to frames.
struct CutObject {
    root: Entity,
    tracks: Vec<(Entity, Vec<ifp::Key>)>,
    /// csplay: the bone entity chains (root → bone) of the shoulder bones, for
    /// CCutsceneObject::PreRender's ShoulderBoneRotation.
    shoulders: HashMap<i32, Vec<Entity>>,
}

/// The cutscene manager state (`CCutsceneMgr` statics).
#[derive(Resource, Default)]
pub struct Cutscene {
    pub status: Status,
    pub name: String,
    cut: Cut,
    splines: Option<CamSplines>,
    flyby: Flyby,
    /// `ms_cutsceneTimer`, seconds.
    timer: f32,
    cur_text: usize,
    /// The current brief (subtitle): GXT key and the cutscene time it expires (ms).
    message: Option<(String, f32)>,
    fade_started: bool,
    objects: Vec<CutObject>,
    track: Option<Entity>,
    /// `ms_wasCutsceneSkipped`.
    pub skipped: bool,
    /// Cleared objects / track waiting to be despawned, and the presentation to restore.
    despawn: Vec<Entity>,
    clear_request: bool,
}

impl Cutscene {
    /// 02E4 LOAD_CUTSCENE.
    pub fn load(&mut self, name: &str) {
        self.name = name.to_ascii_lowercase();
        self.status = Status::Loading;
    }
    /// 06B9 HAS_CUTSCENE_LOADED.
    pub fn has_loaded(&self) -> bool {
        self.status == Status::Loaded
    }
    /// 02E7 START_CUTSCENE.
    pub fn start(&mut self) {
        if self.status == Status::Loaded {
            self.status = Status::Starting;
        }
    }
    /// 02E9 HAS_CUTSCENE_FINISHED: no camera data, or the flyby at its end.
    pub fn has_finished(&self) -> bool {
        match self.status {
            Status::Running => self.splines.is_none() || self.flyby.along >= 1.0,
            Status::Starting => false,
            _ => true,
        }
    }
    /// 02EA CLEAR_CUTSCENE (DeleteCutsceneData): the state goes at once (a LOAD may follow in
    /// the same frame); the entities and the presentation are restored by the next update.
    pub fn clear(&mut self) {
        let objs: Vec<Entity> = self.objects.drain(..).map(|o| o.root).collect();
        self.despawn.extend(objs);
        self.despawn.extend(self.track.take());
        self.status = Status::Idle;
        self.message = None;
        self.splines = None;
        self.clear_request = true;
    }
    pub fn running(&self) -> bool {
        matches!(self.status, Status::Starting | Status::Running)
    }
}

/// `FindCutsceneAudioTrackId` (table 0x8D0AA8), the intro entries.
fn audio_track(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "intro1a" => 703,
        "intro1b" => 704,
        "intro2a" => 705,
        "prolog1" => 706,
        "prolog2" => 707,
        "prolog3" => 708,
        _ => return None,
    })
}

/// The ANPK loader's compression (cutscene.md §3.3): keys of anims not in the `uncompress`
/// list are stored as truncated i16 (quaternion ×4096, time ×60, translation ×1024; the
/// translation wraps outside ±32 m).
fn compress_keys(keys: &mut [ifp::Key]) {
    let q = |v: f32| (v * 4096.0).trunc() as i32 as i16 as f32 / 4096.0;
    let t = |v: f32| (v * 1024.0).trunc() as i32 as i16 as f32 / 1024.0;
    for k in keys {
        k.rot = k.rot.map(q);
        k.time = (k.time * 60.0).trunc() as i32 as i16 as f32 / 60.0;
        k.pos = k.pos.map(|p| p.map(t));
    }
}

/// Sample a track at `t` seconds (absolute key times): slerp / lerp between the bracketing keys.
fn sample(keys: &[ifp::Key], t: f32) -> (Quat, Option<Vec3>) {
    let n = keys.len();
    let i = keys.partition_point(|k| k.time <= t);
    let get = |k: &ifp::Key| (Quat::from_array(k.rot), k.pos.map(Vec3::from));
    if i == 0 {
        return get(&keys[0]);
    }
    if i >= n {
        return get(&keys[n - 1]);
    }
    let (a, b) = (&keys[i - 1], &keys[i]);
    let (qa, pa) = get(a);
    let (mut qb, pb) = get(b);
    if qa.dot(qb) < 0.0 {
        qb = -qb;
    }
    let dt = b.time - a.time;
    let u = if dt > 0.0 { ((t - a.time) / dt).clamp(0.0, 1.0) } else { 1.0 };
    let pos = match (pa, pb) {
        (Some(x), Some(y)) => Some(x.lerp(y, u)),
        (x, _) => x,
    };
    (qa.slerp(qb, u).normalize(), pos)
}

/// Frame names compare trimmed and case-insensitively; prop sequences carry the model as a
/// prefix (`Cs9MM_hern:Root`).
fn frame_key(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).trim().to_ascii_lowercase()
}

/// A non-skinned clump: one entity per frame, one mesh per atomic material.
fn build_static(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    clump: &dff::Clump,
    tex: &dyn Fn(&str) -> Option<(Handle<Image>, bool)>,
) -> (Entity, Vec<Entity>) {
    let model_root = commands.spawn((Transform::from_rotation(Quat::from_rotation_x(-FRAC_PI_2)), Visibility::default())).id();
    let frames: Vec<Entity> =
        clump.frames.iter().map(|f| commands.spawn((frame_transform(f), Visibility::default(), Name::new(f.name.clone()))).id()).collect();
    for (i, f) in clump.frames.iter().enumerate() {
        let parent = if f.parent >= 0 { frames[f.parent as usize] } else { model_root };
        commands.entity(parent).add_child(frames[i]);
    }
    for a in &clump.atomics {
        let Some(geo) = clump.geometries.get(a.geometry as usize) else { continue };
        let Some(&frame) = frames.get(a.frame as usize) else { continue };
        for (mi, mesh) in crate::vehicle::geometry_meshes(geo) {
            let m = &geo.materials[mi];
            let t = m.texture.as_ref().and_then(|t| tex(&t.name.to_ascii_lowercase()));
            let c = m.color;
            let alpha_mode = if c[3] < 255 {
                AlphaMode::Blend
            } else if t.as_ref().is_some_and(|t| t.1) {
                AlphaMode::Mask(0.5)
            } else {
                AlphaMode::Opaque
            };
            let material = materials.add(StandardMaterial {
                base_color: Color::srgba_u8(c[0], c[1], c[2], c[3]),
                base_color_texture: t.map(|t| t.0),
                alpha_mode,
                perceptual_roughness: 0.8,
                double_sided: true,
                cull_mode: None,
                ..default()
            });
            let part = commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material), crate::dynlight::DynLit)).id();
            commands.entity(frame).add_child(part);
        }
    }
    (model_root, frames)
}

fn load_txd(data: &[u8], images: &mut Assets<Image>) -> HashMap<String, (Handle<Image>, bool)> {
    txd::parse(data)
        .map(|v| {
            v.into_iter()
                .filter_map(|t| convert_texture(t, false))
                .map(|t| (t.name.to_ascii_lowercase(), t.alpha, make_image(t)))
                .map(|(n, a, img)| (n, (images.add(img), a)))
                .collect()
        })
        .unwrap_or_default()
}

/// `SA_CUTSCENE=<name>`: the script pattern of §2.7 on its own (load, wait, start with a
/// 1 s fade-in, wait for the end, instant black, clear, fade back in).
fn test_driver(
    time: Res<Time>,
    mut cs: ResMut<Cutscene>,
    mut overlay: ResMut<Overlay>,
    mut sa: ResMut<SaPhys>,
    mut stage: Local<u8>,
    mut wait: Local<f32>,
) {
    let Ok(name) = std::env::var("SA_CUTSCENE") else { return };
    *wait += time.delta_secs();
    match *stage {
        0 if *wait > 2.0 => {
            overlay.mission_table = Some(std::env::var("SA_CUTGXT").unwrap_or_else(|_| "INTRO1".into()));
            // The scripts set the time of day before the cutscene (06:30 before PROLOG3).
            let now = sa.world.now_ms;
            sa.world.clock.set(now, 6, 30);
            overlay.fade(0.0, 0);
            cs.load(&name);
            *stage = 1;
        }
        1 if cs.has_loaded() => {
            cs.start();
            overlay.fade(1.0, 1);
            *stage = 2;
        }
        2 if cs.status == Status::Running && cs.has_finished() => {
            overlay.fade(0.0, 0);
            cs.clear();
            *stage = 3;
            *wait = 0.0;
        }
        3 if *wait > 0.5 => {
            overlay.fade(1.0, 1);
            *stage = 4;
        }
        _ => {}
    }
}

/// LoadCutsceneData (+ _loading / _postload): the .cut, the actors and their anims, the camera.
#[allow(clippy::too_many_arguments)]
fn load_cutscene(
    mut commands: Commands,
    mut cs: ResMut<Cutscene>,
    root: Res<GameRoot>,
    world: Res<WorldRes>,
    vdb: Option<Res<crate::vehicle::VehicleDb>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    if cs.status != Status::Loading {
        return;
    }
    let generic = vdb.map(|v| v.generic.clone()).unwrap_or_default();
    let mut ctx = LoadCtx { commands: &mut commands, meshes: &mut meshes, materials: &mut materials, images: &mut images, bindposes: &mut bindposes };
    match load(&mut cs, &root.0, &world, &generic, &mut ctx) {
        Ok(()) => cs.status = Status::Loaded,
        Err(e) => {
            error!("cutscene {}: {e:#}", cs.name);
            cs.status = Status::Idle;
        }
    }
}

struct LoadCtx<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    meshes: &'a mut Assets<Mesh>,
    materials: &'a mut Assets<StandardMaterial>,
    images: &'a mut Assets<Image>,
    bindposes: &'a mut Assets<SkinnedMeshInverseBindposes>,
}

fn load(
    cs: &mut Cutscene,
    root: &std::path::Path,
    world: &WorldRes,
    generic: &HashMap<String, (Handle<Image>, bool)>,
    ctx: &mut LoadCtx,
) -> Result<()> {
    let cuts = Img::open(&root.join("anim/cuts.img")).context("cuts.img")?;
    let models = Img::open(&root.join("models/cutscene.img")).context("cutscene.img")?;
    let name = cs.name.clone();
    let cut = cutscene::parse_cut(&String::from_utf8_lossy(cuts.get(&format!("{name}.cut")).context("no .cut")?));
    let mut anims = match cuts.get(&format!("{name}.ifp")) {
        Some(d) => ifp::parse(d)?,
        None => Vec::new(),
    };
    for a in &mut anims {
        if !cut.uncompress.iter().any(|u| u.eq_ignore_ascii_case(&a.name)) {
            for t in &mut a.tracks {
                compress_keys(&mut t.keys);
            }
        }
    }
    cs.splines = cuts.get(&format!("{name}.dat")).map(|d| cutscene::parse_dat(&String::from_utf8_lossy(d)));

    let mut objects = Vec::new();
    for m in &cut.models {
        // csplay is CJ rebuilt on csplay.dff (RebuildCutscenePlayer); otherwise an IDE model,
        // else a special CUTOBJ slot.
        if m.model == "csplay" {
            let cj = Img::open(&root.join("models/player.img")).and_then(|img| crate::clothes::build_cj(&world.0, &img, ctx.images, true));
            if let Err(e) = &cj {
                warn!("cutscene CJ: {e:#}");
            }
            if let Ok((clump, textures)) = cj {
                let PedVisual { model_root, bones, .. } = build_ped_visual(ctx.commands, ctx.meshes, ctx.materials, ctx.bindposes, &clump, &textures)?;
                ctx.commands.entity(model_root).insert(Visibility::Hidden);
                let mut by_name: HashMap<String, Entity> = clump.frames.iter().zip(&bones).map(|(f, &e)| (frame_key(&f.name), e)).collect();
                // The rebuilt clump's HAnim root frame is "Normal"; the anim's root sequence
                // ("root") binds to that node (id 0).
                if let Some(i) = clump.frames.iter().position(|f| f.hanim.as_ref().is_some_and(|h| h.node_id == 0)) {
                    by_name.insert("root".into(), bones[i]);
                }
                let anim = m.anims.last().and_then(|a| anims.iter().find(|x| x.name.eq_ignore_ascii_case(a)));
                let tracks = anim.map_or_else(Vec::new, |a| a.tracks.iter().filter_map(|t| Some((*by_name.get(&frame_key(&t.bone_name))?, t.keys.clone()))).filter(|t| !t.1.is_empty()).collect());
                let mut shoulders = HashMap::new();
                for tag in [31, 32, 21, 22, 301, 302] {
                    let Some(mut f) = clump.frames.iter().position(|f| f.hanim.as_ref().is_some_and(|h| h.node_id == tag)) else { continue };
                    let mut chain = vec![bones[f]];
                    while clump.frames[f].parent >= 0 {
                        f = clump.frames[f].parent as usize;
                        chain.push(bones[f]);
                    }
                    chain.reverse();
                    shoulders.insert(tag, chain);
                }
                objects.push(CutObject { root: model_root, tracks, shoulders });
                continue;
            }
        }
        let model = if m.model == "csplay" { crate::player::PED_MODEL.to_string() } else { m.model.clone() };
        let (dff_data, txd_data) = match world.0.file(&format!("{model}.dff")) {
            Some(d) => (d, world.0.file(&format!("{model}.txd"))),
            None => (models.get(&format!("{model}.dff")).with_context(|| format!("model {model}"))?, models.get(&format!("{model}.txd"))),
        };
        let clump = dff::parse(dff_data)?;
        let textures = txd_data.map(|d| load_txd(d, ctx.images)).unwrap_or_default();
        let (root_e, frames) = if clump.geometries.iter().any(|g| g.skin.is_some()) {
            let PedVisual { model_root, bones, .. } =
                build_ped_visual(ctx.commands, ctx.meshes, ctx.materials, ctx.bindposes, &clump, &textures)?;
            (model_root, bones)
        } else {
            let tex = |n: &str| textures.get(n).or_else(|| generic.get(n)).cloned();
            build_static(ctx.commands, ctx.meshes, ctx.materials, &clump, &tex)
        };
        ctx.commands.entity(root_e).insert(Visibility::Hidden);
        let by_name: HashMap<String, Entity> = clump.frames.iter().zip(&frames).map(|(f, &e)| (frame_key(&f.name), e)).collect();
        // The last anim of the line is the one the object keeps.
        let anim = m.anims.last().and_then(|a| anims.iter().find(|x| x.name.eq_ignore_ascii_case(a)));
        let mut tracks = Vec::new();
        if let Some(anim) = anim {
            for t in &anim.tracks {
                match by_name.get(&frame_key(&t.bone_name)) {
                    Some(&e) if !t.keys.is_empty() => tracks.push((e, t.keys.clone())),
                    Some(_) => {}
                    None => debug!("cutscene {name}: {} has no frame {}", m.model, t.bone_name),
                }
            }
        }
        objects.push(CutObject { root: root_e, tracks, shoulders: HashMap::new() });
    }
    info!("cutscene {name}: {} objects, {} anims, {} texts, camera {}", objects.len(), anims.len(), cut.texts.len(), cs.splines.is_some());
    cs.cut = cut;
    cs.objects = objects;
    cs.flyby = Flyby::default();
    cs.timer = 0.0;
    cs.cur_text = 0;
    cs.message = None;
    cs.fade_started = false;
    cs.skipped = false;
    Ok(())
}

/// MakePlayerSafe: no player control while a cutscene runs.
fn freeze_player(
    cs: Res<Cutscene>,
    control: Res<crate::script::PlayerControl>,
    mut walk: ResMut<crate::script::ScriptWalk>,
    mut sa: ResMut<SaPhys>,
    ped: Single<&Ped>,
) {
    if let Some((id, target, ms)) = walk.0 {
        let pos = sa.world.body(id).map(|b| b.phys.matrix.pos);
        let orient = sa.world.cam_info().orientation;
        let Some(pos) = pos else {
            walk.0 = None;
            return;
        };
        let d = (target - pos).truncate();
        if d.length() < 0.5 {
            walk.0 = None;
        } else if let Some(l) = sa.logic_mut::<PedLogic>(id) {
            // PlayerControlZelda: heading = RadianAngleBetweenPoints(0, 0, -lr, ud) - cam
            // orientation; pick the stick direction that gives the heading to the target.
            let want = (-d.x).atan2(d.y);
            let mag = if ms >= 6 { 128.0 } else { 60.0 };
            let mut best = (f32::MAX, 0.0, 0.0);
            for k in 0..360 {
                let a = (k as f32).to_radians();
                let (lr, ud) = (a.sin() * mag, a.cos() * mag);
                let h = sa_physics::pedtask::radian_angle_between_points(0.0, 0.0, -lr, ud) - orient;
                let mut e = (h - want).rem_euclid(std::f32::consts::TAU);
                if e > std::f32::consts::PI {
                    e = std::f32::consts::TAU - e;
                }
                if e < best.0 {
                    best = (e, lr, ud);
                }
            }
            if std::env::var("SA_SCMLOG").is_ok() && (pos.x * 10.0) as i32 % 7 == 0 {
                info!("walk: pos {pos:?} target {target:?} want {want:.2} cur {:.2} stick ({:.0},{:.0}) cam {orient:.2}", l.cur_rot, best.1, best.2);
            }
            l.tasks.pad = Default::default();
            l.tasks.pad.walk_lr = best.1;
            l.tasks.pad.walk_ud = best.2;
            l.tasks.pad.sprint = ms >= 7;
            return;
        }
    }
    if !cs.running() && control.0 {
        return;
    }
    if let Some(l) = sa.logic_mut::<PedLogic>(ped.sa) {
        l.tasks.pad = Default::default();
    }
}

/// `CCutsceneMgr::Update_overlay` + the flyby camera + the object anims, one frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_cutscene(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut cs: ResMut<Cutscene>,
    mut overlay: ResMut<Overlay>,
    audio: Option<Res<crate::audio::Audio>>,
    mut sources: ResMut<Assets<AudioSource>>,
    mut player_vis: Query<&mut Visibility, With<Ped>>,
    cam: Single<(&mut Transform, &mut Projection), With<OrbitCam>>,
    mut tfs: Query<(&mut Transform, &mut Visibility), (Without<OrbitCam>, Without<Ped>)>,
) {
    let cs = &mut *cs;
    if cs.clear_request {
        cs.clear_request = false;
        // DeleteCutsceneData.
        for e in cs.despawn.drain(..) {
            commands.entity(e).despawn();
        }
        overlay.widescreen = false;
        overlay.subtitle = None;
        for mut v in &mut player_vis {
            *v = Visibility::Inherited;
        }
        if !cs.running() {
            return;
        }
    }
    let dt = time.delta_secs();
    let (mut cam_tf, mut proj) = cam.into_inner();
    match cs.status {
        Status::Starting => {
            // StartCutscene + SetupCutsceneToStart: offset.z += 1 for the objects, the anims
            // start, the timer resets, the track plays, widescreen on, the player hidden.
            let base = g2b([cs.cut.offset[0], cs.cut.offset[1], cs.cut.offset[2] + 1.0]);
            for o in &cs.objects {
                if let Ok((mut tf, mut vis)) = tfs.get_mut(o.root) {
                    tf.translation = base;
                    *vis = Visibility::Inherited;
                }
            }
            for mut v in &mut player_vis {
                *v = Visibility::Hidden;
            }
            if let (Some(audio), Some(id)) = (audio.as_deref(), audio_track(&cs.name)) {
                cs.track = crate::audio::play_track(&mut commands, audio, &mut sources, id, 0.0);
            }
            overlay.widescreen = cs.splines.is_some();
            cs.timer = 0.0;
            cs.status = Status::Running;
        }
        Status::Running => {
            cs.timer += dt;
        }
        _ => return,
    }
    let time_ms = (cs.timer * 1000.0) as u32;

    // One subtitle per frame at most (strict <); AddMessageJumpQ replaces the current one.
    if let Some(t) = cs.cut.texts.get(cs.cur_text) {
        if t.start_ms < time_ms {
            cs.message = Some((t.key.clone(), time_ms as f32 + t.duration_ms as f32));
            cs.cur_text += 1;
        }
    }
    if cs.message.as_ref().is_some_and(|m| m.1 <= time_ms as f32) {
        cs.message = None;
    }
    overlay.subtitle = cs.message.as_ref().map(|m| m.0.clone());

    // The flyby camera.
    if let Some(sp) = cs.splines.as_ref() {
        let f = cs.flyby.step(sp, cs.cut.offset, dt * 1000.0);
        let src = Vec3::from(f.source);
        let front = (Vec3::from(f.target) - src).normalize_or(Vec3::Y);
        let a = f.roll.to_radians() + FRAC_PI_2;
        let up0 = Vec3::new(a.cos(), 0.0, a.sin());
        let up = front.cross(up0.cross(front)).normalize_or(Vec3::Z);
        cam_tf.translation = g2b(src.to_array());
        *cam_tf = cam_tf.looking_to(g2b(front.to_array()), g2b(up.to_array()));
        if let Projection::Perspective(p) = &mut *proj {
            // SA's FOV is horizontal.
            p.fov = 2.0 * ((f.fov * 0.5).to_radians().tan() / p.aspect_ratio).atan();
            p.near = 0.1;
        }
        let finish = Flyby::finish_ms(sp);
        if time_ms + 1000 > finish && !cs.fade_started {
            cs.fade_started = true;
            overlay.fade(1.0, 0);
        }
        // IsCutsceneSkipButtonBeingPressed → FinishCutscene.
        // SA_CUTSKIP=1 (testing): skip every cutscene at once.
        let skip = keys.any_just_pressed([KeyCode::Space, KeyCode::Enter, KeyCode::NumpadEnter]) || mouse.just_pressed(MouseButton::Left) || std::env::var("SA_CUTSKIP").is_ok();
        if skip && cs.flyby.along < 1.0 {
            cs.skipped = true;
            cs.timer = finish as f32 * 0.001;
            cs.flyby.finish(sp);
        }
    }

    // The object anims on the cutscene time base.
    let t = cs.timer;
    for o in &cs.objects {
        for (e, keys) in &o.tracks {
            let Ok((mut tf, _)) = tfs.get_mut(*e) else { continue };
            let (q, p) = sample(keys, t);
            tf.rotation = q;
            if let Some(p) = p {
                tf.translation = p;
            }
        }
        if o.shoulders.is_empty() {
            continue;
        }
        // CCutsceneObject::PreRender: ShoulderBoneRotation for csplay.
        let chain_world = |chain: &[Entity]| -> Option<Mat4> {
            let mut m = Mat4::IDENTITY;
            for &e in chain {
                m *= tfs.get(e).ok()?.0.to_matrix();
            }
            Some(m)
        };
        let world = |tag: i32| o.shoulders.get(&tag).and_then(|c| chain_world(c));
        let pads: Vec<(Entity, Quat, Vec3)> = crate::clothes::shoulder_pads(world)
            .into_iter()
            .filter_map(|(tag, w)| {
                let chain = o.shoulders.get(&tag)?;
                let parent = chain_world(&chain[..chain.len() - 1])?;
                let (_, r, tr) = (parent.inverse() * w).to_scale_rotation_translation();
                Some((chain[chain.len() - 1], r, tr))
            })
            .collect();
        for (e, r, tr) in pads {
            if let Ok((mut tf, _)) = tfs.get_mut(e) {
                tf.rotation = r.normalize();
                tf.translation = tr;
            }
        }
    }
}
