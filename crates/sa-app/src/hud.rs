//! The in-game HUD (hud.md) drawn with a port of CFont (font.md): `CHud::DrawPlayerInfo`
//! (clock, money, weapon icon, ammo, health / armour / breath bars) and `CHud::DrawWanted`.
//! Everything is laid out in the 640×448 virtual screen scaled per axis, and drawn as 2D
//! quads (glyphs from fonts.txd with the exact UV insets, bars as untextured rects).
//!
//! `CHud::DrawRadar` / `CRadar` (hud.md §6–7): the 3×3 radarNN map tiles around the player
//! rotated with the camera heading, clipped to the disc's 24-gon, the radardisc ring, the
//! north blip and the player arrow.
//!
//! Not ported: radar blips other than north and the player, the plane horizon / altimeter,
//! zone / vehicle name popups (need GXT), the help box, messages, the vital-stats panel, the
//! 2-player layout, button icons.

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    camera::ClearColorConfig,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use sa_formats::{fonts::FontValues, txd};
use sa_physics::ped::PedLogic;

use crate::{
    player::{GameRoot, Ped},
    saphys::{SaPhys, SaPhysExt},
    stream::{convert_texture, make_image},
};

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_hud).add_systems(PostUpdate, draw_hud);
    }
}

/// `CHudColours` (0xBAB22C).
pub const HUD_COLOURS: [[u8; 3]; 15] = [
    [180, 25, 29],
    [54, 104, 44],
    [50, 60, 127],
    [172, 203, 241],
    [225, 225, 225],
    [0, 0, 0],
    [144, 98, 16],
    [168, 110, 252],
    [150, 150, 150],
    [104, 15, 17],
    [38, 71, 31],
    [226, 192, 99],
    [74, 90, 107],
    [20, 25, 200],
    [255, 255, 0],
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Tex {
    White,
    Font(u8),
    Image(AssetId<Image>),
}

/// One 2D quad: screen rect (pixels, y down), UVs TL TR BL BR, colour.
struct Quad {
    tex: Tex,
    l: f32,
    t: f32,
    r: f32,
    b: f32,
    uv: [[f32; 2]; 4],
    col: [u8; 4],
    /// A triangle fan (screen point, uv) instead of the rect, when not empty.
    fan: Vec<([f32; 2], [f32; 2])>,
}

#[derive(Default)]
struct DrawList {
    images: HashMap<AssetId<Image>, Handle<Image>>,
    quads: Vec<Quad>,
    /// Glyph quads are buffered and drawn after everything else (RenderFontBuffer at DrawFonts).
    text: Vec<Quad>,
}

impl DrawList {
    /// `CSprite2d::DrawRect`.
    fn rect(&mut self, l: f32, t: f32, r: f32, b: f32, col: [u8; 4]) {
        self.quads.push(Quad { tex: Tex::White, l, t, r, b, uv: [[0.0; 2]; 4], col, fan: Vec::new() });
    }

    /// A textured (or untextured) triangle fan.
    fn fan(&mut self, img: Option<&Handle<Image>>, pts: Vec<([f32; 2], [f32; 2])>, col: [u8; 4]) {
        let tex = match img {
            Some(h) => {
                self.images.insert(h.id(), h.clone());
                Tex::Image(h.id())
            }
            None => Tex::White,
        };
        self.quads.push(Quad { tex, l: 0.0, t: 0.0, r: 0.0, b: 0.0, uv: [[0.0; 2]; 4], col, fan: pts });
    }

    fn sprite(&mut self, img: &Handle<Image>, l: f32, t: f32, r: f32, b: f32, col: [u8; 4]) {
        self.images.insert(img.id(), img.clone());
        self.quads.push(Quad { tex: Tex::Image(img.id()), l, t, r, b, uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]], col, fan: Vec::new() });
    }

    /// `CSprite2d::DrawBarChart` (0x728640) as the HUD calls it (border on, no add / text).
    fn bar_chart(&mut self, sc: &Scale, x: f32, y: f32, width: u16, height: u8, progress: f32, col: [u8; 3]) {
        let progress = progress.max(0.0);
        let right = x + width as f32;
        let bottom = y + height as f32;
        let fill = (x + progress * 0.01 * width as f32).min(right);
        self.rect(x, y, fill, bottom, [col[0], col[1], col[2], 255]);
        let half = [(col[0] as f32 * 0.5) as u8, (col[1] as f32 * 0.5) as u8, (col[2] as f32 * 0.5) as u8, 255];
        self.rect(fill, y, right, bottom, half);
        let (bx, by) = (2.0 * sc.sx(1.0), 2.0 * sc.sy(1.0));
        let k = [0, 0, 0, 255];
        self.rect(x, y, right, y + by, k);
        self.rect(x, bottom - by, right, bottom, k);
        self.rect(x, y, x + bx, bottom, k);
        self.rect(right - bx, y, right, bottom, k);
    }
}

/// `SX(v)` / `SY(v)`: the 640×448 virtual screen.
#[derive(Clone, Copy)]
struct Scale {
    w: f32,
    h: f32,
}

