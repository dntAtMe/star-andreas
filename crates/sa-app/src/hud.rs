//! The in-game HUD (hud.md) drawn with a port of CFont (font.md): `CHud::DrawPlayerInfo`
//! (clock, money, weapon icon, ammo, health / armour / breath bars) and `CHud::DrawWanted`.
//! Everything is laid out in the 640×448 virtual screen scaled per axis, and drawn as 2D
//! quads (glyphs from fonts.txd with the exact UV insets, bars as untextured rects).
//!
//! Not ported: the radar, zone / vehicle name popups (need GXT), the help box, messages,
//! the vital-stats panel, the 2-player layout, button icons.

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
        self.quads.push(Quad { tex: Tex::White, l, t, r, b, uv: [[0.0; 2]; 4], col });
    }

    fn sprite(&mut self, img: &Handle<Image>, l: f32, t: f32, r: f32, b: f32, col: [u8; 4]) {
        self.images.insert(img.id(), img.clone());
        self.quads.push(Quad { tex: Tex::Image(img.id()), l, t, r, b, uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]], col });
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
    commands.insert_resource(HudAssets { font_tex: [f2, f1], white, fist: hud.get("fist").cloned(), data: FontData { vals } });
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
