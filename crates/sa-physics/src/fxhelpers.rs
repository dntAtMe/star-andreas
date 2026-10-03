//! `Fx_c` helpers (g_fx 0xA9AE00) and their vehicle callers: AddSparks (0x49F040),
//! AddDebris (0x49F750), AddWheelSpray / Grass / Gravel / Mud / Sand / Dust
//! (0x49FB30..0x4A09C0), `CAutomobile::dmgDrawCarCollidingParticles` (0x6A6DC0),
//! `CVehicle::AddSingleWheelParticles` (0x6DE880) and `AddWheelDirtAndWater` (0x6D2D50).
//! Particles go out as `FxCmd::AddParticle` on the static `prt_*` systems.
//!
//! Not ported: skid marks, the dirt level, audio, bikes, 6-wheelers' middle wheels,
//! the "touching water" physical flag (only WATER surfaces count).

use glam::Vec3;

use crate::{
    colpoint::ColPoint,
    effects::{AddParticle, FrameFx, FxCmd, PrtMult},
    surface::wheel_fx,
};

/// What `AddWheelX` draws (grass / gravel / mud share one function, sand / dust another).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelDirt {
    Grass,
    Gravel,
    Mud,
}

/// The vehicle fields the wheel helpers read.
#[derive(Debug, Clone, Copy)]
pub struct WheelVeh {
    pub model: u16,
    pub pos: Vec3,
    pub move_speed: Vec3,
    pub gas: f32,
    /// +0x594: 0 car, 2 quad, 9 bike, 10 BMX.
    pub subtype: u8,
    /// +0x12C contact-surface brightness (1.0 when not tracked).
    pub lighting: f32,
    pub player_driven: bool,
}