impl Scale {
    fn sx(&self, v: f32) -> f32 {
        self.w * 0.001_562_5 * v
    }
    fn sy(&self, v: f32) -> f32 {
        self.h * 0.002_232_143 * v
    }
}

/// `CFont::Details` (font.md §0.1).
#[derive(Clone)]
struct Font {
    colour: [u8; 4],
    scale: (f32, f32),
    centre: bool,
    right: bool,
    justify: bool,
    proportional: bool,
    wrap_x: f32,
    centre_size: f32,
    right_wrap: f32,
    tex: u8,
    style: u8,
    shadow: i8,
    drop: [u8; 4],
    edge: i8,
    edge_adv: i8,
    is_shadow_pass: bool,
    new_line: bool,
}

impl Font {
    fn new(w: f32) -> Self {
        Self {
            colour: [255, 255, 255, 0],
            scale: (1.0, 1.0),
            centre: false,
            right: false,
            justify: false,
            proportional: true,
            wrap_x: w,
            centre_size: w,
            right_wrap: 0.0,
            tex: 0,
            style: 0,
            shadow: 0,
            drop: [0; 4],
            edge: 0,
            edge_adv: 0,
            is_shadow_pass: false,
            new_line: false,
        }
    }

    /// `SetFontStyle` (0x719490).
    fn set_font_style(&mut self, s: u8) {
        (self.tex, self.style) = match s {
            2 => (0, 2),
            3 => (1, 1),
            s => (s, 0),
        };
    }

    /// `SetOrientation` (0x719610): 0 centre, 1 left, 2 right.
    fn set_orientation(&mut self, o: u8) {
        (self.centre, self.right) = match o {
            0 => (true, false),
            2 => (false, true),
            _ => (false, false),
        };
    }

    /// `SetEdge` clears the shadow, `SetDropShadowPosition` clears the edge.
    fn set_edge(&mut self, e: i8) {
        self.edge = e;
        self.edge_adv = e;
        self.shadow = 0;
    }

    fn set_drop_shadow(&mut self, s: i8) {
        self.shadow = s;
        self.edge = 0;
        self.edge_adv = 0;
    }
}

/// Glyph index of a GXT char (font.md §2.2).
fn glyph_index(c: u8, style: u8) -> u8 {
    let mut idx = c.wrapping_sub(0x20);
    if style == 0 {
        if idx == 0x91 {
            idx = 0x40;
        } else if idx > 0x9B {
            idx = 0;
        }
        return idx;
    }
    // FindNewCharacter (0x7192C0).
    if style == 1 {
        match idx {
            0x1A => return 0x9A,
            0x08 | 0x09 => return idx + 0x56,
            0x04 => return 0x5D,
            0x07 => return 0xCE,
            0x0E => return 0xCF,
            0x01 => return 0xD0,
            _ => {}
        }
    }
    match idx {
        0x8F => 0xCD,
        0x1F => 0x5B,
        0x06 => 0x0A,
        0x3E => 0x20,
        0x10..=0x19 => idx + 0x80,
        0x21..=0x3A => idx + 0x7A,
        0x41..=0x5A => idx + 0x5A,
        0x60..=0x76 => idx + 0x55,
        0x77..=0x8C => idx + 0x3E,
        0x8D | 0x8E => 0xCC,
        _ => idx,
    }
}

struct FontData {
    vals: Vec<FontValues>,
}

impl FontData {
    /// Width (texels) of a glyph index for the current proportional / texture state.
    fn width(&self, f: &Font, idx: u8) -> f32 {
        let v = self.vals.get(f.tex as usize);
        let idx = if idx == 0x3F { 0 } else { idx };
        v.map_or(0.0, |v| if f.proportional { v.prop(idx as usize) } else { v.unprop() } as f32)
    }

    /// `GetCharacterSize(idx)` (0x719750).
    fn char_size(&self, f: &Font, c: u8) -> f32 {
        let idx = glyph_index(c, f.style);
        (self.width(f, idx) + f.edge as f32) * f.scale.0
    }

    /// `GetStringWidth(text, bFull=0)` (0x71A0E0): the first word, tokens count 0.
    fn string_width(&self, f: &Font, s: &[u8]) -> f32 {
        let (mut w, mut saw, mut token_stop) = (0.0f32, false, false);
        let mut i = 0;
        while i < s.len() {
            let c = s[i];
            if c == b' ' {
                break;
            }
            if c == b'~' {
                if token_stop || saw {
                    break;
                }
                i += 1;
                while i < s.len() && s[i] != b'~' {
                    i += 1;
                }
                i += 1;
                if saw || s.get(i) == Some(&b'~') {
                    token_stop = true;
                }
                continue;
            }
            w += self.char_size(f, c);
            i += 1;
            saw = true;
        }
        w
    }
}

