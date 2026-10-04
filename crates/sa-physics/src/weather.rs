//! `CWeather` (0x72A480..0x72B850): the weather cycle, wind, rain, lightning, and the
//! rain / sandstorm particles and rain streaks.
//!
//! Not ported: cull zones (CamNoRain / PlayerNoRain, tunnels), interiors, water
//! (UnderWaterness stays 0), CTimeCycle's sun vector (SunGlare) and FogReduction,
//! weather audio and pad shake, the first-person roof check.

use glam::Vec3;

use crate::{effects::PrtMult, world::World};

/// eWeatherType names (timecyc.dat block order).
pub const WEATHER_NAMES: [&str; 23] = [
    "EXTRASUNNY_LA",
    "SUNNY_LA",
    "EXTRASUNNY_SMOG_LA",
    "SUNNY_SMOG_LA",
    "CLOUDY_LA",
    "SUNNY_SF",
    "EXTRASUNNY_SF",
    "CLOUDY_SF",
    "RAINY_SF",
    "FOGGY_SF",
    "SUNNY_VEGAS",
    "EXTRASUNNY_VEGAS",
    "CLOUDY_VEGAS",
    "EXTRASUNNY_COUNTRYSIDE",
    "SUNNY_COUNTRYSIDE",
    "CLOUDY_COUNTRYSIDE",
    "RAINY_COUNTRYSIDE",
    "EXTRASUNNY_DESERT",
    "SUNNY_DESERT",
    "SANDSTORM_DESERT",
    "UNDERWATER",
    "EXTRACOLOURS_1",
    "EXTRACOLOURS_2",
];

/// Weather cycle lists (0x8D5EB0..), one entry per game hour: default/countryside, LA, SF, LV, desert.
#[rustfmt::skip]
const CYCLES: [[i8; 64]; 5] = [
    [13,13,13,13,13,13,13,13,14,15, 9, 9,15,14,13,14,14,13,13,13,13,14,14,13,13,13,13,13,13,13,13,13,
     13,13,14,14,14,14,14,14,15, 9, 9,15,16,16,16,16,15,14,14,14,14,13,13,13,13,13,13,13,13,13,13,13],
    [ 2, 2, 0, 0, 0, 2, 2, 2, 3, 0, 3, 2, 0, 0, 3, 3, 3, 4, 4, 0, 0, 1, 1, 0, 2, 2, 2, 2, 2, 2, 2, 2,
      2, 2, 3, 3, 3, 3, 3, 3, 2, 2, 3, 3, 0, 0,16, 0, 4, 1, 1, 1, 1, 0, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2],
    [ 5, 5, 5, 5, 5, 7, 7, 9, 9, 7, 5, 5, 5, 5, 5, 7, 7, 7, 8, 8, 7, 7, 5, 5, 5, 5, 6, 6, 5, 7, 9, 9,
      7, 5, 5, 7, 5, 5, 6, 6, 5, 5, 5, 7, 7, 8, 8, 7, 7, 9, 9, 5, 5, 6, 6, 6, 5, 5, 6, 7, 7, 6, 6, 5],
    [11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,10,10,10,10,10,11,11,11,11,11,11,11,11,
     11,10,11,11,11,11,10,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,11,10,10,12,12,10,12,10,10],
    [17,17,17,19,19,17,17,17,17,17,17,17,17,17,17,17,17,17,17,18,18,18,18,18,17,17,17,17,17,17,17,17,
     17,17,17,17,17,19,19,19,17,17,17,17,17,17,17,17,17,17,18,18,18,17,17,17,17,17,17,17,19,19,19,17],
];

/// `WindPerType` (0x8D5E50).
const WIND_PER_TYPE: [f32; 23] =
    [0.0, 0.25, 0.0, 0.2, 0.7, 0.25, 0.0, 0.7, 1.0, 0.0, 0.2, 0.0, 0.4, 0.0, 0.3, 0.7, 1.0, 0.0, 0.3, 1.5, 0.0, 0.0, 0.0];
/// Wind direction noise (0x8D6038) and gust strength (0x8D5FF8).
const WIND_D: [f32; 16] = [0.5, -0.3, 0.8, 0.0, -0.4, -0.8, 0.3, -0.1, -0.9, -0.5, 0.7, 0.7, 0.3, 0.7, 0.0, -0.5];
const WIND_G: [f32; 16] = [1.0, 0.5, 1.0, 0.2, 0.4, 1.0, 1.0, 1.0, 1.0, 0.8, 0.0, 1.0, 1.0, 0.7, 1.0, 1.0];

