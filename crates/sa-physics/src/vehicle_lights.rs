//! Vehicle lights (vehicle_lights.md): `GetVehicleLightsStatus` (0x6D55C0),
//! `DoVehicleLights` (0x6E1A60) with `DoHeadLightEffect` / `DoTailLightEffect` /
//! `DoHeadLightReflection`, and the emergency / taxi lights of `CAutomobile::PreRender`.
//! Everything goes out as world requests (coronas, point lights, light pools).
//!
//! Not ported: gang drivers driving dark, the tunnel rule, car alarms, the ZR-350 pop-up
//! lamps, first-person suppression, trains and bikes.

use glam::{Vec2, Vec3};

use crate::{
    coronas::{CoronaArgs, CoronaTex},
    effects::{FrameFx, WorldRequest},
    physical::{Physical, Status},
    shadows::ShadowTex,
    world::EntityId,
};

/// Per-car light state.
#[derive(Debug, Clone, Default)]
pub struct CarLights {
    /// `bLightsOn`.
    pub on: bool,
    /// `+0x584`: bit0 right head, bit1 left head, bit2 right tail, bit3 left tail (lit this frame).
    pub render: u8,
    /// `m_nRandomSeed` (u16).
    pub seed: u16,
    /// Vehicle structure dummies: headlights, taillights, headlights2, taillights2 (model space).
    pub dummies: [Vec3; 4],
    /// Handling flag 0x400000 (bluish headlights).
    pub halogen: bool,
    /// `bSirenOrAlarm` (+0x42D & 0x80).
    pub siren: bool,
    /// Taxi light (+0x868 & 1).
    pub taxi_light: bool,
    /// Script override (+0x4A8 bits 3-4): 0 none, 1 force off, 2 force on.
    pub force: u8,
}

/// `GetVehicleLightsStatus` (0x6D55C0), without the gang and tunnel rules.
pub fn lights_status(seed: u16, hours: u8, minutes: u8, fog: f32, wet: f32) -> bool {
    let sm = (seed & 0x3F) as u8;
    if hours > 20 || (hours == 20 && minutes > sm) || hours < 6 || (hours == 6 && minutes < sm) {
        return true;
    }
    let r = seed as f32 * 1.999_999_95e-5;
    fog > r || wet > r
}

/// Inputs from the car for one frame.
pub struct LightCtx<'a> {
    pub id: EntityId,
    /// Stable base for corona ids (the original uses the vehicle pointer).
    pub base_id: u64,
    pub model: u16,
    pub phys: &'a Physical,
    pub engine_on: bool,
    pub brake: f32,
    pub handbrake: bool,
    pub has_driver: bool,
    /// CDamageManager light statuses (0 ok).
    pub light_status: [u8; 4],
    pub rear_bumper: u8,
}

fn corona(f: &mut FrameFx, a: CoronaArgs) {
    f.requests.push(WorldRequest::Corona(a));
}

