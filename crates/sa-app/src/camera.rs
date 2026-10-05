//! The on-foot player cameras of SA on PC (`CCam`): `Process_FollowPed_SA` (mode 4) and
//! `Process_AimWeapon` (mode 53), with `CCamera::StartTransition`'s eased switch between
//! them. Angles are SA's: `beta` is the heading of the camera's Front (`Front.xy =
//! (-cos β, -sin β)`), `alpha` its pitch. All maths in GTA space.
//!
//! The car camera is still the simple orbit in `player.rs`.
//!
//! Not in SA: a first-person view on foot (the 4th step of the Home zoom cycle): the eye at the
//! head bone, the ped turned to the view, its head hidden.

use std::f32::consts::{FRAC_PI_2, PI};

use bevy::{input::mouse::AccumulatedMouseMotion, prelude::*, transform::TransformSystems};
use sa_physics::ped::PedLogic;

use crate::{
    player::{CamFollow, Mode, MouseLock, OrbitCam, Ped},
    saphys::{SaPhys, SaPhysExt, SaSync},
    world::{b2g, g2b},
};

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SaCam>().add_systems(
            PostUpdate,
            sa_camera
                .run_if(resource_equals(Mode::Walk))
                .after(SaSync)
                .before(TransformSystems::Propagate),
        );
    }
}

/// `CCamera::m_f3rdPersonCHairMultX/Y` (0xB6EC14 / 0xB6EC10).
pub const CHAIR_X: f32 = 0.53;
/// `CCamera::m_fMouseAccelHorzntl` default (0x573C0A) and `m_fMouseAccelVertical` (0x5BC7B4).
const HACC: f32 = 0.0025;
const VACC: f32 = 0.0015;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CamMode {
    #[default]
    FollowPed = 4,
    /// MODE_ROCKETLAUNCHER (and 51 the heat-seeker variant): `Process_Rocket` 0x511B50.
    Rocket = 8,
    AimWeapon = 53,
}

/// What the weapon code requests for this frame (`SetNewPlayerWeaponMode`, cleared by
/// `CamControl` every frame unless re-requested).
#[derive(Clone, Copy, Debug)]
pub struct AimRequest {
    pub weapon: u32,
    /// weapon.dat flag 0x2 (aim with arm): standing one-handed weapons don't turn the ped.
    pub aim_with_arm: bool,
    pub ducking: bool,
    /// The requested mode (53 aim weapon, 8 / 51 rocket launcher).
    pub mode: u8,
}

#[derive(Clone, Copy)]
struct Transition {
    start: f32,
    /// Seconds.
    dur: f32,
    /// Source ease fractions `+0xC10 / +0xC14`.
    c10: f32,
    c14: f32,
    source: Vec3,
    front: Vec3,
    up: Vec3,
    fov: f32,
}

/// The active `CCam` plus the `CCamera` state the port needs.
#[derive(Resource)]
pub struct SaCam {
    pub mode: CamMode,
    pub request: Option<AimRequest>,
    pub alpha: f32,
    pub beta: f32,
    /// Horizontal field of view, degrees.
    pub fov: f32,
    pub source: Vec3,
    pub front: Vec3,
    pub up: Vec3,
    pub aspect: f32,
    /// Ped zoom ("change camera" key, Home here: V spawns cars): 1, 2 (default), 3, and 4 =
    /// first person (not in SA).
    pub zoom: u8,
    zoom_smooth: f32,
    duck_z: f32,
    /// Follow-cam duck offset (TheCamera+0xC54).
    duck_follow: f32,
    lag: Option<Vec3>,
    col_fraction: f32,
    prev_target: Option<Vec3>,
    transition: Option<Transition>,
    initialised: bool,
    /// `cx`: the crosshair's horizontal angle (AimWeapon).
    pub cross_x: f32,
}

impl Default for SaCam {
    fn default() -> Self {
        Self {
            mode: CamMode::FollowPed,
            request: None,
            alpha: -0.05,
            beta: 0.0,
            fov: 70.0,
            source: Vec3::ZERO,
            front: Vec3::Y,
            up: Vec3::Z,
            aspect: 16.0 / 9.0,
            zoom: 2,
            zoom_smooth: 1.5,
            duck_z: 0.0,
            duck_follow: 0.0,
            lag: None,
            col_fraction: 1.0,
            prev_target: None,
            transition: None,
            initialised: false,
            cross_x: 0.0,
        }
    }
}