/// `ParseToken`: returns (index past the closing '~', colour change).
fn parse_token(s: &[u8], at: usize, colour: &mut [u8; 4], new_line: &mut bool, change: bool) -> usize {
    let t = s.get(at + 1).copied().unwrap_or(0);
    let set = |c: &mut [u8; 4], i: usize| {
        let h = HUD_COLOURS[i];
        c[0] = h[0];
        c[1] = h[1];
        c[2] = h[2];
    };
    if change {
        match t {
            b'r' | b'R' => set(colour, 0),
            b'g' | b'G' => set(colour, 1),
            b'b' | b'B' => set(colour, 2),
            b'w' | b'W' | b's' | b'S' => set(colour, 4),
            b'y' | b'Y' => set(colour, 11),
            b'p' | b'P' => set(colour, 7),
            b'l' => set(colour, 5),
            b'h' | b'H' => {
                for c in colour.iter_mut().take(3) {
                    *c = (*c as f32 * 1.5).min(255.0) as u8;
                }
            }
            _ => {}
        }
    }
    if matches!(t, b'n' | b'N') {
        *new_line = true;
    }
    let mut i = at + 1;
    while i < s.len() && s[i] != b'~' {
        i += 1;
    }
    i + 1
}

impl FontData {
    /// Internal `PrintString(x, y, start, end, justifySpace)` (0x719B40) + `RenderFontBuffer` /
    /// `PrintChar`: the shadow or outline passes, then the main pass.
    fn print_line(&self, f: &mut Font, sc: &Scale, x: f32, y: f32, s: &[u8], gap: f32, out: &mut DrawList) {
        let saved = f.colour;
        if f.shadow != 0 {
            let sh = f.shadow;
            f.colour = f.drop;
            f.is_shadow_pass = true;
            f.shadow = 0;
            self.print_line(f, sc, x + sc.sx(sh as f32), y + sc.sy(sh as f32), s, gap, out);
            f.colour = saved;
            f.shadow = sh;
            f.is_shadow_pass = false;
        } else if f.edge != 0 {
            let e = f.edge as f32;
            f.colour = f.drop;
            f.is_shadow_pass = true;
            f.edge = 0;
            let (ex, ey) = (sc.sx(e), sc.sy(e));
            for (dx, dy) in [(ex, -ey), (-ex, -ey), (ex, ey), (-ex, ey), (ex, 0.0), (-ex, 0.0), (0.0, ey), (0.0, -ey)] {
                self.print_line(f, sc, x + dx, y + dy, s, gap, out);
            }
            f.colour = saved;
            f.edge = e as i8;
            f.is_shadow_pass = false;
        }
        // The main (or pass) glyphs.
        let mut col = f.colour;
        let (sx, sy) = f.scale;
        let mut px = x;
        let mut i = 0;
        while i < s.len() {
            while i < s.len() && s[i] == b'~' {
                let mut nl = false;
                i = parse_token(s, i, &mut col, &mut nl, !f.is_shadow_pass);
                if f.is_shadow_pass {
                    col = f.colour;
                }
            }
            if i >= s.len() {
                break;
            }
            let idx = glyph_index(s[i], f.style);
            // PrintChar (0x718A10): whole-cell quad, top-left culling.
            if (0.0..=sc.h).contains(&y) && (0.0..=sc.w).contains(&px) && idx != 0 && idx != 0x3F {
                let g = if f.style == 1 && idx == 0xD0 { 0 } else { idx };
                if g != 0 {
                    let u = (g & 15) as f32 * 0.0625;
                    let v = (g >> 4) as f32 * 0.078125;
                    let (bottom, bl, br) = if g >= 0xC0 {
                        (y + 16.0 * sy, v + 0.078125 - 0.016, v + 0.078125 - 0.015)
                    } else {
                        (y + 20.0 * sy, v + 0.076025, v + 0.076025)
                    };
                    out.text.push(Quad {
                        tex: Tex::Font(f.tex),
                        l: px,
                        t: y,
                        r: px + 32.0 * sx,
                        b: bottom,
                        uv: [[u, v + 0.0021], [u + 0.0615, v + 0.0021], [u, bl], [u + 0.0615, br]],
                        col,
                        fan: Vec::new(),
                    });
                }
            }
            px += (f.edge_adv as f32 + self.width(f, idx)) * sx;
            if idx == 0 {
                px += gap;
            }
            i += 1;
        }
        if !f.is_shadow_pass {
            f.colour = [col[0], col[1], col[2], f.colour[3]];
        }
    }