impl FrameFx<'_> {
    fn rand01(&mut self) -> f32 {
        self.rng.rand01()
    }

    /// `(rand() % 10000) * 1e-4`.
    fn u4(&mut self) -> f32 {
        (self.rng.next() % 10000) as f32 * 1e-4
    }

    /// `CGeneral::GetRandomNumberInRange(float, float)`.
    fn grnir(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.rand01()
    }

    #[allow(clippy::too_many_arguments)]
    fn particle(
        &mut self,
        system: &'static str,
        pos: Vec3,
        vel: Vec3,
        time_since: f32,
        mult: PrtMult,
        light_mult: f32,
        limit: f32,
        prim: Option<u8>,
    ) {
        self.fx.cmds.push(FxCmd::AddParticle(AddParticle {
            system,
            pos,
            vel,
            time_since,
            mult,
            z_rot: -1.0,
            light_mult,
            light_mult_limit: limit,
            local: false,
            prim,
        }));
    }

    /// `CEntity::GetIsOnScreen` with the bounding sphere.
    pub fn on_screen(&self, c: Vec3, r: f32) -> bool {
        self.cam_planes.iter().all(|(n, d)| n.dot(c) - d <= r)
    }

    /// "LOD-A" frame thinning of the wheel helpers (sparks.md 2.2).
    fn lod_a(&self, model: u16, d2: f32) -> bool {
        let s = (model as u8 as u32).wrapping_add(self.frame);
        if d2 > 400.0 {
            s & 3 == 0
        } else if d2 > 64.0 || !self.player_in_vehicle {
            s & 1 == 0
        } else {
            true
        }
    }

    /// "LOD-B" (quality >= 1, the highest setting in this port).
    fn lod_b(&self, model: u16, d2: f32) -> bool {
        let s = (model as i16 as i32).wrapping_add(self.frame as i32);
        let near_player = d2 <= 64.0 && self.player_in_vehicle;
        !(s & 1 != 0 || (!near_player && s & 3 != 0))
    }

    /// `Fx_c::AddSparks` (0x49F040).
    #[allow(clippy::too_many_arguments)]
    pub fn add_sparks(
        &mut self,
        pos: Vec3,
        dir: Vec3,
        force: f32,
        count: i32,
        across: Vec3,
        use_spark1: bool,
        spread: f32,
        life: f32,
    ) {
        let d2 = (self.cam - pos).length_squared();
        if d2 > 22500.0 || (d2 > 225.0 && self.frame & 1 != 0) {
            return;
        }
        let mult = PrtMult::new(1.0, 1.0, 1.0, 1.0, 1.0, 0.0, life * 0.8);
        let a_ts = across * self.ts;
        let nf = count as f32;
        let s2 = spread - -spread;
        for i in 0..count.max(0) {
            let t = 1.0 - i as f32 / nf;
            let mut d = dir;
            d.x += self.rand01() * s2 + -spread;
            d.y += self.rand01() * s2 + -spread;
            d.z += self.rand01() * s2 + -spread;
            let vel = d * force;
            let p = pos - a_ts * t;
            let sys = if use_spark1 { "prt_spark" } else { "prt_spark_2" };
            self.particle(sys, p, vel, t * 0.05, mult, 1.2, 0.6, None);
        }
    }

    /// `Fx_c::AddDebris` (0x49F750): round robin over prt_cardebris' four prims.
    pub fn add_debris(&mut self, pos: Vec3, col: [u8; 4], scale: f32, count: i32) {
        if (self.cam - pos).length_squared() > 625.0 {
            return;
        }
        let k = 1.0 / 255.0;
        let ang = (self.u4() + 1.0) * 0.5;
        let mult = PrtMult {
            rgba: col.map(|c| c as f32 * k),
            size: scale,
            ang_change: ang,
            life: 0.2,
        };
        for _ in 0..count.max(0) {
            let vx = self.rand01() * 0.5 * 20.0 - 5.0;
            let vy = self.rand01() * 0.5 * 20.0 - 5.0;
            let vz = self.rand01() * 0.15 * 20.0 + 2.0;
            let prim = self.fx.debris_prim as u8;
            self.particle("prt_cardebris", pos, Vec3::new(vx, vy, vz), 0.0, mult, 1.2, 0.6, Some(prim));
            self.fx.debris_prim = (self.fx.debris_prim + 1) & 3;
        }
    }

    /// `Fx_c::AddWheelSpray` (0x49FB30), prt_boatsplash.
    pub fn add_wheel_spray(&mut self, v: &WheelVeh, pos: Vec3, strong: bool, bright: bool, light_mult: f32) {
        let d2 = (self.cam - pos).length_squared();
        if d2 > 625.0 || !self.lod_a(v.model, d2) {
            return;
        }
        let speed = v.move_speed.length();
        if !(speed > 0.01) && !strong {
            return;
        }
        let k = if strong { 1.0 } else { (2.0 * speed).min(1.0) };
        let mult = PrtMult::new(1.0, 1.0, 1.0, (k + 1.0) * if bright { 0.2 } else { 0.15 }, (k + 1.0) * 0.2, 1.0, 0.08);
        let s1 = (k + 1.0) * 10.0;
        let lo = 30.0 - s1;
        let sc = self.rand01() * ((s1 + 30.0) - lo) + lo;
        let vel = v.move_speed * sc;
        let n = ((v.move_speed * self.ts).length() as i32).max(1);
        let inv = 1.0 / n as f32;
        for i in 0..n {
            let off = ((v.move_speed * inv) * i as f32) * self.ts;
            let p = Vec3::new(pos.x - off.x, pos.y - off.y, (pos.z - off.z) + 0.25);
            let mut vi = vel;
            vi.z += (self.rand01() + 1.0) * k;
            self.particle("prt_boatsplash", p, vi, 0.0, mult, light_mult, 0.6, None);
        }
    }

    /// `Fx_c::AddWheelGrass / Gravel / Mud` (0x49FF20 / 0x4A0170 / 0x4A03C0), prt_wheeldirt.
    pub fn add_wheel_dirt(&mut self, kind: WheelDirt, v: &WheelVeh, pos: Vec3, light_mult: f32) {
        if !v.player_driven {
            return;
        }
        let d2 = (self.cam - pos).length_squared();
        if d2 > 625.0 || !self.lod_a(v.model, d2) {
            return;
        }
        let (r, g, b) = match kind {
            WheelDirt::Grass => (0.03, 0.09, 0.03),
            WheelDirt::Gravel => (0.25, 0.25, 0.25),
            WheelDirt::Mud => (0.25, 0.12, 0.06),
        };
        let mut mult = PrtMult::new(r, g, b, 1.0, 0.0, 0.0, 0.05);
        for _ in 0..3 {
            mult.size = self.rand01() * 0.03 + 0.03;
            let vx = self.rand01() * (v.move_speed.x * -1.5);
            let vy = self.rand01() * (v.move_speed.y * -1.5);
            let vz = self.rand01() * 1.5 + 2.0;
            let px = (self.rand01() * 0.4 + pos.x) - 0.2;
            let py = (self.rand01() * 0.4 + pos.y) - 0.2;
            self.particle("prt_wheeldirt", Vec3::new(px, py, pos.z), Vec3::new(vx, vy, vz), 0.0, mult, light_mult, 0.6, None);
        }
    }

    /// `Fx_c::AddWheelSand / Dust` (0x4A0610 / 0x4A09C0), prt_sand.
    pub fn add_wheel_sand(&mut self, dust: bool, v: &WheelVeh, pos: Vec3, strong: bool, light_mult: f32) {
        let d2 = (self.cam - pos).length_squared();
        if d2 > 625.0 || !self.lod_b(v.model, d2) {
            return;
        }
        let mut mult = if dust {
            PrtMult::new(0.51, 0.44, 0.31, 0.5, 1.0, 0.0, 0.0)
        } else {
            PrtMult::new(0.81, 0.67, 0.57, 0.5, 1.0, 0.0, 0.0)
        };
        let g = v.gas.abs();
        let k = if strong { 1.0 } else { (2.0 * v.move_speed.length()).min(1.0) };
        mult.life = if dust { 0.05 * k + 0.1 } else { (1.0 + k) * 0.1 };
        mult.size = 0.9 * k + 0.1;
        let mut m = 1.5;
        match v.subtype {
            10 => {
                m = 2.0;
                mult.size *= 0.25;
            }
            9 | 2 => {
                m = 2.0;
                mult.size *= 0.5;
            }
            _ => mult.size *= 0.7,
        }
        let v_ts = v.move_speed * self.ts;
        let n = ((v_ts.length() * m) as i32).max(1);
        let zs = (k + 0.8) - 0.2;
        for i in 0..n {
            let vx = self.rand01() * (g * v.move_speed.x * -40.0);
            let vy = self.rand01() * (g * v.move_speed.y * -40.0);
            let vz = self.rand01() * zs + 0.2;
            let t = 1.0 - i as f32 / n as f32;
            let p = pos - v_ts * t;
            self.particle("prt_sand", p, Vec3::new(vx, vy, vz), 0.0, mult, light_mult, 0.7, None);
        }
    }

    /// `CAutomobile::dmgDrawCarCollidingParticles` (0x6A6DC0). `colour` is the primary
    /// carcol RGBA; `rammed` = weapon type not in {0 fist, 14 flowers}.
    #[allow(clippy::too_many_arguments)]
    pub fn car_colliding_particles(
        &mut self,
        car_pos: Vec3,
        bound_radius: f32,
        move_speed: Vec3,
        pos: Vec3,
        force: f32,
        rammed: bool,
        colour: [u8; 4],
        lighting: f32,
    ) {
        if !self.on_screen(car_pos, bound_radius) {
            return;
        }
        let n = force as i32;
        if rammed {
            let mag = move_speed.length();
            let dir = if mag > 0.0 { move_speed / mag } else { Vec3::X };
            let mag = if mag > 0.0 { mag } else { 1.0 };
            self.add_sparks(pos, dir, mag * -10.0, ((n / 10) + 4) & 0x3F, move_speed, true, 0.3, 1.0);
        }
        let mult = PrtMult::new(0.4, 0.4, 0.4, 0.6, 0.4, 1.0, 0.1);
        let p = (pos - car_pos) * 0.7 + car_pos;
        let cnt = (((move_speed * self.ts).length() * 4.0) as i32).max(1);
        for _ in 0..cnt {
            self.particle("prt_smoke_huge", p, Vec3::ZERO, 0.0, mult, 1.2, 0.6, None);
        }
        if move_speed.length_squared() > 0.0625 {
            // The low byte of ftol is kept: values above 255 wrap.
            let c = [
                (colour[0] as f32 * lighting) as i32 as u8,
                (colour[1] as f32 * lighting) as i32 as u8,
                (colour[2] as f32 * lighting) as i32 as u8,
                colour[3],
            ];
            self.add_debris(p, c, 0.06, n / 100 + 1);
        }
    }

    /// `CVehicle::AddWheelDirtAndWater` (0x6D2D50). Returns whether tyre smoke may follow.
    pub fn wheel_dirt_and_water(&mut self, v: &WheelVeh, cp: &ColPoint, fast: bool, strong: bool, in_water: bool) -> bool {
        let info = self.surfaces.info(cp.surface_b);
        let pos = cp.point;
        let l = v.lighting;
        if !fast && !info.is_sand {
            return false;
        }
        if in_water {
            self.add_wheel_spray(v, pos, strong, true, l);
            return false;
        }
        let w = info.wheel_fx;
        if w[wheel_fx::GRASS] {
            self.add_wheel_dirt(WheelDirt::Grass, v, pos, l);
            return false;
        }
        if w[wheel_fx::GRAVEL] {
            self.add_wheel_dirt(WheelDirt::Gravel, v, pos, l);
            return true;
        }
        if w[wheel_fx::MUD] {
            self.add_wheel_dirt(WheelDirt::Mud, v, pos, l);
            return false;
        }
        for (flag, dust) in [(wheel_fx::DUST, true), (wheel_fx::SAND, false)] {
            if w[flag] {
                let wet = self.wet_roads;
                if wet > 0.0 && self.grnir(wet, 1.01) > 0.5 {
                    return false;
                }
                self.add_wheel_sand(dust, v, pos, strong, l);
                return false;
            }
        }
        if w[wheel_fx::SPRAY] && self.wet_roads > 0.4 {
            self.add_wheel_spray(v, pos, strong, false, l);
            return false;
        }
        true
    }

    /// `CVehicle::AddSingleWheelParticles` (0x6DE880), particle part.
    /// `state` 0 normal, 1 spinning, 2 skidding, 3 locked; `status` 0 ok, 1 burst, 2 missing.
    #[allow(clippy::too_many_arguments)]
    pub fn single_wheel_particles(
        &mut self,
        v: &WheelVeh,
        fwd: Vec3,
        state: u8,
        status: u8,
        comp: f32,
        speed: f32,
        cp: &ColPoint,
        flags: u32,
    ) {
        let d2 = (self.cam - v.pos).length_squared();
        if d2 > 625.0 {
            return;
        }
        let do_fx = self.lod_a(v.model, d2);
        if !(comp < 1.0) {
            return;
        }
        let surf = self.surfaces.info(cp.surface_b);
        let in_water = surf.is_water;
        let fast = speed < 1.0;
        let from = cp.point;
        // Burst tyre on a sparking surface: rim sparks (not LOD gated).
        if status == 1 && (speed > 0.1 || state == 1) && surf.friction_effect == 1 {
            let mut dir = v.move_speed * -50.0;
            dir.z += 2.5;
            let mut cnt = speed * 32.0;
            if state == 1 && speed < 0.2 {
                dir = (fwd * v.gas) * -12.0;
                dir.z += 2.5;
                cnt = 10.0;
            }
            let mag = dir.length();
            let dir = if mag > 0.0 { dir / mag } else { Vec3::X };
            self.add_sparks(from, dir, mag, cnt as i32, v.move_speed, true, 0.1, 0.3);
        }
        match state {
            1 => {
                if self.wheel_dirt_and_water(v, cp, fast, true, in_water) && do_fx {
                    let mut mult = PrtMult::new(0.9, 0.9, 1.0, 0.5, 1.0, 1.0, 0.5);
                    if v.move_speed.length() > 0.15 {
                        mult.rgba[3] = 0.3;
                        mult.size = 0.5;
                    }
                    match v.subtype {
                        9 | 2 => mult.size *= 0.5,
                        10 => {
                            mult.size *= 0.2;
                            mult.life *= 0.3;
                        }
                        _ => {}
                    }
                    let g = v.gas.abs();
                    let vx = self.grnir(0.0, g * v.move_speed.x * -30.0);
                    let vy = self.grnir(0.0, g * v.move_speed.y * -30.0);
                    let vz = self.u4() * 0.8;
                    let c = self.grnir(0.5, 1.0);
                    mult.rgba[2] = c;
                    mult.rgba[0] = c * 0.9;
                    mult.rgba[1] = c * 0.9;
                    self.particle("prt_smokeII_3_expand", from, Vec3::new(vx, vy, vz), 0.0, mult, v.lighting, 0.6, None);
                }
            }
            2 | 3 => {
                if state == 2 && flags & 4 != 0 {
                    return;
                }
                if speed > 0.03 {
                    self.skid_smoke(v, cp, fast, in_water, do_fx);
                }
            }
            _ => {
                if speed > 0.03 {
                    self.wheel_dirt_and_water(v, cp, fast, false, in_water);
                }
            }
        }
    }

    fn skid_smoke(&mut self, v: &WheelVeh, cp: &ColPoint, fast: bool, in_water: bool, do_fx: bool) {
        if !(self.wheel_dirt_and_water(v, cp, fast, false, in_water) && do_fx) {
            return;
        }
        let mut mult = PrtMult::new(0.9, 0.9, 1.0, 0.5, 0.7, 1.0, 0.3);
        let mut m = 2.0;
        match v.subtype {
            9 | 2 => {
                mult.size *= 0.5;
                m = 3.0;
            }
            10 => {
                mult.size *= 0.2;
                mult.life *= 0.3;
                m = 3.0;
            }
            _ => {}
        }
        let v_ts = v.move_speed * self.ts;
        let n = ((v_ts.length() * m) as i32).max(1);
        for i in 0..n {
            let c = self.grnir(0.5, 1.0);
            mult.rgba[2] = c;
            mult.rgba[0] = c * 0.9;
            mult.rgba[1] = c * 0.9;
            let t = 1.0 - i as f32 / n as f32;
            let p = cp.point - v_ts * t;
            self.particle("prt_smokeII_3_expand", p, Vec3::new(0.0, 0.0, 0.5), 0.3, mult, v.lighting, 0.6, None);
        }
    }
}