/// `DoVehicleLights` + the emergency / taxi lights, from `CAutomobile::PreRender`.
pub fn do_vehicle_lights(l: &mut CarLights, c: &LightCtx, f: &mut FrameFx) {
    let model = c.model;
    // Sirens, taxi, FBI Rancher: independent of the time of day and the engine.
    special_lights(l, c, f);
    if matches!(model, 441 | 0xFFFE | 432) {
        return;
    }
    let flags: u8 = if model == 532 || model == 471 { 0 } else { 3 };
    let p = c.phys;
    // State machine (§1.2).
    let want = lights_status(l.seed, f.hours, f.minutes, f.foggyness, f.wet_roads);
    if want != l.on && p.status != Status::Wrecked {
        if p.status == Status::Abandoned {
            if l.on {
                let cam = f.cam;
                if (cam.x - p.matrix.pos.x).abs() + (cam.y - p.matrix.pos.y).abs() > 100.0 {
                    l.on = false;
                }
            }
        } else {
            l.on = want;
        }
    }
    let mut force_on = false;
    match l.force {
        1 => l.on = false,
        2 => force_on = true,
        _ => {}
    }
    l.render &= 0xF0;
    // Which lamps may light (§1.3).
    let ok = |i: usize| c.light_status[i] == 0;
    let mut head_r = ok(1);
    let mut head_l = flags & 1 != 0 && ok(0);
    let mut tail_r = ok(3);
    let mut tail_l = flags & 1 != 0 && ok(3); // exe bug: light 3 again
    if flags & 0x10 != 0 {
        head_l = false;
        head_r = false;
    }
    if flags & 0x20 != 0 {
        tail_l = false;
        tail_r = false;
    }
    if !c.engine_on {
        return;
    }
    let lights_on = l.on || force_on;
    let brake_lit = c.brake > 0.0 && !c.handbrake && c.has_driver;
    if !lights_on {
        if tail_effect(l, c, f, 0, true, !tail_r, false, brake_lit) {
            l.render |= 4;
            tail_effect(l, c, f, 1, true, !tail_r, false, brake_lit);
        }
        if flags & 1 != 0 && tail_effect(l, c, f, 0, false, !tail_l, false, brake_lit) {
            l.render |= 8;
            tail_effect(l, c, f, 1, false, !tail_l, false, brake_lit);
        }
        return;
    }
    if head_effect(l, c, f, 0, true, !head_r) {
        l.render |= 1;
        head_effect(l, c, f, 1, true, !head_r);
    }
    if head_effect(l, c, f, 0, false, !head_l) {
        l.render |= 2;
        head_effect(l, c, f, 1, false, !head_l);
    }
    if tail_effect(l, c, f, 0, true, !tail_r, true, brake_lit) {
        l.render |= 4;
        tail_effect(l, c, f, 1, true, !tail_r, true, brake_lit);
    }
    if flags & 1 != 0 && tail_effect(l, c, f, 0, false, !tail_l, true, brake_lit) {
        l.render |= 8;
        tail_effect(l, c, f, 1, false, !tail_l, true, brake_lit);
    }
    head_light_reflection(l, c, f, flags, head_l, head_r);
    let m = &p.matrix;
    if head_r || head_l {
        let v = p.move_speed;
        let slow = v.x * v.x + v.y * v.y < 0.2025;
        f.requests.push(WorldRequest::PointLight {
            ty: 1,
            pos: m.pos,
            dir: m.fwd,
            range: 20.0,
            rgb: Vec3::ONE,
            fog_type: if slow { 1 } else { 0 },
            shadows: false,
        });
    }
    if (tail_r || tail_l) && brake_lit {
        f.requests.push(WorldRequest::PointLight {
            ty: 1,
            pos: m.pos - m.fwd * 4.0,
            dir: -m.fwd,
            range: 10.0,
            rgb: Vec3::new(0.1, 0.02, 0.02),
            fog_type: 0,
            shadows: false,
        });
    }
}

/// Camera tests of §2.1: returns (dist, dot) or None when the lamp faces away.
fn lamp_view(c: &LightCtx, f: &FrameFx, local: Vec3, facing: Vec3) -> Option<(f32, f32)> {
    let w = c.phys.matrix.transform(local);
    let d = f.cam - w;
    let dist = d.length();
    let dir = if dist > 0.0 { d / dist } else { Vec3::X };
    let dot = dir.dot(facing);
    (dot > 0.0).then_some((dist, dot))
}

/// `DoHeadLightEffect` (0x6E0A50). Returns `!off`.
fn head_effect(l: &CarLights, c: &LightCtx, f: &mut FrameFx, pair: usize, right: bool, off: bool) -> bool {
    let d = l.dummies[2 * pair];
    if pair == 1 && d == Vec3::ZERO {
        return false;
    }
    if off {
        return false;
    }
    let m = &c.phys.matrix;
    // Quirk: the world forward added to a model-space offset.
    let mut lp = d + m.fwd * 0.05;
    if !right {
        lp.x -= 2.0 * d.x;
    }
    let Some((dist, dot)) = lamp_view(c, f, lp, m.fwd) else { return true };
    let sq = dot.sqrt();
    let side = right as u64;
    let far = 150.0;
    if sq > 0.9 && dist < 40.0 {
        let rgb = if l.halogen { [150, 150, 195] } else { [160, 160, 140] };
        corona(
            f,
            CoronaArgs {
                id: c.base_id + 4 + 2 * pair as u64 + side,
                attach: Some(c.id),
                rgb,
                pos: lp,
                radius: 0.075,
                far_clip: far,
                tex: Some(CoronaTex::HeadlightLine),
                near_clip: 0.3,
                ..Default::default()
            },
        );
    }
    let k = 0.5 * sq + 0.3;
    let size = (1.0 - dist * 0.006_666_667) * 0.4 * sq;
    let rgb = if l.halogen {
        [(190.0 * k) as i32 as u8, (190.0 * k) as i32 as u8, (255.0 * k) as i32 as u8]
    } else {
        [(210.0 * k) as i32 as u8, (210.0 * k) as i32 as u8, (195.0 * k) as i32 as u8]
    };
    corona(
        f,
        CoronaArgs {
            id: c.base_id + 2 * pair as u64 + side,
            attach: Some(c.id),
            rgb,
            alpha: 128,
            pos: lp,
            radius: size,
            far_clip: far,
            reflection: true,
            near_clip: 0.5,
            ..Default::default()
        },
    );
    true
}

