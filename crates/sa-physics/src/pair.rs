//! Physical-vs-physical responses: `ApplyCollision(CPhysical*, ...)` (0x548680),
//! `ApplySoftCollision(CPhysical*, ...)` (0x54A2C0) and
//! `ApplyFriction(CPhysical*, float, CColPoint&)` (0x545980).
//!
//! The "B is a static physical that may get woken up" preamble of 0x548680 is
//! handled by the caller (the world), which only calls in here with a movable B.

use glam::Vec3;

use crate::{
    Ctx,
    colpoint::ColPoint,
    physical::{EntityType, Physical, Status, ef, pf},
};

/// Extra relationships the original reads from ped / player state.
#[derive(Debug, Clone, Copy, Default)]
pub struct PairInfo {
    /// A is a ped standing on B (ped+0x568 == B).
    pub a_stands_on_b: bool,
    /// B is a ped standing on A.
    pub b_stands_on_a: bool,
    /// A is the player's ped.
    pub a_is_player_ped: bool,
}

struct Factors {
    sa: f32,
    sb: f32,
    stand_on_a: bool,
    stand_on_b: bool,
    immovable_b: bool,
}

/// Mass scale factors. `move_mask` is 0x20 for ApplyCollision and 0x60 for the soft variant.
fn factors(a: &Physical, b: &Physical, info: PairInfo, move_mask: u32) -> Factors {
    let sa;
    let mut sb = 1.0;
    let mut stand_on_a = false;
    let mut stand_on_b = false;
    if b.has(pf::DISABLE_TURN_FORCE) && a.flags & move_mask == 0 {
        sa = 10.0;
        if b.kind == EntityType::Ped && info.b_stands_on_a {
            stand_on_a = true;
        }
    } else {
        sa = if a.has(pf::HEAVY) { 2.0 } else { 1.0 };
    }
    if a.has(pf::DISABLE_TURN_FORCE) {
        if info.a_is_player_ped
            && b.is_vehicle()
            && (matches!(b.status, Status::Abandoned | Status::Wrecked) || a.has_e(ef::HAS_HIT_WALL))
        {
            sb = 1.0 / (1.0 + (b.mass - 2000.0).max(0.0) * 0.0002);
        } else if b.flags & move_mask == 0 {
            sb = 10.0;
        }
        if a.kind == EntityType::Ped && info.a_stands_on_b {
            stand_on_b = true;
            sb = 10.0;
        }
    } else if let Some(towed) = a.vehicle.and_then(|v| v.towed_mass) {
        // B's factor depends on A's trailer (as in the original).
        sb = (towed + a.mass) / a.mass;
    } else {
        sb = if b.has(pf::HEAVY) { 2.0 } else { 1.0 };
    }
    // Attachment is not modelled; immovable = collision-force-disabled and not collide-as-static.
    let immovable_b = b.has(pf::DISABLE_COLLISION_FORCE) && !b.has(pf::COLLIDE_AS_STATIC);
    if immovable_b {
        stand_on_b = false;
    }
    Factors { sa, sb, stand_on_a, stand_on_b, immovable_b }
}

fn com_of(p: &Physical) -> Vec3 {
    if p.has(pf::INFINITE_MASS) { Vec3::ZERO } else { p.matrix.rotate(p.com) }
}

/// Effective mass scaled by `s`; rotation-only when the body can't translate.
fn eff_scaled(p: &Physical, r: Vec3, n: Vec3, s: f32, rot_only: bool) -> f32 {
    let x = r.cross(n).length_squared() / (s * p.turn_mass);
    if rot_only { 1.0 / x } else { 1.0 / (x + 1.0 / (s * p.mass)) }
}

fn approach(p: &Physical, r: Vec3, n: Vec3) -> f32 {
    let v = if p.has(pf::DISABLE_TURN_FORCE) { p.move_speed } else { p.get_speed(r) };
    v.dot(n)
}