    /// Public `PrintString(x, y, text)` (0x71A700) → `ProcessCurrentString(1, …)` (0x71A220).
    fn print_string(&self, f: &mut Font, sc: &Scale, x: f32, y: f32, text: &str, out: &mut DrawList) {
        let s = text.as_bytes();
        if s.is_empty() || s[0] == b'*' {
            return;
        }
        let saved = f.colour;
        let (mut spaces, mut last_w, mut first) = (0i16, 0.0f32, true);
        let mut cur_x = if f.centre || f.right { 0.0 } else { x };
        let mut cur_y = y;
        let mut line_start = 0usize;
        let mut p = 0usize;
        while p < s.len() {
            let word = self.string_width(f, &s[p..]);
            if s[p] == b'~' {
                let mut c = f.colour;
                let mut nl = false;
                p = parse_token(s, p, &mut c, &mut nl, false);
                f.new_line |= nl;
            }
            let limit = if f.centre {
                f.centre_size
            } else if f.right {
                x - f.right_wrap
            } else {
                f.wrap_x
            };
            let new_x = cur_x + word;
            if (new_x > limit && !first) || f.new_line {
                let line_end = if f.new_line { p.saturating_sub(3) } else { p };
                let gap = if f.justify && !f.centre { (f.wrap_x - last_w) / spaces as f32 } else { 0.0 };
                let line_x = if f.centre {
                    x - cur_x * 0.5
                } else if f.right {
                    x - (cur_x - self.char_size(f, b' '))
                } else {
                    x
                };
                self.print_line(f, sc, line_x, cur_y, &s[line_start..line_end.max(line_start)], gap, out);
                f.new_line = false;
                cur_y += 18.0 * f.scale.1;
                cur_x = if f.centre || f.right { 0.0 } else { x };
                line_start = p;
                spaces = 0;
                last_w = 0.0;
                first = true;
            } else {
                cur_x = new_x;
                while p < s.len() && s[p] != b' ' && s[p] != b'~' {
                    p += 1;
                }
                if p >= s.len() {
                    let line_x = if f.centre {
                        x - cur_x * 0.5
                    } else if f.right {
                        x - cur_x
                    } else {
                        x
                    };
                    self.print_line(f, sc, line_x, cur_y, &s[line_start..], 0.0, out);
                } else {
                    if !first {
                        spaces += 1;
                    }
                    if s[p] != b'~' {
                        cur_x += self.char_size(f, b' ');
                        p += 1;
                    }
                    last_w = cur_x;
                    first = false;
                }
            }
        }
        f.colour = saved;
    }
}

#[derive(Resource)]
struct HudAssets {
    font_tex: [Handle<Image>; 2],
    white: Handle<Image>,
    fist: Option<Handle<Image>>,
    radar_disc: Option<Handle<Image>>,
    radar_north: Option<Handle<Image>>,
    radar_centre: Option<Handle<Image>>,
    data: FontData,
}

/// The 2D overlay camera.
#[derive(Component)]
struct HudCamera;

/// A HUD mesh (one per texture and layer, rebuilt each frame).
#[derive(Component)]
struct HudMesh;

fn setup_hud(mut commands: Commands, root: Res<GameRoot>, mut images: ResMut<Assets<Image>>) {
    let load_txd = |name: &str, images: &mut Assets<Image>| -> HashMap<String, Handle<Image>> {
        std::fs::read(root.0.join(name))
            .ok()
            .and_then(|d| txd::parse(&d).ok())
            .map(|t| {
                t.into_iter()
                    .filter_map(|t| convert_texture(t, false))
                    .map(|t| (t.name.to_ascii_lowercase(), images.add(make_image(t))))
                    .collect()
            })
            .unwrap_or_default()
    };
    let fonts = load_txd("models/fonts.txd", &mut images);
    let hud = load_txd("models/hud.txd", &mut images);
    // fonts.dat has Latin-1 comment bytes.
    let vals = std::fs::read(root.0.join("data/fonts.dat")).map(|b| sa_formats::fonts::parse_fonts_dat(&String::from_utf8_lossy(&b))).unwrap_or_default();
    if vals.len() < 2 {
        warn!("fonts.dat: {} fonts", vals.len());
    }
    let white = images.add(Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ));
    let (Some(f2), Some(f1)) = (fonts.get("font2").cloned(), fonts.get("font1").cloned()) else {
        warn!("fonts.txd: font1/font2 missing, no HUD");
        return;
    };
    commands.insert_resource(HudAssets {
        font_tex: [f2, f1],
        white,
        fist: hud.get("fist").cloned(),
        radar_disc: hud.get("radardisc").cloned(),
        radar_north: hud.get("radar_north").cloned(),
        radar_centre: hud.get("radar_centre").cloned(),
        data: FontData { vals },
    });
    commands.spawn((
        Camera2d,
        Camera { order: 5, clear_color: ClearColorConfig::None, ..default() },
        HudCamera,
    ));
}