/// `DoTailLightEffect` (0x6E1780). Returns whether a lit lamp was registered.
#[allow(clippy::too_many_arguments)]
fn tail_effect(
    l: &CarLights,
    c: &LightCtx,
    f: &mut FrameFx,
    pair: usize,
    right: bool,
    off: bool,
    lights_on: bool,
    brake: bool,
) -> bool {
    if matches!(c.model, 439 | 475) && c.rear_bumper != 0 {
        return false;
    }
    let d = l.dummies[2 * pair + 1];
    if pair == 1 && d == Vec3::ZERO {
        return false;
    }
    if off {
        return false;
    }
    let mut lp = d;
    if !right {
        lp.x = -d.x;
    }
    let m = &c.phys.matrix;
    let Some((dist, dot)) = lamp_view(c, f, lp, -m.fwd) else { return false };
    let k = 0.5 * dot + 0.2;
    let size = (1.0 - dist / 150.0) * 0.2 * dot;
    let (red, lit) = if brake {
        ((128.0 * k) as i32 as u8, true)
    } else if lights_on {
        ((96.0 * k) as i32 as u8, true)
    } else {
        (0, false)
    };
    corona(
        f,
        CoronaArgs {
            id: c.base_id + 8 + 2 * pair as u64 + right as u64,
            attach: Some(c.id),
            rgb: [red, 0, 0],
            alpha: 128,
            pos: lp,
            radius: size,
            far_clip: 150.0,
            reflection: true,
            near_clip: 0.5,
            ..Default::default()
        },
    );
    lit
}

/// `DoHeadLightReflection` (0x6E1720): the headlight pool on the ground.
fn head_light_reflection(l: &CarLights, c: &LightCtx, f: &mut FrameFx, flags: u8, left: bool, right: bool) {
    let m = &c.phys.matrix;
    let fw = Vec2::new(m.fwd.x, m.fwd.y).normalize_or_zero();
    let p = m.pos;
    let d0 = l.dummies[0];
    let twin = |f: &mut FrameFx| {
        let w = 4.0 * d0.x;
        let k = 2.0 * w + 1.0 + d0.y;
        let cpos = Vec3::new(p.x + fw.x * k, p.y + fw.y * k, p.z + 2.0);
        push_pool(f, c, ShadowTex::Headlight, cpos, Vec2::new(2.0 * w * fw.x, 2.0 * w * fw.y), Vec2::new(w * fw.y, -w * fw.x));
    };
    let single = |f: &mut FrameFx, is_right: bool| {
        let mut d = d0;
        if !is_right {
            d.x = -d.x;
        }
        let w = if c.model == 471 { 1.25 } else { 4.0 * d.x.abs() };
        let r = Vec2::new(m.right.x, m.right.y).normalize_or_zero();
        let k = 2.0 * w + 1.0 + d.y;
        let cpos = Vec3::new(p.x + fw.x * k + r.x * d.x, p.y + fw.y * k + r.y * d.x, p.z + 2.0);
        push_pool(f, c, ShadowTex::Headlight1, cpos, Vec2::new(2.0 * w * fw.x, 2.0 * w * fw.y), Vec2::new(w * fw.y, -w * fw.x));
    };
    if flags & 1 != 0 {
        if left && right {
            twin(f);
        } else if left {
            single(f, false);
        } else if right {
            single(f, true);
        }
    } else if c.model == 532 {
        twin(f);
    } else {
        single(f, true);
    }
}

fn push_pool(f: &mut FrameFx, c: &LightCtx, tex: ShadowTex, pos: Vec3, front: Vec2, side: Vec2) {
    f.requests.push(WorldRequest::CarLightShadow {
        car: c.id,
        id: c.base_id + 0x16,
        tex,
        pos,
        front,
        side,
        rgb: [45, 45, 45],
        max_view_angle: 7.0,
    });
}