impl SaCam {
    /// The first-person view is active.
    pub fn first_person(&self) -> bool {
        self.zoom == 4 && self.mode != CamMode::Rocket
    }

    /// `TheCamera+0x58`: a mode transition is running.
    pub fn in_transition(&self) -> bool {
        self.transition.is_some()
    }

    /// Re-read alpha/beta from the orbit camera the next time the SA camera runs.
    pub fn reset_from_orbit(&mut self) {
        self.initialised = false;
        self.transition = None;
        self.lag = None;
        self.col_fraction = 1.0;
    }
}

fn front_of(alpha: f32, beta: f32) -> Vec3 {
    Vec3::new(-beta.cos() * alpha.cos(), -beta.sin() * alpha.cos(), alpha.sin())
}

/// `0.5 - 0.5 cos(πx)`.
fn ease(x: f32) -> f32 {
    0.5 - 0.5 * (PI * x.clamp(0.0, 1.0)).cos()
}

/// Sphere-sweep camera collision (0x520190), approximated by a ray kept 0.3 m off walls:
/// snaps in at once, recovers at most 5 % of the distance per frame.
fn collide(cam: &mut SaCam, sa: &mut SaPhys, pivot: Vec3, source: Vec3, ped_e: Entity, ts: f32, rate: f32) -> Vec3 {
    let d = source - pivot;
    let len = d.length();
    if len < 1e-4 {
        return source;
    }
    let t = sa
        .cast_ray(g2b(pivot.to_array()), g2b(d.to_array()), len + 0.3, false, Some(ped_e))
        .map(|h| ((h.toi - 0.3) / len).clamp(0.0, 1.0))
        .unwrap_or(1.0);
    if t < cam.col_fraction {
        cam.col_fraction = t;
    } else {
        cam.col_fraction += ((t - cam.col_fraction) * ts * rate).min(0.05);
        cam.col_fraction = cam.col_fraction.min(t);
    }
    pivot + d * cam.col_fraction
}