fn srgb_col(c: [u8; 4]) -> [f32; 4] {
    let l = |v: u8| {
        let x = v as f32 / 255.0;
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    [l(c[0]), l(c[1]), l(c[2]), c[3] as f32 / 255.0]
}

/// `CHud::DrawPlayerInfo` + `CHud::DrawWanted`.
#[allow(clippy::too_many_arguments)]
fn draw_hud(
    mut commands: Commands,
    assets: Option<Res<HudAssets>>,
    sa: Res<SaPhys>,
    window: Single<&Window>,
    ped: Single<&Ped>,
    icons: Res<crate::weapons::WeaponIcons>,
    world_res: Res<crate::world::WorldRes>,
    mut tiles: Local<HashMap<usize, Option<Handle<Image>>>>,
    mut images: ResMut<Assets<Image>>,
    driving: Res<crate::vehicle::Driving>,
    cars: Query<&crate::vehicle::Vehicle>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut last_level: Local<i32>,
) {
    let Some(assets) = assets else { return };
    let sc = Scale { w: window.width(), h: window.height() };
    let (w, _h) = (sc.w, sc.h);
    let mut out = DrawList::default();
    let world = &sa.world;
    let frame = world.frame;
    let Some(l) = sa.logic::<PedLogic>(ped.sa) else { return };
    let t = &l.tasks;
    let health = &t.health;
    let max_h = health.max_health as u8;
    // GetYPosBasedOnHealth.
    let ypos = |y: f32, off: f32| if (max_h as f32) < 101.0 { y - sc.sy(off) } else { y };
    let fd = &assets.data;
    let mut f = Font::new(w);

    // Clock.
    f.scale = (sc.sx(0.55), sc.sy(1.1));
    f.proportional = false;
    f.set_font_style(3);
    f.set_orientation(2);
    f.right_wrap = 0.0;
    f.set_edge(2);
    f.drop = [0, 0, 0, 255];
    let lg = HUD_COLOURS[4];
    f.colour = [lg[0], lg[1], lg[2], 255];
    let clock = format!("{:02}:{:02}", world.clock.hours, world.clock.minutes);
    fd.print_string(&mut f, &sc, w - sc.sx(32.0), sc.sy(22.0), &clock, &mut out);
    f.set_edge(0);

    // Money.
    let m = world.display_money;
    let (col, text) = if m < 0 { (HUD_COLOURS[0], format!("-${:07}", -m)) } else { (HUD_COLOURS[1], format!("${:08}", m)) };
    f.proportional = false;
    f.scale = (sc.sx(0.55), sc.sy(1.1));
    f.set_orientation(2);
    f.right_wrap = 0.0;
    f.set_font_style(3);
    f.set_drop_shadow(0);
    f.set_edge(2);
    f.drop = [0, 0, 0, 255];
    f.colour = [col[0], col[1], col[2], 255];
    fd.print_string(&mut f, &sc, w - sc.sx(32.0), ypos(sc.sy(89.0), 12.0), &text, &mut out);
    f.set_edge(0);

    // Weapon icon (fist or "<model>icon").
    let wpn = *t.active_weapon();
    let info = t.infos.as_deref().map(|i| i.get(wpn.ty, 1).clone());
    let ix = (w - (sc.sx(32.0) + w * 0.173_430_46)) as i32 as f32;
    let iy = sc.sy(20.0) as i32 as f32;
    let model = info.as_ref().map_or(-1, |i| i.model1);
    if model <= 0 {
        if let Some(fist) = &assets.fist {
            out.sprite(fist, ix, iy, ix + sc.sx(47.0), iy + sc.sy(58.0), [255, 255, 255, 255]);
        }
    } else if let Some(Some(img)) = icons.0.get(&model) {
        out.sprite(img, ix, iy, ix + sc.sx(47.0), iy + sc.sy(58.0), [255, 255, 255, 255]);
    }

    // Ammo "reserve-clip".
    if let Some(info) = info.as_ref() {
        let total = wpn.total_ammo;
        let clip = wpn.ammo_in_clip;
        let clip_size = t.info_of(wpn.ty).map_or(0, |i| i.ammo_clip);
        let text = if !(2..=999).contains(&clip_size) {
            format!("{total}")
        } else if wpn.ty == 37 {
            format!("{}-{}", ((total - clip) / 10).min(9999), clip / 10)
        } else {
            format!("{}-{}", (total - clip).min(9999), clip)
        };
        let shown = total.saturating_sub(clip) < 9999
            && !matches!(wpn.ty, 0 | 40 | 10..=15 | 46)
            && info.fire_type != sa_physics::weapon::fire::USE
            && info.slot > 1;
        if shown {
            f.scale = (sc.sx(0.3), sc.sy(0.7));
            f.set_orientation(0);
            f.centre_size = sc.sx(640.0);
            f.proportional = true;
            f.set_edge(1);
            f.drop = [0, 0, 0, 255];
            f.set_font_style(1);
            let lb = HUD_COLOURS[3];
            f.colour = [lb[0], lb[1], lb[2], 255];
            let ax = (w - (w * 0.173_430_46 + sc.sx(32.0)) + sc.sx(47.0) * 0.5) as i32 as f32;
            let ay = (sc.sy(20.0) + sc.sy(43.0)) as i32 as f32;
            fd.print_string(&mut f, &sc, ax, ay, &text, &mut out);
            f.set_edge(0);
        }
    }

    // Health bar (flashing below 10 hp).
    let hp16 = health.health as i16;
    if !(hp16 < 10 && frame & 8 == 0) {
        let x = (w - sc.sx(141.0)) as i32 as f32;
        let y = ypos(sc.sy(77.0), 10.0) as i32 as f32;
        let width = (sc.sx(1.0) * max_h as f32 * 109.0 / 176.0) as i32 as u16;
        let pct = health.health * 100.0 / max_h.max(1) as f32;
        out.bar_chart(&sc, x + sc.sx(109.0) - width as f32, y, width, sc.sy(9.0) as i32 as u8, pct, HUD_COLOURS[0]);
    }
    // Armour bar.
    if health.armour > 1.0 {
        let x = (w - sc.sx(94.0)) as i32 as f32;
        let y = ypos(sc.sy(48.0), 3.0) as i32 as f32;
        let pct = health.armour / 100.0 * 100.0;
        out.bar_chart(&sc, x, y, sc.sx(62.0) as i32 as u16, sc.sy(9.0) as i32 as u8, pct, HUD_COLOURS[4]);
    }
    // Breath bar (in water, or refilling for 500 ms after).
    let max_breath = sa_physics::ped::BREATH_MAX;
    if t.swim.is_some() {
        let x = (w - sc.sx(94.0)) as i32 as f32;
        let y = ypos(sc.sy(62.0), 6.0) as i32 as f32;
        let pct = t.pd.breath / max_breath * 100.0;
        out.bar_chart(&sc, x, y, sc.sx(62.0) as i32 as u16, sc.sy(9.0) as i32 as u8, pct, HUD_COLOURS[3]);
    }

    // DrawWanted.
    let wanted = &world.wanted;
    let level = wanted.level;
    let parole = wanted.level_before_parole;
    let unchanged = level == *last_level;
    *last_level = level;
    if (level > 0 && unchanged) || parole > 0 {
        f.scale = (sc.sx(0.605), sc.sy(1.21));
        f.set_orientation(2);
        f.proportional = true;
        f.set_font_style(0);
        let mut x = w - sc.sx(29.0);
        let y = ypos(sc.sy(114.0), 12.0);
        let now = world.now_ms;
        for i in 0..6 {
            f.set_edge(1);
            f.drop = [0, 0, 0, 255];
            f.scale = (sc.sx(0.605), sc.sy(1.21));
            if i < level && (now > wanted.last_time_level_changed.wrapping_add(2000) || frame & 4 != 0) {
                let g = HUD_COLOURS[6];
                f.colour = [g[0], g[1], g[2], 255];
                fd.print_string(&mut f, &sc, x, y, "]", &mut out);
            } else if i < parole && frame & 4 != 0 {
                let g = HUD_COLOURS[6];
                f.colour = [(g[0] as f32 * 0.8) as u8, (g[1] as f32 * 0.8) as u8, (g[2] as f32 * 0.8) as u8, 255];
                fd.print_string(&mut f, &sc, x, y, "]", &mut out);
            } else if i >= level {
                f.set_edge(0);
                f.colour = [0, 0, 0, (255.0 * 0.7) as u8];
                f.scale = (sc.sx(0.605) * 1.2, sc.sy(1.21) * 1.2);
                fd.print_string(&mut f, &sc, x, y - 2.0 * sc.sy(1.0), "]", &mut out);
            }
            x -= sc.sx(18.0);
        }
        f.set_edge(0);
    }

    // CHud::DrawRadar.
    let veh = driving.0.and_then(|e| cars.get(e).ok()).map(|v| v.sa);
    let player_pos = sa.world.body(veh.unwrap_or(ped.sa)).map(|b| b.phys.matrix.pos);
    let heading = sa.world.body(veh.unwrap_or(ped.sa)).map_or(0.0, |b| {
        if veh.is_some() { (-b.phys.matrix.fwd.x).atan2(b.phys.matrix.fwd.y) } else { l.cur_rot }
    });
    let speed = veh.and_then(|v| sa.world.body(v)).map_or(0.0, |b| b.phys.move_speed.length());
    if let Some(origin) = player_pos {
        let mut load_tile = |i: usize| -> Option<Handle<Image>> {
            tiles
                .entry(i)
                .or_insert_with(|| {
                    let data = world_res.0.file(&format!("radar{i:02}.txd"))?;
                    let t = txd::parse(data).ok()?.into_iter().next()?;
                    let mut img = make_image(convert_texture(t, false)?);
                    // TEXTUREADDRESS clamp (DrawMap's render state).
                    img.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
                        mag_filter: bevy::image::ImageFilterMode::Linear,
                        min_filter: bevy::image::ImageFilterMode::Linear,
                        ..bevy::image::ImageSamplerDescriptor::default()
                    });
                    Some(images.add(img))
                })
                .clone()
        };
        draw_radar(&mut out, &sc, &assets, &sa.world, origin, heading, veh.is_some(), speed, &mut load_tile);
    }

    // Build one mesh per texture; z orders the immediate quads under the buffered text.
    let mut groups: Vec<(Tex, Vec<&Quad>, f32)> = Vec::new();
    for (list, z) in [(&out.quads, 0.0), (&out.text, 10.0)] {
        for q in list.iter() {
            match groups.iter_mut().find(|g| g.0 == q.tex && g.2 == z) {
                Some(g) => g.1.push(q),
                None => groups.push((q.tex, vec![q], z)),
            }
        }
    }
    let (hw, hh) = (sc.w * 0.5, sc.h * 0.5);
    let mut used = Vec::new();
    for (k, (tex, quads, z)) in groups.into_iter().enumerate() {
        let n = quads.len();
        let mut pos = Vec::with_capacity(n * 4);
        let mut uv = Vec::with_capacity(n * 4);
        let mut colv = Vec::with_capacity(n * 4);
        let mut idx = Vec::with_capacity(n * 6);
        for q in quads {
            let b = pos.len() as u32;
            if !q.fan.is_empty() {
                let c = srgb_col(q.col);
                for (p, t) in &q.fan {
                    pos.push([p[0] - hw, hh - p[1], 0.0]);
                    uv.push(*t);
                    colv.push(c);
                }
                for k in 1..q.fan.len().saturating_sub(1) as u32 {
                    idx.extend_from_slice(&[b, b + k, b + k + 1]);
                }
                continue;
            }
            for (px, py) in [(q.l, q.t), (q.r, q.t), (q.l, q.b), (q.r, q.b)] {
                pos.push([px - hw, hh - py, 0.0]);
            }
            uv.extend_from_slice(&q.uv);
            let c = srgb_col(q.col);
            colv.extend_from_slice(&[c, c, c, c]);
            idx.extend_from_slice(&[b, b + 1, b + 3, b, b + 3, b + 2]);
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colv);
        mesh.insert_indices(Indices::U32(idx));
        let image = match tex {
            Tex::White => assets.white.clone(),
            Tex::Font(i) => assets.font_tex[i as usize].clone(),
            Tex::Image(id) => out.images.get(&id).cloned().unwrap_or_else(|| assets.white.clone()),
        };
        let mat = materials.add(ColorMaterial { color: Color::WHITE, texture: Some(image), alpha_mode: bevy::sprite_render::AlphaMode2d::Blend, ..default() });
        let zz = z + k as f32 * 0.01;
        used.push((meshes.add(mesh), mat, zz));
    }
    // The HUD meshes are rebuilt every frame.
    commands.queue(move |world: &mut World| {
        let old: Vec<Entity> = world.query_filtered::<Entity, With<HudMesh>>().iter(world).collect();
        for e in old {
            world.despawn(e);
        }
        for (mesh, mat, z) in used {
            world.spawn((Mesh2d(mesh), MeshMaterial2d(mat), Transform::from_xyz(0.0, 0.0, z), HudMesh));
        }
    });
}