const RAINY: &[i16] = &[8, 16];
const SANDSTORM: &[i16] = &[19];
const FOGGY: &[i16] = &[9, 19];
const FOGGY_SF: &[i16] = &[9];
const EXTRASUNNY: &[i16] = &[0, 2, 13, 6, 11, 17];
const SUNNY: &[i16] = &[1, 3, 14, 5, 10, 18];
const CLOUDY: &[i16] = &[4, 15, 12, 7];
const HEATHAZE: &[i16] = &[18, 17, 11, 0];

/// One rain streak line (`RenderRainStreaks`): bottom with alpha `a`, top with `a/2`.
#[derive(Debug, Clone, Copy)]
pub struct RainStreak {
    pub bottom: Vec3,
    pub top: Vec3,
    pub alpha: u8,
}

#[derive(Debug, Clone)]
pub struct Weather {
    pub old_type: i16,
    pub new_type: i16,
    pub forced_type: i16,
    pub type_in_list: i32,
    pub region: i16,
    pub interpolation: f32,
    pub wind: f32,
    pub wind_clipped: f32,
    /// Not normalised: includes WindClipped (see weather.md 3.11).
    pub wind_dir: Vec3,
    pub rain: f32,
    pub sandstorm: f32,
    pub wet_roads: f32,
    pub cloud_coverage: f32,
    pub foggyness: f32,
    pub foggyness_sf: f32,
    pub extra_sunnyness: f32,
    pub rainbow: f32,
    pub heat_haze: f32,
    pub heat_haze_fx_control: f32,
    heat_haze_acc: f32,
    heat_haze_prev_minute: i32,
    pub under_waterness: f32,
    pub traffic_lights_brightness: f32,
    pub lightning_flash: bool,
    pub lightning_burst: bool,
    lightning_start_frame: u32,
    lightning_last_change: u32,
    pub lightning_duration: u32,
    when_thunder: u32,
    /// Set when thunder should sound (cleared by the app).
    pub thunder: bool,
    rain_recently: bool,
    rain_stop_countdown: i32,
    pub mist_alpha: f32,
    /// Rain streak state (`RenderRainStreaks`): count and the 32 int-metre streaks.
    pub streak_count: i32,
    streaks_xyz: Option<[[i32; 3]; 32]>,
    streaks_a: [u8; 32],
    /// Streak lines for the renderer, rebuilt every frame.
    pub streaks: Vec<RainStreak>,
}

impl Default for Weather {
    /// `CWeather::Init` (0x72A480).
    fn default() -> Self {
        Self {
            old_type: 0,
            new_type: 0,
            forced_type: -1,
            type_in_list: 0,
            region: 0,
            interpolation: 0.0,
            wind: 0.0,
            wind_clipped: 0.0,
            wind_dir: Vec3::ZERO,
            rain: 0.0,
            sandstorm: 0.0,
            wet_roads: 0.0,
            cloud_coverage: 0.0,
            foggyness: 0.0,
            foggyness_sf: 0.0,
            extra_sunnyness: 0.0,
            rainbow: 0.0,
            heat_haze: 0.0,
            heat_haze_fx_control: 0.0,
            heat_haze_acc: 0.0,
            heat_haze_prev_minute: -1,
            under_waterness: 0.0,
            traffic_lights_brightness: 0.0,
            lightning_flash: false,
            lightning_burst: false,
            lightning_start_frame: 0,
            lightning_last_change: 0,
            lightning_duration: 0,
            when_thunder: 0,
            thunder: false,
            rain_recently: false,
            rain_stop_countdown: 0,
            mist_alpha: 0.0,
            streak_count: 0,
            streaks_xyz: None,
            streaks_a: [0; 32],
            streaks: Vec::new(),
        }
    }
}

impl Weather {
    fn blend(&self, set: &[i16]) -> f32 {
        let i = self.interpolation;
        (if set.contains(&self.old_type) { 1.0 - i } else { 0.0 }) + if set.contains(&self.new_type) { i } else { 0.0 }
    }

