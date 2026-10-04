//! `CCoronas` (64 slots at 0xC3E058): RegisterCorona (0x6FC180), UpdateCoronaCoors
//! (0x6FC4D0), CCoronas::Update / CRegisteredCorona::Update (0x6FADF0 / 0x6FABF0) and
//! DoSunAndMoon (0x6FC5A0). Rendering (sprites, flares, wet-road reflections) is the
//! app's; it writes back the off-screen flag and the reflection ground height.
//!
//! Not ported: the camera look-direction snap (bChangeBrightnessImmediately).

use glam::Vec3;

use crate::world::{EntityId, World};

pub const MAX_CORONAS: usize = 64;

/// Corona textures (CCoronas::Init, particle.txd).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoronaTex {
    Star,
    Moon,
    Reflect,
    HeadlightLine,
    RingB,
}

impl CoronaTex {
    /// `CoronaTextures[type]` (0x8D4950): 0/1 coronastar, 2 moon, 3 reflect, 4 headlight line, 9 ringb.
    pub fn from_type(t: u8) -> Option<Self> {
        match t {
            0 | 1 => Some(Self::Star),
            2 => Some(Self::Moon),
            3 => Some(Self::Reflect),
            4 => Some(Self::HeadlightLine),
            9 => Some(Self::RingB),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Star => "coronastar",
            Self::Moon => "coronamoon",
            Self::Reflect => "coronareflect",
            Self::HeadlightLine => "coronaheadlightline",
            Self::RingB => "coronaringb",
        }
    }
}

/// `CRegisteredCorona` (0x3C bytes).
#[derive(Debug, Clone, Copy)]
pub struct Corona {
    /// Raw position: a local offset when attached.
    pub pos: Vec3,
    pub id: u64,
    pub tex: Option<CoronaTex>,
    pub size: f32,
    pub far_clip: f32,
    pub near_clip: f32,
    pub height_above_ground: f32,
    pub fade_speed: f32,
    pub rgb: [u8; 3],
    /// Target intensity (the "alpha" argument).
    pub intensity: u8,
    pub faded: u8,
    pub registered: bool,
    /// 0 none, 1 sun, 2 headlight.
    pub flare: u8,
    pub reflection: bool,
    pub check_obstacles: bool,
    pub off_screen: bool,
    pub just_created: bool,
    pub only_from_below: bool,
    pub valid_ground_height: bool,
    pub attached: Option<EntityId>,
}

/// RegisterCorona arguments (defaults as most callers pass them).
#[derive(Debug, Clone, Copy)]
pub struct CoronaArgs {
    pub id: u64,
    pub attach: Option<EntityId>,
    pub rgb: [u8; 3],
    pub alpha: u8,
    pub pos: Vec3,
    pub radius: f32,
    pub far_clip: f32,
    pub tex: Option<CoronaTex>,
    pub flare: u8,
    pub reflection: bool,
    pub check_obstacles: bool,
    pub long_distance: bool,
    pub near_clip: f32,
    /// true = start fully faded in.
    pub fade_state: bool,
    pub fade_speed: f32,
    pub only_from_below: bool,
}