/// Sirens (§4.1), taxi light (§4.2), FBI Rancher (§4.3).
fn special_lights(l: &CarLights, c: &LightCtx, f: &mut FrameFx) {
    let now = f.now_ms;
    let m = &c.phys.matrix;
    let sb = f.sprite_brightness;
    let sirens: Option<(Vec3, Vec3, [f32; 3], [f32; 3])> = match c.model {
        407 => Some((Vec3::new(0.9, 3.2, 1.3), Vec3::new(-0.9, 3.2, 1.3), [255.0, 0.0, 0.0], [255.0, 255.0, 0.0])),
        416 => Some((Vec3::new(0.6, 0.9, 1.2), Vec3::new(-0.6, 0.9, 1.2), [255.0, 0.0, 0.0], [255.0, 255.0, 255.0])),
        427 => Some((Vec3::new(0.55, 1.1, 1.4), Vec3::new(-0.55, 1.1, 1.4), [255.0, 0.0, 0.0], [0.0, 0.0, 255.0])),
        596..=598 => Some((Vec3::new(0.7, -0.4, 1.0), Vec3::new(-0.7, -0.4, 1.0), [255.0, 0.0, 0.0], [0.0, 0.0, 255.0])),
        599 => Some((Vec3::new(0.7, -0.1, 1.2), Vec3::new(-0.7, -0.1, 1.2), [255.0, 0.0, 0.0], [0.0, 0.0, 255.0])),
        _ => None,
    };
    if let (Some((a, b, c1, c2)), true) = (sirens, l.siren) {
        // One point light alternating the colours every 512 ms with 100 ms fades.
        let t = now & 0x3FF;
        let col = if t < 512 { c1 } else { c2 }.map(|x| ((x as i32) / 3) as f32);
        let u = now & 0x1FF;
        let col = if u < 100 {
            col.map(|x| ((x * u as f32 * 0.01) as i32) as f32)
        } else if u > 412 {
            col.map(|x| ((x * (512 - u) as f32 * 0.01) as i32) as f32)
        } else {
            col
        };
        f.requests.push(WorldRequest::PointLight {
            ty: 0,
            pos: m.pos + m.up * 2.0,
            dir: Vec3::ZERO,
            range: 10.0,
            rgb: Vec3::from(col) * 0.005,
            fog_type: 0,
            shadows: true,
        });
        let cc = |c: [f32; 3]| c.map(|x| (x * sb * 0.1) as i32 as u8);
        let (k1, k2) = (cc(c1), cc(c2));
        for i in 0..4u32 {
            let lp = (b * (3 - i) as f32 + a * i as f32) * 0.333_333_34;
            let s = ((now + 64 * i) >> 8) & 3;
            let rgb = match s {
                0 => k1,
                2 => k2,
                _ => continue,
            };
            corona(
                f,
                CoronaArgs {
                    id: c.base_id + 0x15 + i as u64,
                    attach: Some(c.id),
                    rgb,
                    pos: lp,
                    radius: 0.4,
                    far_clip: 150.0,
                    ..Default::default()
                },
            );
        }
    }
    if l.taxi_light && matches!(c.model, 420 | 438) {
        let lp = if c.model == 420 { Vec3::new(0.0, -0.4, 0.95) } else { Vec3::new(0.0, 0.0, 0.85) };
        let k = (sb * 10.0) as i32 as u8;
        corona(
            f,
            CoronaArgs {
                id: c.base_id + 0x11,
                attach: Some(c.id),
                rgb: [k, k, 0],
                pos: lp,
                radius: 0.8,
                far_clip: 150.0,
                reflection: true,
                ..Default::default()
            },
        );
        f.requests.push(WorldRequest::PointLight {
            ty: 0,
            pos: m.transform(lp),
            dir: Vec3::ZERO,
            range: 10.0,
            rgb: Vec3::new(0.1, 0.1, 0.05),
            fog_type: 0,
            shadows: true,
        });
    }
    if c.model == 490 && l.siren && now & 0x100 != 0 && f.cam_fwd.dot(m.fwd) < 0.0 {
        corona(
            f,
            CoronaArgs {
                id: c.base_id + 0x15,
                attach: Some(c.id),
                rgb: [0, 0, 255],
                pos: Vec3::new(0.0, 1.2, 0.5),
                radius: 0.4,
                far_clip: 150.0,
                ..Default::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::lights_status;

    #[test]
    fn lights_follow_the_clock_with_a_per_car_offset() {
        assert!(lights_status(0, 22, 0, 0.0, 0.0));
        assert!(!lights_status(0, 12, 0, 0.0, 0.0));
        // seed & 63 = 30: on after 20:30, off after 06:30.
        assert!(!lights_status(30, 20, 30, 0.0, 0.0) && lights_status(30, 20, 31, 0.0, 0.0));
        assert!(lights_status(30, 6, 29, 0.0, 0.0) && !lights_status(30, 6, 30, 0.0, 0.0));
        // Fog switches lights on when it exceeds seed * 2e-5.
        assert!(lights_status(10_000, 12, 0, 0.3, 0.0) && !lights_status(10_000, 12, 0, 0.1, 0.0));
    }
}