    /// `FindWeatherTypesList` (0x72A520).
    fn list(&self) -> &'static [i8; 64] {
        &CYCLES[if (1..=4).contains(&self.region) { self.region as usize } else { 0 }]
    }

    /// `UpdateWeatherRegion` (0x72A640).
    pub fn update_region(&mut self, x: f32, y: f32) {
        self.region = if x > 1000.0 && y > 910.0 {
            3
        } else if x > -850.0 && x < 1000.0 && y > 1280.0 {
            4
        } else if x < -1430.0 && y > -580.0 && y < 1430.0 {
            2
        } else if x > 250.0 && x < 3000.0 && y > -3000.0 && y < -850.0 {
            1
        } else {
            0
        };
    }

    /// `ForceWeatherNow` (0x72A4F0).
    pub fn force_now(&mut self, t: i16) {
        self.forced_type = t;
        self.old_type = t;
        self.new_type = t;
    }

    /// `ReleaseWeather` (0x72A510).
    pub fn release(&mut self) {
        self.forced_type = -1;
    }

    /// `SetWeatherToAppropriateTypeNow` (0x72A790), from the player's position.
    pub fn set_appropriate_now(&mut self, x: f32, y: f32) {
        self.update_region(x, y);
        self.forced_type = -1;
        let t = self.list()[0] as i16;
        self.old_type = t;
        self.new_type = t;
    }
}

/// `approach(var, target)` from Update 3.6.
fn approach(v: &mut f32, target: f32, step: f32) {
    let d = target - *v;
    if d.abs() < step {
        *v = target;
    } else if d > 0.0 {
        *v += step;
    } else {
        *v -= step;
    }
}

impl World {
    fn rand_f(&mut self) -> f32 {
        self.rng.rand01()
    }