#[allow(clippy::too_many_arguments)]
fn sa_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    lock: Res<MouseLock>,
    motion: Res<AccumulatedMouseMotion>,
    mut sa: ResMut<SaPhys>,
    mut cam: ResMut<SaCam>,
    window: Single<&Window>,
    target: Single<(Entity, &Transform, &Ped), With<CamFollow>>,
    view: Single<(&mut Transform, &mut Projection, &mut OrbitCam), (With<Camera3d>, Without<CamFollow>)>,
) {
    let (ped_e, ped_tf, ped) = *target;
    let (mut tf, mut proj, mut orbit) = view.into_inner();
    let cam = &mut *cam;
    let ts = (time.delta_secs() * 50.0).max(1e-4);
    let now = time.elapsed_secs();
    cam.aspect = window.width().max(1.0) / window.height().max(1.0);

    if !cam.initialised {
        // Continue from wherever the orbit/fly camera looked.
        cam.beta = orbit.yaw - FRAC_PI_2;
        cam.alpha = orbit.pitch;
        cam.initialised = true;
        // SA_FP=1: start in first person (debug).
        if std::env::var("SA_FP").is_ok() {
            cam.zoom = 4;
        }
    }

    // SetNewPlayerWeaponMode from the player's weapon task (re-requested every frame).
    cam.request = sa.logic::<PedLogic>(ped.sa).and_then(|l| {
        (l.tasks.cam_request != 0).then(|| {
            let w = l.tasks.active_weapon().ty;
            let arm = l.tasks.info_of(w).is_some_and(|i| i.has(sa_physics::weapon::wf::AIMWITHARM));
            AimRequest { weapon: w, aim_with_arm: arm, ducking: l.tasks.ducking_for_camera(), mode: l.tasks.cam_request }
        })
    });

    // Mode request → transition (StartTransition: alpha, beta and FOV carry over).
    let want = match cam.request.map(|r| r.mode) {
        Some(8 | 51) => CamMode::Rocket,
        Some(_) => CamMode::AimWeapon,
        None => CamMode::FollowPed,
    };
    // Into and out of the 1st-person rocket camera: a jump cut.
    if want != cam.mode && (want == CamMode::Rocket || cam.mode == CamMode::Rocket) {
        cam.mode = want;
        cam.transition = None;
        cam.prev_target = None;
        if want == CamMode::Rocket {
            cam.alpha = 0.0;
        }
    }
    if want != cam.mode {
        let (dur, c10, c14) = if want == CamMode::AimWeapon { (0.4, 0.0, 1.0) } else { (0.35, 0.1, 0.9) };
        cam.transition = Some(Transition {
            start: now,
            dur,
            c10,
            c14,
            source: cam.source,
            front: cam.front,
            up: cam.up,
            fov: cam.fov,
        });
        cam.mode = want;
        cam.prev_target = None;
        if want == CamMode::AimWeapon {
            cam.duck_z = if cam.request.is_some_and(|r| r.ducking) { -0.35 } else { 0.0 };
        }
    }
    let transitioning = cam.transition.is_some_and(|t| now - t.start < t.dur);

    // Mouse: per-frame counts. MY > 0 = mouse pushed away (the default "invert" setting).
    let (mx, my) = if lock.0 { (motion.delta.x, -motion.delta.y) } else { (0.0, 0.0) };
    let k = cam.fov * 0.0125;

    let ped_pos = Vec3::from(b2g(ped_tf.translation));
    let ped_right = Vec3::from(b2g(ped_tf.rotation * Vec3::X));
    let (source, pivot) = match cam.mode {
        CamMode::FollowPed => {
            // Zoom key cycles 1 → 2 → 3 (TheCamera+0xC8); extra distance followed at ts·0.12.
            if keys.just_pressed(KeyCode::Home) {
                cam.zoom = cam.zoom % 4 + 1;
            }
            if cam.zoom == 4 {
                // First person: the view is free, the ped follows it.
                if !transitioning {
                    cam.fov += (75.0 - cam.fov).clamp(-ts, ts);
                }
                cam.beta += mx * -2.5 * k * HACC;
                cam.alpha += my * 2.5 * k * HACC;
                cam.alpha = cam.alpha.clamp(-1.4, 1.3);
                cam.front = front_of(cam.alpha, cam.beta);
                let eye = first_person_eye(&mut sa, ped.sa, ped_pos, cam.front, cam.beta);
                cam.lag = None;
                (eye, eye)
            } else {
            let extra = [-0.55, 1.5, 3.6][cam.zoom as usize - 1];
            let step = ts * 0.12;
            cam.zoom_smooth += (extra - cam.zoom_smooth).clamp(-step, step);
            if !transitioning {
                cam.fov += (70.0 - cam.fov).clamp(-ts, ts);
            }
            cam.beta += mx * -2.5 * k * HACC;
            cam.alpha += my * 2.5 * k * HACC;
            cam.alpha = cam.alpha.clamp(-1.483_529_9, 0.785_398_2);
            cam.front = front_of(cam.alpha, cam.beta);
            // Raw ped position with the mouse (Using3rdPersonMouseCam), +0.6.
            let t = ped_pos + Vec3::new(0.0, 0.0, 0.6);
            let ideal = cam.zoom_smooth + 2.0;
            let mut dist = ideal;
            if let Some(lag) = cam.lag {
                let trail = (t - lag).length();
                if trail < ideal && ideal > 2.0 {
                    dist = trail.max(2.0);
                }
            }
            let mut src = t - cam.front * dist;
            cam.lag = Some(src);
            // 0x50CFA0: -0.7 crouched still, -0.2 crouch-walking, eased at 0.1·ts.
            let (ducking, moving) = sa
                .logic::<PedLogic>(ped.sa)
                .map(|l| (l.tasks.ducking_for_camera(), sa.world.body(ped.sa).is_some_and(|b| b.phys.move_speed.length_squared() > 1e-6)))
                .unwrap_or((false, false));
            let want = if !ducking { 0.0 } else if moving { -0.2 } else { -0.7 };
            cam.duck_follow += (want - cam.duck_follow) * (ts * 0.1).min(1.0);
            src.z += cam.duck_follow;
            let mut t = t;
            t.z += cam.duck_follow;
            (collide(cam, &mut sa, t, src, ped_e, ts, 0.2), t)
            }
        }
        CamMode::Rocket => {
            // Process_Rocket (0x511B50): FOV 70, eye at the head bone + 0.1 z.
            cam.fov = 70.0;
            cam.beta += mx * -3.0 * k * HACC;
            cam.alpha += my * 4.0 * k * VACC;
            cam.alpha = cam.alpha.clamp(-1.562_069_8, 1.047_197_6);
            cam.front = front_of(cam.alpha, cam.beta);
            let head = sa.logic::<PedLogic>(ped.sa).and_then(|l| {
                let c = l.clump.as_deref()?;
                let f = c.frame_of_tag(5)?;
                let m = sa.world.body(ped.sa)?.phys.matrix;
                Some(m.transform(c.ltm(f).w_axis.truncate()))
            });
            let eye = head.unwrap_or(ped_pos + Vec3::new(0.0, 0.0, 0.6)) + Vec3::new(0.0, 0.0, 0.1);
            // The ped faces where the launcher points.
            let h = cam.beta + FRAC_PI_2;
            if let Some(logic) = sa.logic_mut::<PedLogic>(ped.sa) {
                logic.cur_rot = h;
                logic.aim_rot = h;
                logic.tasks.pd.look_pitch = -cam.alpha;
            }
            (eye, eye)
        }
        CamMode::AimWeapon if cam.zoom == 4 => {
            // First-person aiming: the same eye; the ped and its gun follow the view.
            let req = cam.request.unwrap();
            let fov_target = match req.weapon {
                30 | 31 => 50.0,
                33 => 35.0,
                _ => 70.0,
            };
            if !transitioning {
                cam.fov += (fov_target - cam.fov).clamp(-ts, ts);
            }
            let tn = (cam.fov * 0.5).to_radians().tan();
            cam.cross_x = (2.0 * (CHAIR_X - 0.5) * tn).atan();
            cam.beta += mx * -2.5 * k * HACC;
            cam.alpha += my * 4.0 * k * VACC;
            cam.alpha = cam.alpha.clamp(-1.4, 1.3);
            cam.front = front_of(cam.alpha, cam.beta);
            let eye = first_person_eye(&mut sa, ped.sa, ped_pos, cam.front, cam.beta);
            let cy = ((1.0 / cam.aspect) * 2.0 * (0.5 - 0.4) * tn).atan();
            if let Some(logic) = sa.logic_mut::<PedLogic>(ped.sa) {
                logic.tasks.pd.look_pitch = -(cy + cam.alpha);
            }
            (eye, eye)
        }
        CamMode::AimWeapon => {
            let req = cam.request.unwrap();
            let fov_target = match req.weapon {
                30 | 31 => 50.0,
                33 => 35.0,
                _ => 70.0,
            };
            if !transitioning {
                cam.fov += (fov_target - cam.fov).clamp(-ts, ts);
            }
            let tn = (cam.fov * 0.5).to_radians().tan();
            cam.cross_x = (2.0 * (CHAIR_X - 0.5) * tn).atan();
            if mx != 0.0 || my != 0.0 {
                cam.beta += mx * -2.5 * k * HACC;
                cam.alpha += my * 4.0 * k * VACC;
            }
            cam.alpha = cam.alpha.clamp(-1.553_343_1, 0.785_398_2);
            // Target smoothing with the use-gun task: p = 0.9^ts.
            let p = 0.9f32.powf(ts);
            let mut t = match cam.prev_target {
                Some(prev) => prev * p + ped_pos * (1.0 - p),
                None => ped_pos,
            };
            cam.prev_target = Some(t);
            t.z = ped_pos.z + 0.5;
            if cam.fov < 70.0 {
                t.z += ((70.0 - cam.fov) / 20.0).min(1.0) * 0.1;
            }
            let mut lat = 0.2;
            if cam.fov < 70.0 {
                lat += ((70.0 - cam.fov) / 35.0).min(1.0) * 0.1;
            }
            let r = cam.front.cross(cam.up).normalize_or_zero();
            let c = r.dot(ped_right).clamp(0.0, 1.0);
            t += r * lat * (1.0 - c.acos() * (2.0 / PI));
            let ca = if cam.alpha <= 0.0 { cam.alpha.cos() } else { cam.alpha.min(FRAC_PI_2).cos() };
            let dist = 1.0 + 1.6 * ca;
            cam.front = front_of(cam.alpha, cam.beta);
            let mut src = t - cam.front * dist;
            let duck = if req.ducking { -0.35 } else { 0.0 };
            cam.duck_z += (duck - cam.duck_z) * ts * 0.13;
            src.z += cam.duck_z;
            t.z += cam.duck_z;
            let src = collide(cam, &mut sa, t, src, ped_e, ts, 0.2);

            // Two-handed weapons (and any weapon crouched) turn the ped to the crosshair heading
            // and get the look pitch (Find3rdPersonQuickAimPitch) for the torso IK.
            if !req.aim_with_arm || req.ducking {
                let h = (-cam.front.x).atan2(cam.front.y) - cam.cross_x;
                let cy = ((1.0 / cam.aspect) * 2.0 * (0.5 - 0.4) * (cam.fov * 0.5).to_radians().tan()).atan();
                let pitch = -(cy + cam.alpha);
                if let Some(logic) = sa.logic_mut::<PedLogic>(ped.sa) {
                    logic.cur_rot = h - 0.05;
                    logic.aim_rot = h - 0.05;
                    logic.tasks.pd.look_pitch = pitch;
                }
            }
            (src, t)
        }
    };
    let _ = pivot;
    cam.up = cam.front.cross(Vec3::Z).cross(cam.front).normalize_or(Vec3::Z);
    cam.source = source;

    // Transition blend from the snapshot.
    let (mut src, mut front, mut up, mut fov) = (cam.source, cam.front, cam.up, cam.fov);
    if let Some(tr) = cam.transition {
        let f = ((now - tr.start) / tr.dur).min(1.0);
        if f >= 1.0 {
            cam.transition = None;
        } else {
            let e = if f <= tr.c10 { 0.0 } else { ease((f - tr.c10) / tr.c14) };
            src = tr.source + (src - tr.source) * e;
            front = tr.front.lerp(front, e).normalize_or(front);
            up = tr.up.lerp(up, e).normalize_or(up);
            fov = tr.fov + (fov - tr.fov) * e;
        }
    }
    cam.source = src;
    cam.front = front;
    cam.up = up;

    tf.translation = g2b(src.to_array());
    *tf = tf.looking_to(g2b(front.to_array()), g2b(up.to_array()));
    if let Projection::Perspective(p) = &mut *proj {
        // SA's FOV is horizontal.
        p.fov = 2.0 * ((fov * 0.5).to_radians().tan() / cam.aspect).atan();
    }
    // Keep the orbit angles in sync (movement direction, car camera).
    orbit.yaw = cam.beta + FRAC_PI_2;
    orbit.pitch = cam.alpha;

    if std::env::var("SA_CAMLOG").is_ok() && (now * 2.0).fract() < 0.02 {
        let pp = sa.logic::<PedLogic>(ped.sa).map(|l| (l.tasks.pd.look_pitch, l.tasks.ducking, l.tasks.ik.torso_pitch));
        info!("cam {:?} alpha {:.3} beta {:.3} src {:?} front {:?} ped {:?} {:?}", cam.mode, cam.alpha, cam.beta, src, front, ped_pos, pp);
    }
    sa.world.camera_up = up;
    sa.world.camera_fov = fov;
    sa.world.camera_aspect = cam.aspect;
    sa.world.camera_mode = cam.mode as u8;
}

/// First person: the eye just in front of the head bone; the ped faces the view's heading.
fn first_person_eye(sa: &mut SaPhys, ped: sa_physics::world::EntityId, ped_pos: Vec3, front: Vec3, beta: f32) -> Vec3 {
    let head = sa.logic::<PedLogic>(ped).and_then(|l| {
        let c = l.clump.as_deref()?;
        let f = c.frame_of_tag(5)?;
        let m = sa.world.body(ped)?.phys.matrix;
        Some(m.transform(c.ltm(f).w_axis.truncate()))
    });
    let flat = Vec3::new(front.x, front.y, 0.0).normalize_or_zero();
    let eye = head.unwrap_or(ped_pos + Vec3::new(0.0, 0.0, 0.65)) + Vec3::new(0.0, 0.0, 0.08) + flat * 0.12;
    let h = beta + FRAC_PI_2;
    if let Some(logic) = sa.logic_mut::<PedLogic>(ped) {
        // On foot (not in a vehicle / swimming): the body turns with the view.
        if logic.vehicle.is_none() {
            logic.aim_rot = h;
        }
    }
    eye
}
