//! A port of GTA San Andreas' rigid-body physics, from the PC 1.0 executable.
//!
//! Everything works in the game's own space and units: Z up, world units,
//! speeds per 1/50 s frame, and a timestep `ts` measured in such frames.

pub mod collision;
pub mod colpoint;
pub mod pair;
pub mod physical;

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
}

impl Ctx {
    pub fn new(ts: f32) -> Self {
        Self { ts, later_collision_pass: false, keep_going_after_hit: true }
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
