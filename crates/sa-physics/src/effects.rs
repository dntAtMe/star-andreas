//! What the gameplay code asks of the renderer: FX systems (`FxManager_c` calls),
//! point lights (`CPointLights::AddLight`), camera shakes and scorch decals.
//!
//! The simulation only records requests; the app owns the particle runtime and
//! drains `cmds` after every step. Handles are allocated here so gameplay code can
//! keep talking to a system it created (SetConstTime, Kill, ...).

use glam::Vec3;

use crate::world::EntityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FxHandle(pub u32);

#[derive(Debug, Clone)]
pub enum FxCmd {
    /// `FxManager_c::CreateFxSystem(name, pos, attach, ignoreBounding)` (0x4A9BE0). With
    /// `attach`, `offset` is in that body's model space and the system follows it.
    Create { h: FxHandle, name: &'static str, offset: Vec3, attach: Option<EntityId>, ignore_bounding: bool },
    /// A world system with `Fx_c::CreateMatFromVec(pos, dir)` as its matrix (local +Y = dir).
    CreateDir { h: FxHandle, name: &'static str, pos: Vec3, dir: Vec3 },
    /// Re-aim a `CreateDir` system.
    SetDir(FxHandle, Vec3, Vec3),
    Play(FxHandle),
    /// Play, then free the system once it has finished.
    PlayAndKill(FxHandle),
    Kill(FxHandle),
    /// 0x4AA6C0: pin the system time to `t` (0..1 of its length).
    SetConstTime(FxHandle, bool, f32),
    /// 0x4AA730: velocity added to new particles, world units per second.
    SetVelAdd(FxHandle, Vec3),
    /// 0x4AA660: move the system (world-positioned systems only).
    SetOffsetPos(FxHandle, Vec3),
    /// `FxSystem_c::AddParticle` (0x4AA440) on one of the `Fx_c` (g_fx) systems, by name.
    AddParticle(AddParticle),
}

/// `FxPrtMult_c` (0x4AB290): colour, size, spin and life multipliers.
#[derive(Debug, Clone, Copy)]
pub struct PrtMult {
    pub rgba: [f32; 4],
    pub size: f32,
    pub ang_change: f32,
    pub life: f32,
}

impl PrtMult {
    pub fn new(r: f32, g: f32, b: f32, a: f32, size: f32, ang_change: f32, life: f32) -> Self {
        Self { rgba: [r, g, b, a], size, ang_change, life }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AddParticle {
    /// The g_fx member system, e.g. "prt_splash".
    pub system: &'static str,
    pub pos: Vec3,
    pub vel: Vec3,
    pub time_since: f32,
    pub mult: PrtMult,
    pub z_rot: f32,
    pub light_mult: f32,
    pub light_mult_limit: f32,
    pub local: bool,
    /// `EnablePrim`: only this prim enabled (AddDebris' round robin); None = leave as is.
    pub prim: Option<u8>,
}

/// `CPointLight` (0x30 bytes, `CPointLights::AddLight` 0x7000E0), one frame.
#[derive(Debug, Clone, Copy)]
pub struct PointLight {
    /// 0 point, 1 spot (`dir`), 2 darkness, 3 fog only (always), 4 fog only (lit lamp).
    pub ty: u8,
    pub pos: Vec3,
    pub dir: Vec3,
    pub radius: f32,
    /// Already faded by the camera distance.
    pub color: Vec3,
    pub fog_type: u8,
    pub shadows: bool,
}

/// `CPointLights::aLights` size.
pub const MAX_POINT_LIGHTS: usize = 32;

#[derive(Debug, Default)]
pub struct Effects {
    next: u32,
    pub cmds: Vec<FxCmd>,
    /// Lights of the last processed frame (replaced every step).
    pub lights: Vec<PointLight>,
    /// `TheCamera.CamShake(strength, pos)` requests.
    pub cam_shakes: Vec<(f32, Vec3)>,
    /// `g_debrisPrim` (0xA9ADE4): AddDebris' round-robin prim.
    pub debris_prim: u32,
}

impl Effects {
    pub fn create(&mut self, name: &'static str, offset: Vec3, attach: Option<EntityId>, ignore_bounding: bool) -> FxHandle {
        self.next += 1;
        let h = FxHandle(self.next);
        self.cmds.push(FxCmd::Create { h, name, offset, attach, ignore_bounding });
        h
    }

    pub fn create_dir(&mut self, name: &'static str, pos: Vec3, dir: Vec3) -> FxHandle {
        self.next += 1;
        let h = FxHandle(self.next);
        self.cmds.push(FxCmd::CreateDir { h, name, pos, dir });
        h
    }

    pub fn set_dir(&mut self, h: FxHandle, pos: Vec3, dir: Vec3) {
        self.cmds.push(FxCmd::SetDir(h, pos, dir));
    }

    pub fn play(&mut self, h: FxHandle) {
        self.cmds.push(FxCmd::Play(h));
    }

    pub fn play_and_kill(&mut self, h: FxHandle) {
        self.cmds.push(FxCmd::PlayAndKill(h));
    }

    pub fn kill(&mut self, h: FxHandle) {
        self.cmds.push(FxCmd::Kill(h));
    }

    pub fn set_const_time(&mut self, h: FxHandle, on: bool, t: f32) {
        self.cmds.push(FxCmd::SetConstTime(h, on, t));
    }

    pub fn set_vel_add(&mut self, h: FxHandle, v: Vec3) {
        self.cmds.push(FxCmd::SetVelAdd(h, v));
    }

    pub fn set_offset_pos(&mut self, h: FxHandle, p: Vec3) {
        self.cmds.push(FxCmd::SetOffsetPos(h, p));
    }

    /// `FxSystem_c::AddParticle(pos, vel, timeSince, mult, zRot, lightMult, lightMultLimit, local)`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_particle(
        &mut self,
        system: &'static str,
        pos: Vec3,
        vel: Vec3,
        time_since: f32,
        mult: PrtMult,
        z_rot: f32,
        light_mult: f32,
        light_mult_limit: f32,
        local: bool,
    ) {
        self.cmds.push(FxCmd::AddParticle(AddParticle {
            system,
            pos,
            vel,
            time_since,
            mult,
            z_rot,
            light_mult,
            light_mult_limit,
            local,
            prim: None,
        }));
    }

    /// `CPointLights::AddLight` (0x7000E0): culled beyond range + 15 from the camera, faded
    /// from 0.75 of that, at most 32 per frame.
    #[allow(clippy::too_many_arguments)]
    pub fn add_point_light(
        &mut self,
        cam: Vec3,
        ty: u8,
        pos: Vec3,
        dir: Vec3,
        range: f32,
        rgb: Vec3,
        fog_type: u8,
        shadows: bool,
    ) {
        let r = range + 15.0;
        let (dx, dy) = (pos.x - cam.x, pos.y - cam.y);
        if !(-r < dx && dx < r) || !(-r < dy && dy < r) || self.lights.len() >= MAX_POINT_LIGHTS {
            return;
        }
        let dist = (pos - cam).length();
        if dist >= r {
            return;
        }
        let color = if dist < 0.75 * r { rgb } else { rgb * (1.0 - (dist / r - 0.75) * 4.0) };
        self.lights.push(PointLight { ty, pos, dir, radius: range, color, fog_type, shadows });
    }
}

/// Things a body asks the world to do after its ProcessControl (they need other
/// entities, which a body's own logic cannot reach).
#[derive(Debug, Clone, Copy)]
pub enum WorldRequest {
    /// `CExplosion::AddExplosion(victim, creator, type, pos, lifetime, makeSound, camShake, noDamage)`.
    Explosion {
        victim: Option<EntityId>,
        creator: Option<EntityId>,
        kind: ExplosionType,
        pos: Vec3,
        lifetime_ms: u32,
        cam_shake: f32,
        no_damage: bool,
    },
    /// `gFireManager.StartFire(entity, creator, ...)` with the vehicle-fire lifetime rules.
    StartFire { target: EntityId, creator: Option<EntityId> },
    /// `CCoronas::RegisterCorona`.
    Corona(crate::coronas::CoronaArgs),
    /// `CPointLights::AddLight(type, pos, dir, range, rgb, fogType, extraShadows)`.
    PointLight { ty: u8, pos: Vec3, dir: Vec3, range: f32, rgb: Vec3, fog_type: u8, shadows: bool },
    /// `CShadows::StoreCarLightShadow` (0x70C500).
    CarLightShadow {
        car: EntityId,
        id: u64,
        tex: crate::shadows::ShadowTex,
        pos: Vec3,
        front: glam::Vec2,
        side: glam::Vec2,
        rgb: [u8; 3],
        max_view_angle: f32,
    },
    /// `CWeapon::Fire` of an instant-hit gun (FireInstantHit needs the whole world).
    FireInstantHit(crate::bullet::InstantHit),
    /// `CWeapon::Fire` of a thrown weapon or a rocket launcher; `cam` = (front, up) for rockets.
    FireProjectile { owner: EntityId, ty: u32, effect: Vec3, force: f32, cam: Option<(Vec3, Vec3)> },
    /// `CWeapon::FireAreaEffect`.
    FireAreaEffect { owner: EntityId, ty: u32, src: Vec3, mouse_cam: bool, look_pitch: Option<f32> },
    /// `CWorld::UseDetonator`.
    Detonate,
    /// `CTaskSimpleFight::FightStrike` (the owner is filled in by the ped).
    MeleeStrike(crate::melee::Strike),
}

/// `eExplosionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplosionType {
    Grenade = 0,
    Molotov = 1,
    Rocket = 2,
    WeakRocket = 3,
    Car = 4,
    QuickCar = 5,
    Boat = 6,
    Aircraft = 7,
    Mine = 8,
    Object = 9,
    TankFire = 10,
    Small = 11,
    RcVehicle = 12,
}

/// Per-frame context for code that adds particles (`BodyLogic::process_effects`, the
/// `Fx_c` helpers in fxhelpers.rs).
pub struct FrameFx<'a> {
    pub fx: &'a mut Effects,
    pub requests: &'a mut Vec<WorldRequest>,
    /// `CTimer::m_snTimeInMilliseconds`, `m_FrameCounter`, `ms_fTimeStep`.
    pub now_ms: u32,
    pub frame: u32,
    pub ts: f32,
    /// The global `rand()`.
    pub rng: &'a mut crate::damage::Rand,
    pub cam: Vec3,
    /// Camera side planes (outward normal, d): a sphere is off screen if `n.c - d > r`.
    pub cam_planes: [(Vec3, f32); 4],
    pub wet_roads: f32,
    pub foggyness: f32,
    /// Game clock.
    pub hours: u8,
    pub minutes: u8,
    /// Camera forward (GTA space).
    pub cam_fwd: Vec3,
    /// Time-cycle sprite brightness (x10, raw).
    pub sprite_brightness: f32,
    /// `FindPlayerVehicle() != null`.
    pub player_in_vehicle: bool,
    pub surfaces: &'a crate::surface::SurfaceInfos,
}