fn new_v(p: &Physical, v: f32, vcom: f32, e: f32) -> f32 {
    if p.has_e(ef::HAS_HIT_WALL) { vcom } else { vcom - e * (v - vcom) }
}

/// 0x548680 (dynamic part) and, with `soft`, 0x54A2C0. Returns (impulse on A, impulse on B).
pub fn apply_collision(
    ctx: &Ctx,
    a: &mut Physical,
    b: &mut Physical,
    cp: &ColPoint,
    info: PairInfo,
    soft: bool,
) -> Option<(f32, f32)> {
    let move_mask = if soft { pf::DISABLE_MOVE_FORCE | pf::INFINITE_MASS } else { pf::DISABLE_MOVE_FORCE };
    let f = factors(a, b, info, move_mask);
    let n = cp.normal;
    let ra = cp.point - a.matrix.pos;
    let rb = cp.point - b.matrix.pos;
    let (ca, cb) = (com_of(a), com_of(b));
    let va = approach(a, ra, n);
    let vb = approach(b, rb, n);
    let e = (a.elasticity + b.elasticity) * 0.5;
    let a_turns = !a.has(pf::DISABLE_TURN_FORCE);
    let b_turns = !b.has(pf::DISABLE_TURN_FORCE);
    let no_speed = if soft { 0 } else { pf::DONT_APPLY_SPEED };

    match (a_turns, b_turns) {
        (false, false) => {
            let a_immovable = a.flags & (pf::DISABLE_COLLISION_FORCE | no_speed) != 0;
            let b_immovable = f.immovable_b || b.flags & no_speed != 0;
            let (vcom, b_responds) = if a_immovable {
                (va, true)
            } else if b_immovable {
                (vb, false)
            } else {
                (vb.min(0.0), false)
            };
            if va - vcom >= 0.0 {
                return None;
            }
            let imp_a = (new_v(a, va, vcom, e) - va) * a.mass;
            if a.flags & (pf::DISABLE_COLLISION_FORCE | no_speed) == 0 {
                a.apply_move_force(n * imp_a);
            }
            let mut imp_b = 0.0;
            if b_responds && !(soft && vb - vcom >= 0.0) {
                imp_b = -(new_v(b, vb, vcom, e) - vb) * b.mass;
                if !b_immovable {
                    b.apply_move_force(-n * imp_b);
                }
            }
            Some((imp_a, imp_b))
        }
        (false, true) => {
            let ma = a.mass * f.sa;
            let eff_b = eff_scaled(b, rb - cb, n, f.sb, b.flags & move_mask != 0);
            let vcom = if f.immovable_b { vb } else { (eff_b * vb + ma * va) / (eff_b + ma) };
            if va - vcom >= 0.0 {
                return None;
            }
            let imp_a = (new_v(a, va, vcom, e) - va) * ma;
            let imp_b = -(new_v(b, vb, vcom, e) - vb) * eff_b;
            let mut ja = n * (imp_a / f.sa);
            ja.z = ja.z.max(0.0);
            if f.stand_on_b {
                ja.x *= 2.0;
                ja.y *= 2.0;
            }
            if !a.has(pf::DISABLE_COLLISION_FORCE) {
                a.apply_move_force(ja);
            }
            if !b.has(pf::DISABLE_COLLISION_FORCE) && !f.stand_on_b {
                b.apply_force(-n * (imp_b / f.sb), rb, true);
            }
            Some((imp_a, imp_b))
        }
        (true, false) => {
            let eff_a = eff_scaled(a, ra - ca, n, f.sa, a.flags & move_mask != 0);
            let mb = b.mass * f.sb;
            let vcom = (mb * vb + eff_a * va) / (mb + eff_a);
            if va - vcom >= 0.0 {
                return None;
            }
            let imp_a = (new_v(a, va, vcom, e) - va) * eff_a;
            let imp_b = -(new_v(b, vb, vcom, e) - vb) * mb;
            if !a.has(pf::DISABLE_COLLISION_FORCE) && !f.stand_on_a {
                let mut ja = n * (imp_a / f.sa);
                ja.z = ja.z.max(0.0);
                a.apply_force(ja, ra, true);
            }
            let mut jb = -n * (imp_b / f.sb);
            if jb.z < 0.0 {
                jb.z = 0.0;
                if va.abs() < 0.01 {
                    jb.x *= 0.5;
                    jb.y *= 0.5;
                }
            }
            if f.stand_on_a {
                jb.x *= 2.0;
                jb.y *= 2.0;
            }
            b.apply_move_force(jb);
            Some((imp_a, imp_b))
        }
        (true, true) => {
            let eff_a = eff_scaled(a, ra - ca, n, f.sa, a.flags & move_mask != 0);
            let eff_b = eff_scaled(b, rb - cb, n, f.sb, b.flags & move_mask != 0);
            let vcom = (eff_b * vb + eff_a * va) / (eff_b + eff_a);
            if va - vcom >= 0.0 {
                return None;
            }
            let imp_a = (new_v(a, va, vcom, e) - va) * eff_a;
            let imp_b = -(new_v(b, vb, vcom, e) - vb) * eff_b;
            let mut ja = n * (imp_a / f.sa);
            let mut jb = -n * (imp_b / f.sb);
            let mut ra2 = ra;
            let mut rb2 = rb;
            vehicle_extras(ctx, a, &mut ja, &mut ra2, n.z);
            vehicle_extras(ctx, b, &mut jb, &mut rb2, -n.z);
            if !a.has(pf::DISABLE_COLLISION_FORCE) {
                a.apply_force(ja, ra2, true);
            }
            if !b.has(pf::DISABLE_COLLISION_FORCE) {
                b.apply_force(jb, rb2, true);
            }
            Some((imp_a, imp_b))
        }
    }
}