    /// `CClock::Update` + `CWeather::Update` (0x72B850), as `CGame::Process` runs them.
    pub(crate) fn update_clock_and_weather(&mut self, ts: f32) {
        let now = self.now_ms;
        self.clock.update(now);
        let cam = self.camera_pos;
        let (hours, minutes, seconds) = (self.clock.hours, self.clock.minutes, self.clock.seconds);

        // Region refresh every 16 frames.
        if self.frame as u8 & 0x0F == 0 {
            self.weather.update_region(cam.x, cam.y);
        }
        // Hourly step.
        {
            let w = &mut self.weather;
            let new_i = ((seconds as u8) as f32 * (1.0 / 60.0) + minutes as f32) * (1.0 / 60.0);
            if new_i < w.interpolation {
                w.update_region(cam.x, cam.y);
                w.old_type = w.new_type;
                if w.forced_type >= 0 {
                    w.new_type = w.forced_type;
                } else if cam.z < 950.0 {
                    w.type_in_list = (w.type_in_list + 1) % 64;
                    w.new_type = w.list()[w.type_in_list as usize] as i16;
                }
            }
            w.interpolation = new_i;
        }

        // Lightning.
        let rainy_both = RAINY.contains(&self.weather.new_type) && RAINY.contains(&self.weather.old_type);
        if rainy_both && self.weather.under_waterness <= 0.0 {
            if !self.weather.lightning_burst {
                if self.rng.next() & 0xFFFF < 200 {
                    let w = &mut self.weather;
                    w.lightning_start_frame = self.frame;
                    w.lightning_last_change = now;
                    w.lightning_burst = true;
                    w.lightning_flash = true;
                } else {
                    self.weather.lightning_flash = false;
                }
            } else if self.rng.next() & 0xFF < 24 {
                let w = &mut self.weather;
                w.lightning_burst = false;
                w.lightning_duration = self.frame.wrapping_sub(w.lightning_start_frame).min(20);
                w.when_thunder = now + (20 - w.lightning_duration) * 150;
                w.lightning_flash = false;
            } else if now.wrapping_sub(self.weather.lightning_last_change) > 50 {
                let old = self.weather.lightning_flash;
                self.weather.lightning_flash = self.rng.next() & 1 != 0;
                if self.weather.lightning_flash != old {
                    self.weather.lightning_last_change = now;
                }
            }
        } else {
            self.weather.lightning_flash = false;
            self.weather.lightning_burst = false;
        }
        if self.weather.when_thunder != 0 && now > self.weather.when_thunder {
            self.weather.thunder = true;
            self.weather.when_thunder = 0;
        }

        let w = &mut self.weather;
        let i = w.interpolation;
        let (o, n) = (w.old_type, w.new_type);
        // WetRoads.
        w.wet_roads = if RAINY.contains(&o) {
            if RAINY.contains(&n) { 1.0 } else { 1.0 - i }
        } else if RAINY.contains(&n) {
            i
        } else {
            0.0
        };
        // Rain and sandstorm.
        let rain_mod = ((now >> 13) & 3) as f32 * 0.1 + 0.7;
        let step = ts * 0.005;
        let (rain_t, sand_t) = (rain_mod * w.blend(RAINY), rain_mod * w.blend(SANDSTORM));
        approach(&mut w.rain, rain_t, step);
        approach(&mut w.sandstorm, sand_t, step);
        // Clouds, fog, sun.
        let sunny_or_extra = |t: i16| EXTRASUNNY.contains(&t) || SUNNY.contains(&t);
        w.cloud_coverage = (if !sunny_or_extra(o) { 1.0 - i } else { 0.0 }) + if !sunny_or_extra(n) { i } else { 0.0 };
        w.foggyness = w.blend(FOGGY);
        w.foggyness_sf = w.blend(FOGGY_SF);
        w.extra_sunnyness = w.blend(EXTRASUNNY);
        w.rainbow = if CLOUDY.contains(&o) && SUNNY.contains(&n) && i < 0.5 && hours > 6 && hours < 21 {
            1.0 - (i - 0.25).abs() * 4.0
        } else {
            0.0
        };
        // Heat haze.
        w.heat_haze = w.blend(HEATHAZE);
        if w.heat_haze > 0.0 {
            let changed = minutes as i32 != w.heat_haze_prev_minute;
            let mut out = 0.0;
            if (10..19).contains(&hours) {
                if changed {
                    w.heat_haze_acc += 0.05;
                }
                if w.heat_haze_acc > 1.0 {
                    w.heat_haze_acc = 1.0;
                }
                out = w.heat_haze_acc;
            }
            if hours >= 19 {
                if changed {
                    w.heat_haze_acc -= 0.05;
                }
                if w.heat_haze_acc < 0.0 {
                    w.heat_haze_acc = 0.0;
                }
                out = w.heat_haze_acc;
            }
            w.heat_haze_fx_control = out * w.heat_haze;
            w.heat_haze_prev_minute = minutes as i32;
        }
        // Wind.
        w.wind = WIND_PER_TYPE[o as usize] * (1.0 - i) + WIND_PER_TYPE[n as usize] * i;
        w.wind_clipped = if w.wind > 1.0 { 1.0 } else { w.wind };
        let wc = w.wind_clipped;
        let lerp = |t: &[f32; 16], k: usize, f: f32| (1.0 - f) * t[k & 15] + f * t[(k + 1) & 15];
        let pi = std::f32::consts::PI;
        let ii = ((now >> 10) & 15) as usize;
        let f = 0.5 - ((now & 0x3FF) as f32 * (1.0 / 1024.0) * pi).cos() * 0.5;
        let mut x = lerp(&WIND_D, ii, f) * wc * 0.4 + wc * 0.7;
        let mut y = lerp(&WIND_D, ii + 3, f) * wc * 0.4 + wc * 0.7;
        let mut z = lerp(&WIND_D, ii + 6, f) * wc * 0.2;
        let g = (wc - 0.5) * 0.4;
        if g > 0.0 {
            let j = ((now >> 8) & 15) as usize;
            let u = (now & 0xFF) as f32 * (1.0 / 256.0);
            x += lerp(&WIND_D, j, u) * g;
            y += lerp(&WIND_D, j + 3, u) * g;
            z += lerp(&WIND_D, j + 6, u) * g;
        }
        let k = ((now >> 11) & 15) as usize;
        let h = 0.5 - ((now & 0x7FF) as f32 * (1.0 / 2048.0) * pi).cos() * 0.5;
        let s = lerp(&WIND_G, k, h);
        w.wind_dir = Vec3::new(s * x, s * y, s * z);
        // Rain clamp, traffic lights.
        if w.rain >= 1.0 - w.under_waterness {
            w.rain = 1.0 - w.under_waterness;
        }
        let mut tlb = if hours > 20 {
            1.0
        } else if hours == 20 {
            minutes as f32 * (1.0 / 60.0)
        } else if hours >= 7 {
            0.0
        } else if hours == 6 {
            1.0 - minutes as f32 * (1.0 / 60.0)
        } else {
            1.0
        };
        for v in [w.wet_roads, w.foggyness, w.rain] {
            if tlb <= v {
                tlb = v;
            }
        }
        w.traffic_lights_brightness = tlb;

        self.add_rain();
        self.update_rain_streaks();

        // CGame::Process: CTimeCycle::Update after the weather.
        let (w, c, cam) = (&self.weather, &self.clock, self.camera_pos);
        if let Some(tc) = self.timecycle.as_mut() {
            tc.lights_mult = self.coronas.lights_mult;
            tc.calc(c.hours, c.minutes, c.seconds, w.old_type, w.new_type, w.interpolation, cam, w.under_waterness, 0.0);
        }
        // CGame::Process: CCoronas::DoSunAndMoon, then CCoronas::Update.
        self.do_sun_and_moon();
        self.update_coronas(ts);
    }