/// `CRadar` state for one draw.
struct Radar {
    origin: Vec2,
    range: f32,
    sin: f32,
    cos: f32,
}

impl Radar {
    /// `TransformRealWorldPointToRadarSpace` (0x583530).
    fn to_radar(&self, p: Vec2) -> Vec2 {
        let d = (p - self.origin) / self.range;
        Vec2::new(self.cos * d.x + self.sin * d.y, self.cos * d.y - self.sin * d.x)
    }

    /// The inverse (world point of a radar point).
    fn to_world(&self, r: Vec2) -> Vec2 {
        let d = Vec2::new(self.cos * r.x - self.sin * r.y, self.sin * r.x + self.cos * r.y);
        d * self.range + self.origin
    }
}

/// `TransformRadarPointToScreenSpace` (0x583480).
fn radar_to_screen(sc: &Scale, r: Vec2) -> [f32; 2] {
    [sc.sx(94.0) * 0.5 * r.x + sc.sx(40.0) + sc.sx(94.0) * 0.5, sc.sy(76.0) * 0.5 + (sc.h - sc.sy(104.0)) - sc.sy(76.0) * 0.5 * r.y]
}

/// Sutherland–Hodgman clip of a polygon against a convex polygon (counter-clockwise).
fn clip_convex(poly: Vec<Vec2>, clip: &[Vec2]) -> Vec<Vec2> {
    let mut out = poly;
    for i in 0..clip.len() {
        let (a, b) = (clip[i], clip[(i + 1) % clip.len()]);
        let inside = |p: Vec2| (b - a).perp_dot(p - a) >= 0.0;
        let input = std::mem::take(&mut out);
        for j in 0..input.len() {
            let (p, q) = (input[j], input[(j + 1) % input.len()]);
            let (ip, iq) = (inside(p), inside(q));
            if ip {
                out.push(p);
            }
            if ip != iq {
                let d = q - p;
                let den = (b - a).perp_dot(d);
                if den.abs() > 1e-12 {
                    let t = (b - a).perp_dot(a - p) / den;
                    out.push(p + d * t);
                }
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out
}

/// The disc mask (DrawRadarMask 0x585700): a 24-gon on the unit circle in radar space.
fn disc_polygon() -> Vec<Vec2> {
    (0..24).map(|i| {
        let a = i as f32 * std::f32::consts::PI / 12.0;
        Vec2::new(a.cos(), a.sin())
    }).collect()
}

#[allow(clippy::too_many_arguments)]
fn draw_radar(
    out: &mut DrawList,
    sc: &Scale,
    assets: &HudAssets,
    world: &sa_physics::world::World,
    origin3: Vec3,
    heading: f32,
    in_vehicle: bool,
    speed: f32,
    load_tile: &mut dyn FnMut(usize) -> Option<Handle<Image>>,
) {
    // DrawMap: the range (on foot 180 m; in a vehicle 180..350 m by speed).
    let range = if !in_vehicle {
        180.0
    } else if speed < 0.3 {
        180.0
    } else if speed < 0.9 {
        (speed - 0.3) * 283.333_34 + 180.0
    } else {
        350.0
    };
    // CalculateCachedSinCos: the camera heading.
    let cam = world.cam_info();
    let angle = (-cam.front.x).atan2(cam.front.y);
    let r = Radar { origin: origin3.truncate(), range, sin: angle.sin(), cos: angle.cos() };
    let disc = disc_polygon();
    let tx = ((r.origin.x + 3000.0) * 0.002).floor() as i32;
    let ty = (11.0 - (r.origin.y + 3000.0) * 0.002).ceil() as i32;
    for dy in -1..=1 {
        for dx in -1..=1 {
            // DrawRadarSection (0x586110).
            let (x, y) = (tx + dx, ty + dy);
            let x0 = (x - 6) as f32 * 500.0;
            let y0 = (5 - y) as f32 * 500.0;
            let corners = [
                Vec2::new(x0, y0),
                Vec2::new(x0 + 500.0, y0),
                Vec2::new(x0 + 500.0, y0 + 500.0),
                Vec2::new(x0, y0 + 500.0),
            ];
            let poly: Vec<Vec2> = corners.iter().map(|&c| r.to_radar(c)).collect();
            // Square clip then the disc (the square clip is implied by the disc).
            let clipped = clip_convex(poly, &disc);
            if clipped.len() < 3 {
                continue;
            }
            let inside = (0..12).contains(&x) && (0..12).contains(&y);
            let tex = if inside { load_tile((x + 12 * y) as usize) } else { None };
            if inside && tex.is_none() {
                continue;
            }
            let pts = clipped
                .iter()
                .map(|&p| {
                    let w = r.to_world(p);
                    let u = (w.x - (x as f32 * 500.0 - 3000.0)) * 0.002;
                    let v = (w.y - ((12 - y) as f32 * 500.0 - 3000.0)) * -0.002;
                    (radar_to_screen(sc, p), [u, v])
                })
                .collect();
            let col = if inside { [255, 255, 255, 255] } else { [111, 137, 170, 255] };
            out.fan(tex.as_ref(), pts, col);
        }
    }
    // The radardisc ring: one quarter texture drawn four times mirrored, in black.
    if let Some(disc_tex) = assets.radar_disc.as_ref() {
        let left = sc.sx(40.0);
        let top = sc.h - sc.sy(104.0);
        let cx = sc.sx(40.0) + sc.sx(47.0);
        let cy = top + sc.sy(38.0);
        let x_l = left - sc.sx(4.0);
        let x_r = left + sc.sx(94.0) + sc.sx(4.0);
        let y_t = top - sc.sy(4.0);
        let y_b = top + sc.sy(76.0) + sc.sy(4.0);
        for (x1, y2) in [(x_l, y_t), (x_r, y_t), (x_l, y_b), (x_r, y_b)] {
            // CSprite2d::Draw: uv (0,0) at (x1, y2), (1,1) at the centre.
            let pts = vec![([x1, y2], [0.0, 0.0]), ([cx, y2], [1.0, 0.0]), ([cx, cy], [1.0, 1.0]), ([x1, cy], [0.0, 1.0])];
            out.fan(Some(disc_tex), pts, [0, 0, 0, 255]);
        }
    }
    // DrawBlips: north on the rim, then the player arrow.
    if let Some(north) = assets.radar_north.as_ref() {
        let mut n = r.to_radar(Vec2::new(r.origin.x, r.origin.y + range * 1.414_213_5));
        if n.length() > 1.0 {
            n /= n.length();
        }
        let p = radar_to_screen(sc, n);
        let (hw, hh) = (sc.sx(8.0) as i32 as f32, sc.sy(8.0) as i32 as f32);
        out.sprite(north, p[0] - hw, p[1] - hh, p[0] + hw, p[1] + hh, [255, 255, 255, 255]);
    }
    if let Some(arrow) = assets.radar_centre.as_ref() {
        let p = radar_to_screen(sc, Vec2::ZERO);
        let a = heading - (angle + std::f32::consts::PI);
        let w = sc.sx(8.0) as i32 as f32;
        // DrawRotatingRadarSprite: v_i = (x + sin(a_i)·w, y + cos(a_i)·h), Draw(v3, v2, v0, v1);
        // CSprite2d::Draw(p1, p2, p3, p4) (0x727590) puts uv (0,0) at p3, (1,0) p4, (1,1) p2, (0,1) p1.
        let v: Vec<[f32; 2]> = (0..4)
            .map(|i| {
                let ai = i as f32 * std::f32::consts::FRAC_PI_2 + a - std::f32::consts::FRAC_PI_4;
                [p[0] + ai.sin() * w, p[1] + ai.cos() * w]
            })
            .collect();
        let pts = vec![(v[0], [0.0, 0.0]), (v[1], [1.0, 0.0]), (v[2], [1.0, 1.0]), (v[3], [0.0, 1.0])];
        out.fan(Some(arrow), pts, [255, 255, 255, 255]);
    }
}