/// Car-specific tweaks in the both-turnable case.
fn vehicle_extras(ctx: &Ctx, x: &mut Physical, j: &mut Vec3, r: &mut Vec3, nz: f32) {
    if !x.is_vehicle() || x.has_e(ef::HAS_HIT_WALL) || x.has(pf::DISABLE_COLLISION_FORCE) {
        return;
    }
    if nz < 0.7 {
        j.z *= 0.3;
    }
    if x.status == Status::Player {
        *r *= 0.8;
    }
    if ctx.later_collision_pass {
        x.apply_friction_force(*j * -0.3, *r);
    }
}

/// 0x545980 `ApplyFriction(CPhysical* B, float adhesion, CColPoint&)`.
pub fn apply_friction(ctx: &Ctx, a: &mut Physical, b: &mut Physical, mut adhesion: f32, cp: &ColPoint) -> bool {
    let n = cp.normal;
    let ra = cp.point - a.matrix.pos;
    let rb = cp.point - b.matrix.pos;
    let tangent = |v: Vec3| v - n * v.dot(n);
    let a_turns = !a.has(pf::DISABLE_TURN_FORCE);
    let b_turns = !b.has(pf::DISABLE_TURN_FORCE);
    let vel_a = if a_turns { a.get_speed(ra) } else { a.move_speed };
    let vel_b = if b_turns { b.get_speed(rb) } else { b.move_speed };
    let ta = tangent(vel_a);
    let sa = ta.length();
    if sa <= 0.0 {
        return false;
    }
    // dir always comes from A; B's own tangential speed magnitude is used along it (quirk).
    let dir = ta / sa;
    let sb = tangent(vel_b).length();
    let (ma, mb) = (a.mass, b.mass);
    match (a_turns, b_turns) {
        (false, false) => {
            let avg = (mb * sb + ma * sa) / (mb + ma);
            if sa - avg <= 0.0 {
                return false;
            }
            let ia = ((avg - sa) * ma).max(-ctx.ts * adhesion);
            let ib = (avg - sb) * mb;
            a.apply_friction_move_force(dir * ia);
            b.apply_friction_move_force(dir * ib);
            true
        }
        (false, true) => {
            if b.is_vehicle() {
                return false;
            }
            let eff_b = b.eff_mass(rb - com_of(b), dir);
            let avg = (sb * eff_b + ma * sa) / (eff_b + ma);
            if sa - avg <= 0.0 {
                return false;
            }
            let ia = ((avg - sa) * ma).max(-ctx.ts * adhesion);
            let ib = ((avg - sb) * eff_b).min(ctx.ts * adhesion);
            a.apply_friction_move_force(dir * ia);
            if !b.has(pf::DISABLE_COLLISION_FORCE) {
                b.apply_friction_force(dir * ib, rb);
            }
            true
        }
        (true, false) => {
            if a.is_vehicle() {
                return false;
            }
            let eff_a = a.eff_mass(ra - com_of(a), dir);
            let avg = (mb * sb + eff_a * sa) / (mb + eff_a);
            if sa - avg <= 0.0 {
                return false;
            }
            let ia = ((avg - sa) * eff_a).max(-ctx.ts * adhesion);
            let ib = ((avg - sb) * mb).min(ctx.ts * adhesion);
            if !a.has(pf::DISABLE_COLLISION_FORCE) {
                a.apply_friction_force(dir * ia, ra);
            }
            b.apply_friction_move_force(dir * ib);
            true
        }
        (true, true) => {
            if vel_a.dot(n).abs() < 0.2 * 0.707 {
                adhesion *= 0.05;
            }
            let eff_a = a.eff_mass(ra - com_of(a), dir);
            let eff_b = b.eff_mass(rb - com_of(b), dir);
            let avg = (sb * eff_b + eff_a * sa) / (eff_b + eff_a);
            if sa - avg <= 0.0 {
                return false;
            }
            // Not timestep-scaled here (quirk).
            let ia = ((avg - sa) * eff_a).max(-adhesion);
            let ib = ((avg - sb) * eff_b).min(adhesion);
            if !a.has(pf::DISABLE_COLLISION_FORCE) {
                a.apply_friction_force(dir * ia, ra);
            }
            if !b.has(pf::DISABLE_COLLISION_FORCE) {
                b.apply_friction_force(dir * ib, rb);
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::Matrix;

    fn ball(x: f32, vx: f32) -> Physical {
        let mut p = Physical::new(EntityType::Object, Matrix { pos: Vec3::new(x, 0.0, 0.0), ..Matrix::IDENTITY });
        p.mass = 10.0;
        p.turn_mass = 10.0;
        p.elasticity = 1.0;
        p.move_speed = Vec3::new(vx, 0.0, 0.0);
        p
    }

    #[test]
    fn equal_masses_head_on_elastic_swap_speeds() {
        let mut a = ball(0.0, 1.0);
        let mut b = ball(1.0, -1.0);
        // Contact on the line between centres; normal points from B to A.
        let cp = ColPoint { point: Vec3::new(0.5, 0.0, 0.0), normal: -Vec3::X, ..Default::default() };
        apply_collision(&Ctx::new(1.0), &mut a, &mut b, &cp, PairInfo::default(), false).unwrap();
        assert!((a.move_speed.x - -1.0).abs() < 1e-5, "{:?}", a.move_speed);
        assert!((b.move_speed.x - 1.0).abs() < 1e-5, "{:?}", b.move_speed);
    }

    #[test]
    fn separating_bodies_do_not_collide() {
        let mut a = ball(0.0, -1.0);
        let mut b = ball(1.0, 1.0);
        let cp = ColPoint { point: Vec3::new(0.5, 0.0, 0.0), normal: -Vec3::X, ..Default::default() };
        assert!(apply_collision(&Ctx::new(1.0), &mut a, &mut b, &cp, PairInfo::default(), false).is_none());
    }
}