    /// `CWeather::AddRain` (0x72A9A0): ground splashes and rain mist (and sandstorm).
    fn add_rain(&mut self) {
        let cam = self.camera_pos;
        if self.weather.under_waterness > 0.0 || cam.z > 900.0 {
            return;
        }
        if self.weather.rain > 0.0 {
            self.weather.rain_recently = true;
            self.weather.rain_stop_countdown = 800;
        } else if self.weather.rain_recently {
            if self.weather.rain_stop_countdown > 0 {
                self.weather.rain_stop_countdown -= 1;
            } else {
                self.weather.rain_recently = false;
                self.weather.rain_stop_countdown = 800;
            }
        }
        if self.weather.wind > 1.01 && self.weather.under_waterness <= 0.0 {
            self.add_sandstorm_particles();
        }
        if !(self.weather.rain > 0.1) && self.weather.mist_alpha == 0.0 {
            return;
        }
        let rain = self.weather.rain;
        // Ground splashes.
        let n = (rain * 5.0) as i32;
        let radius = 40.0f32; // max(Rain*10, 40)
        let per = 15 - (rain * -2.0) as i32;
        let mult = PrtMult::new(1.0, 1.0, 1.0, 0.25, 0.02, 0.0, 0.03);
        for _ in 0..n {
            let dist = self.rand_f() * (radius * 0.5);
            let r = self.rng.next();
            let ang = if r & 1 != 0 {
                (self.rng.next() & 0xFF) as f32 * 0.024_531_25
            } else {
                ((r & 0xFF) as f32 - 128.0) * 0.00625 + self.camera_orientation
            };
            let (x, y) = (ang.sin() * dist + cam.x, ang.cos() * dist + cam.y);
            let hit = self.line_of_sight(Vec3::new(x, y, 40.0), Vec3::new(x, y, -40.0), true, None);
            if let Some((_, _, cp)) = hit {
                let z = cp.point.z + 0.1;
                for _ in 0..per {
                    let px = x + self.rand_f() * 30.0 - 15.0;
                    let py = y + self.rand_f() * 30.0 - 15.0;
                    self.rng.next(); // third rand(), result unused
                    let p = Vec3::new(px, py, z);
                    self.effects.add_particle("prt_splash", p, Vec3::ZERO, 0.0, mult, -1.0, 1.2, 0.6, false);
                }
            }
        }
        // Rain mist.
        let w = &mut self.weather;
        let t = rain * 0.2;
        if w.mist_alpha < t {
            w.mist_alpha += 0.0025;
        }
        if w.mist_alpha > t {
            w.mist_alpha -= 0.0025;
        }
        w.mist_alpha = (w.mist_alpha.max(0.0) * 1.0).min(1.0);
        let mult = PrtMult::new(0.9, 0.9, 1.0, w.mist_alpha, 1.0, 0.0, 0.2);
        let vel = w.wind_dir * 15.0;
        let fwd = self.camera_fwd;
        let px = cam.x + fwd.x * 10.0 + self.rand_f() * 40.0 - 20.0;
        let py = cam.y + fwd.y * 10.0 + self.rand_f() * 40.0 - 20.0;
        let pz = cam.z + self.rand_f() * 7.0 - 2.0;
        self.effects.add_particle("prt_sand2", Vec3::new(px, py, pz), vel, 0.0, mult, -1.0, 1.2, 0.6, false);
    }

    /// `CWeather::AddSandStormParticles` (0x72A820).
    fn add_sandstorm_particles(&mut self) {
        let cam = self.camera_pos;
        let fwd = self.camera_fwd;
        let mult = PrtMult::new(0.67, 0.65, 0.55, 0.25, 1.0, 0.0, 0.2);
        let px = cam.x + fwd.x * 10.0 + self.rand_f() * 40.0 - 20.0;
        let py = cam.y + fwd.y * 10.0 + self.rand_f() * 40.0 - 20.0;
        let pz = cam.z + self.rand_f() * 7.0 - 2.0;
        let vel = self.weather.wind_dir * 25.0;
        self.effects.add_particle("prt_sand2", Vec3::new(px, py, pz), vel, 0.0, mult, -1.0, 1.2, 0.6, false);
    }

