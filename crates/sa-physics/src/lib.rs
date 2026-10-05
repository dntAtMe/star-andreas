//! A port of GTA San Andreas' rigid-body physics, from the PC 1.0 executable.
//!
//! Everything works in the game's own space and units: Z up, world units,
//! speeds per 1/50 s frame, and a timestep `ts` measured in such frames.

pub mod anim;
pub mod automobile;
pub mod bike;
pub mod boat;
pub mod bullet;
pub mod clock;
#[cfg(feature = "bevy")]
pub mod bevy_api;
pub mod collision;
pub mod coronas;
pub mod colpoint;
pub mod damage;
pub mod duck;
pub mod effects;
pub mod explosion;
pub mod fire;
pub mod fxhelpers;
pub mod gun;
pub mod ik;
pub mod incar;
pub mod pair;
pub mod ped;
pub mod peddamage;
pub mod pedtask;
pub mod shadows;
pub mod npc;
pub mod objects;
pub mod paths;
pub mod physical;
pub mod population;
pub mod projectile;
pub mod surface;
pub mod timecycle;
pub mod vehicle_lights;
pub mod traffic;
pub mod water;
pub mod melee;
pub mod swim;
pub mod weapon;
pub mod weather;
pub mod world;

pub use glam::Vec3;

/// Per-step globals the original keeps in statics.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    /// `CTimer::ms_fTimeStep`, in 1/50 s frames.
    pub ts: f32,
    /// 0xB7CD72: false during the first ProcessCollision pass of a frame.
    pub later_collision_pass: bool,
    /// 0xB7CD6C: false only during the first ProcessShift pass.
    pub keep_going_after_hit: bool,
    /// `CWeather::WetRoads` (tyre grip on wet surfaces).
    pub wet_roads: f32,
    /// `CTimer::m_snTimeInMilliseconds`.
    pub now_ms: u32,
    /// The active camera (TheCamera), for the player's tasks.
    pub cam: CamInfo,
    /// `CTimer::m_FrameCounter`.
    pub frame: u32,
}

/// What the ped tasks read from `TheCamera`.
#[derive(Debug, Clone, Copy)]
pub struct CamInfo {
    pub pos: Vec3,
    pub front: Vec3,
    pub up: Vec3,
    /// Horizontal FOV, degrees.
    pub fov: f32,
    pub aspect: f32,
    /// Active `CCam` mode (4 follow ped, 53 aim weapon).
    pub mode: u8,
    /// `TheCamera.m_fOrientation`: atan2(front.x, front.y).
    pub orientation: f32,
}

impl Default for CamInfo {
    fn default() -> Self {
        Self { pos: Vec3::ZERO, front: Vec3::Y, up: Vec3::Z, fov: 70.0, aspect: 16.0 / 9.0, mode: 4, orientation: 0.0 }
    }
}

impl CamInfo {
    /// `CCamera::Find3rdPersonCamTargetVector` (0x514970): the ray through the crosshair
    /// (0.53, 0.4), its start moved to the point nearest `src`. Returns (start, end).
    pub fn target_vector(&self, range: f32, src: Vec3) -> (Vec3, Vec3) {
        const CHAIR_X: f32 = 0.53;
        const CHAIR_Y: f32 = 0.4;
        let t = (self.fov * 0.5).to_radians().tan();
        let sx = 2.0 * (CHAIR_X - 0.5) * t;
        let sy = 2.0 * (0.5 - CHAIR_Y) * t / self.aspect;
        let dir = (self.front + self.up * sy + self.front.cross(self.up) * sx).normalize_or(self.front);
        let start = self.pos + dir * (src - self.pos).dot(dir);
        (start, start + dir * range)
    }

    /// `CCamera::Find3rdPersonQuickAimPitch` (0x50AD40) without the alpha term: the pitch of
    /// the crosshair ray above the camera's own (callers add the camera alpha).
    pub fn crosshair_pitch_offset(&self) -> f32 {
        ((1.0 / self.aspect) * 2.0 * (0.5 - 0.4) * (self.fov * 0.5).to_radians().tan()).atan()
    }
}

impl Ctx {
    pub fn new(ts: f32) -> Self {
        Self {
            ts,
            later_collision_pass: false,
            keep_going_after_hit: true,
            wet_roads: 0.0,
            now_ms: 0,
            cam: CamInfo::default(),
            frame: 0,
        }
    }
}

/// Timestep (in frames) for a duration in seconds, clamped like CTimer (0.01 ..= 3.0).
pub fn timestep_for(seconds: f32) -> f32 {
    (seconds * 50.0).clamp(0.01, 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use physical::*;

    fn object() -> Physical {
        let mut p = Physical::new(EntityType::Object, Matrix::IDENTITY);
        p.mass = 100.0;
        p.turn_mass = 100.0;
        p.air_resistance = 0.99;
        p
    }

    #[test]
    fn gravity_then_air_resistance() {
        let mut p = object();
        let ctx = Ctx::new(1.0);
        p.process_control(&ctx);
        // v.z = -0.008, then *= 0.99^1
        assert!((p.move_speed.z - (-0.008 * 0.99)).abs() < 1e-9);
        p.apply_speed(ctx.ts);
        assert!((p.matrix.pos.z - p.move_speed.z).abs() < 1e-9);
    }

    #[test]
    fn friction_accumulators_apply_next_frame() {
        let mut p = object();
        p.flags &= !pf::APPLY_GRAVITY;
        p.friction_move = Vec3::new(0.5, 0.0, 0.0);
        p.process_control(&Ctx::new(1.0));
        assert!((p.move_speed.x - 0.5 * 0.99).abs() < 1e-6);
        assert_eq!(p.friction_move, Vec3::ZERO);
    }

    #[test]
    fn static_collision_reflects_with_elasticity() {
        let mut p = object();
        p.elasticity = 0.5;
        p.flags |= pf::DISABLE_TURN_FORCE;
        p.move_speed = Vec3::new(0.0, 0.0, -1.0);
        let cp = colpoint::ColPoint { point: Vec3::new(0.0, 0.0, -1.0), normal: Vec3::Z, ..Default::default() };
        // No-turn bodies take the translation-only path: velocity along n cancelled (no bounce).
        let imp = p.apply_collision_static(&Ctx::new(1.0), &cp).unwrap();
        assert!((imp - 100.0).abs() < 1e-4);
        assert!(p.move_speed.z.abs() < 1e-6);

        let mut q = object();
        q.elasticity = 0.5;
        q.move_speed = Vec3::new(0.0, 0.0, -1.0);
        let cp = colpoint::ColPoint { point: Vec3::ZERO, normal: Vec3::Z, ..Default::default() };
        q.apply_collision_static(&Ctx::new(1.0), &cp).unwrap();
        // Contact through the COM: v' = -e*v.
        assert!((q.move_speed.z - 0.5).abs() < 1e-5);
    }

    #[test]
    fn turn_then_reorthogonalise_stays_orthonormal() {
        let mut p = object();
        p.turn_speed = Vec3::new(0.0, 0.0, 0.1);
        for _ in 0..100 {
            p.apply_speed(1.0);
            p.matrix.reorthogonalise();
        }
        let m = p.matrix;
        assert!((m.right.length() - 1.0).abs() < 1e-5);
        assert!(m.right.dot(m.fwd).abs() < 1e-5);
        assert!((m.up - Vec3::Z).length() < 1e-4);
    }
}