impl Default for CoronaArgs {
    fn default() -> Self {
        Self {
            id: 0,
            attach: None,
            rgb: [255; 3],
            alpha: 255,
            pos: Vec3::ZERO,
            radius: 1.0,
            far_clip: 100.0,
            tex: Some(CoronaTex::Star),
            flare: 0,
            reflection: false,
            check_obstacles: false,
            long_distance: false,
            near_clip: 1.5,
            fade_state: false,
            fade_speed: 15.0,
            only_from_below: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Coronas {
    pub slots: Vec<Option<Corona>>,
    /// `CCoronas::LightsMult` (0x8D4B5C): 1.0, down to 0.6 while the sun dazzles.
    pub lights_mult: f32,
}

impl Default for Coronas {
    fn default() -> Self {
        Self { slots: vec![None; MAX_CORONAS], lights_mult: 1.0 }
    }
}

impl World {
    fn attached_pos(&self, c: &Corona) -> Option<Vec3> {
        match c.attached {
            Some(e) => self.body(e).map(|b| b.phys.matrix.transform(c.pos)),
            None => Some(c.pos),
        }
    }

    /// World position of a corona (attached ones follow their entity).
    pub fn corona_world_pos(&self, c: &Corona) -> Option<Vec3> {
        self.attached_pos(c)
    }

    /// `CCoronas::RegisterCorona` (0x6FC180).
    pub fn register_corona(&mut self, a: CoronaArgs) {
        let w = match a.attach {
            Some(e) => match self.body(e) {
                Some(b) => b.phys.matrix.transform(a.pos),
                None => return,
            },
            None => a.pos,
        };
        let cam = self.camera_pos;
        if a.far_clip * a.far_clip < (cam.x - w.x).powi(2) + (cam.y - w.y).powi(2) {
            return;
        }
        let mut alpha = a.alpha;
        if a.long_distance {
            let d = (cam - w).length();
            if d < 35.0 {
                return;
            }
            if d < 50.0 {
                alpha = ((d - 35.0) * alpha as f32 * (1.0 / 15.0)) as i32 as u8;
            }
        }
        let slots = &mut self.coronas.slots;
        let i = match slots.iter().position(|s| s.is_some_and(|s| s.id == a.id)) {
            Some(i) => {
                let s = slots[i].as_ref().unwrap();
                if s.faded == 0 && alpha == 0 {
                    slots[i] = None;
                    return;
                }
                i
            }
            None => {
                if alpha == 0 {
                    return;
                }
                let Some(i) = slots.iter().position(Option::is_none) else { return };
                slots[i] = Some(Corona {
                    pos: a.pos,
                    id: a.id,
                    tex: a.tex,
                    size: a.radius,
                    far_clip: a.far_clip,
                    near_clip: a.near_clip,
                    height_above_ground: 0.0,
                    fade_speed: a.fade_speed,
                    rgb: a.rgb,
                    intensity: alpha,
                    faded: if a.fade_state { 255 } else { 0 },
                    registered: true,
                    flare: a.flare,
                    reflection: a.reflection,
                    check_obstacles: a.check_obstacles,
                    off_screen: true,
                    just_created: true,
                    only_from_below: a.only_from_below,
                    valid_ground_height: false,
                    attached: a.attach,
                });
                i
            }
        };
        let s = slots[i].as_mut().unwrap();
        s.rgb = a.rgb;
        s.intensity = alpha;
        s.pos = a.pos;
        s.size = a.radius;
        s.far_clip = a.far_clip;
        s.tex = a.tex;
        s.flare = a.flare;
        s.reflection = a.reflection;
        s.registered = true;
        s.check_obstacles = a.check_obstacles;
        s.near_clip = a.near_clip;
        s.fade_speed = a.fade_speed;
        s.only_from_below = a.only_from_below;
        s.attached = a.attach;
    }

    /// `CCoronas::UpdateCoronaCoors` (0x6FC4D0): moves a corona without registering it.
    pub fn update_corona_coors(&mut self, id: u64, pos: Vec3, far_clip: f32) {
        let cam = self.camera_pos;
        if (cam.x - pos.x).powi(2) + (cam.y - pos.y).powi(2) <= far_clip * far_clip {
            if let Some(s) = self.coronas.slots.iter_mut().flatten().find(|s| s.id == id) {
                s.pos = pos;
            }
        }
    }

    /// `CCoronas::Update` (0x6FADF0), once per game frame.
    pub(crate) fn update_coronas(&mut self, ts: f32) {
        self.coronas.lights_mult = (self.coronas.lights_mult + ts * 0.03).min(1.0);
        let cam = self.camera_pos;
        for i in 0..MAX_CORONAS {
            let Some(mut c) = self.coronas.slots[i] else { continue };
            if !c.registered {
                c.intensity = 0;
            }
            if let Some(e) = c.attached {
                if self.body(e).is_none() {
                    self.coronas.slots[i] = None;
                    continue;
                }
            }
            let mut fade_out = false;
            // NB: the raw position, which is the local offset for attached coronas.
            if c.check_obstacles && self.line_of_sight(c.pos, cam, true, None).is_some() {
                fade_out = true;
            }
            if c.off_screen || (c.only_from_below && !(cam.z <= c.pos.z)) {
                fade_out = true;
            }
            if fade_out {
                c.faded = ((c.faded as f32 - ts * c.fade_speed).max(0.0)) as i32 as u8;
            } else {
                if c.intensity > c.faded {
                    c.faded = ((c.faded as f32 + ts * c.fade_speed).min(c.intensity as f32)) as i32 as u8;
                } else if c.intensity < c.faded {
                    c.faded = ((c.faded as f32 - ts * c.fade_speed).max(c.intensity as f32)) as i32 as u8;
                }
                if c.id == 2 {
                    self.coronas.lights_mult = (self.coronas.lights_mult - ts * 0.06).max(0.6);
                }
            }
            if c.faded == 0 && !c.just_created {
                self.coronas.slots[i] = None;
                continue;
            }
            c.just_created = false;
            c.registered = false;
            self.coronas.slots[i] = Some(c);
        }
    }

    /// `CCoronas::DoSunAndMoon` (0x6FC5A0): the two sun coronas from the time cycle.
    pub(crate) fn do_sun_and_moon(&mut self) {
        let Some(tc) = self.timecycle.as_ref() else { return };
        let c = tc.current;
        let sun = tc.vector_to_sun;
        let p = self.camera_pos + sun * (c.far_clip * 0.95);
        let rgb = |v: [f32; 3]| v.map(|x| x as i32 as u8);
        if sun.z > -0.1 {
            self.register_corona(CoronaArgs {
                id: 1,
                rgb: rgb(c.sun_core),
                pos: p,
                radius: c.sun_size * 2.7335,
                far_clip: 999_999.9,
                ..Default::default()
            });
            if sun.z > 0.0 {
                self.register_corona(CoronaArgs {
                    id: 2,
                    rgb: rgb(c.sun_corona),
                    pos: p,
                    radius: c.sun_size * 6.0,
                    far_clip: 999_999.9,
                    flare: 1,
                    check_obstacles: true,
                    ..Default::default()
                });
            }
        }
    }
}