    /// `CWeather::RenderRainStreaks` (0x72AF70), simulation part (once per game frame).
    fn update_rain_streaks(&mut self) {
        let cam = self.camera_pos;
        let fwd = self.camera_fwd;
        let fog_reduction = 0.0f32; // CTimeCycle FogReduction, not ported
        self.weather.streaks.clear();
        let target = (((self.weather.rain * 110.0) as i32) as f32 * (64.0 - fog_reduction) * (1.0 / 64.0)) as i32;
        let w = &mut self.weather;
        if w.streak_count < target {
            w.streak_count += 1;
        } else if w.streak_count > target {
            w.streak_count -= 1;
        }
        w.streak_count = w.streak_count.max(0);
        if w.streak_count == 0 || w.under_waterness > 0.0 || cam.z > 900.0 {
            return;
        }
        let n = w.streak_count;
        if w.streaks_xyz.is_none() {
            w.streaks_xyz = Some([[0; 3]; 32]);
            w.streaks_a = [(n as f32 * 0.6) as i32 as u8; 32];
        }
        let (wind, rain, wind_dir) = (w.wind, w.rain, w.wind_dir);
        for i in 0..32 {
            let mut p = self.weather.streaks_xyz.unwrap()[i];
            let mut a = self.weather.streaks_a[i];
            let rel = Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) - cam;
            if a == 0 || p[2] as f32 <= 0.0 || rel.length() > 8.0 {
                p[0] = (self.rand_f() * 5.0 + fwd.x * 6.0 + cam.x - 2.5) as i32;
                p[1] = (self.rand_f() * 5.0 + fwd.y * 6.0 + cam.y - 2.5) as i32;
                p[2] = (self.rand_f() * 5.0 + fwd.z * 6.0 + cam.z - 2.5) as i32;
                a = (n as f32 * 0.6) as i32 as u8;
            }
            let k = if i & 1 != 0 { wind * 0.1 } else { wind * rain * 0.1 };
            let base = Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32);
            let t = base - wind_dir * k;
            let s = self.rand_f();
            let dx = self.rand_f() * 0.4 - 0.2;
            let dy = self.rand_f() * 0.4 - 0.2;
            let dz = self.rand_f() * 0.2 - 0.1;
            self.weather.streaks.push(RainStreak {
                bottom: base + Vec3::new(dx, dy, dz),
                top: Vec3::new(t.x + dx, t.y + dy, s * 0.4 + 0.1 + dz + t.z),
                alpha: a,
            });
            p[2] = (p[2] as f32 - (self.rand_f() * 0.09 + 0.01)) as i32;
            let d = 2 + ((self.rng.next() & 0xFFFF) as f32 * (1.0 / 32768.0) * 3.0) as u8;
            a = if a > d { a - d } else { 0 };
            if let Some(arr) = self.weather.streaks_xyz.as_mut() {
                arr[i] = p;
            }
            self.weather.streaks_a[i] = a;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rainy_hours_bring_rain_wind_and_wet_roads() {
        let mut w = World::default();
        w.camera_pos = Vec3::new(-2000.0, 500.0, 10.0); // SF
        w.weather.update_region(-2000.0, 500.0);
        assert_eq!(w.weather.region, 2);
        w.weather.force_now(8); // RAINY_SF
        for _ in 0..400 {
            w.process(1.0);
        }
        let wt = &w.weather;
        assert_eq!(wt.wet_roads, 1.0);
        assert!(wt.rain >= 0.7 && wt.rain <= 1.0, "rain {}", wt.rain);
        assert_eq!(wt.wind, 1.0);
        assert!(wt.wind_dir.length() > 0.0);
    }

    #[test]
    fn weather_steps_every_game_hour() {
        let mut w = World::default();
        let before = w.weather.type_in_list;
        // One game hour = 60 s = 3000 ticks of 20 ms.
        for _ in 0..3100 {
            w.process(1.0);
        }
        assert_eq!(w.weather.type_in_list, before + 1);
        assert_eq!(w.clock.hours, 13);
    }
}
