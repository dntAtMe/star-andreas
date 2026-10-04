//! `CShadows` permanent and static shadows (scorch marks, fire glow): AddPermanentShadow
//! (0x706F60), UpdatePermanentShadows (0x70C950), StoreStaticShadow (0x70BA00), the polygon
//! cast onto building collision (0x70B730 / 0x70A470 / 0x7086B0) and UpdateStaticShadows
//! (0x707F40). Rendering (blend per type, colour via `shadow_colour`) is the app's.
//!
//! Not ported: collision face groups (all triangles are tested), the petrol-trail
//! permanent shadow types, interiors (area codes).

use glam::{Vec2, Vec3};

use crate::world::{EntityId, SECTORS, World, sector_coord};

pub const MAX_PERMANENT: usize = 48;
pub const MAX_STATIC: usize = 48;
/// CPolyBunch pool size.
const MAX_BUNCHES: usize = 360;

/// Shadow textures from models\particle.txd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShadowTex {
    /// gpShadowHeliTex `shad_heli` (the explosion scorch).
    Heli,
    /// gpShadowExplosionTex `shad_exp` (fire glow, lamp pools).
    Exp,
    /// `headlight` / `headlight1` (car light pools, twin / single lamp).
    Headlight,
    Headlight1,
}

impl ShadowTex {
    pub fn name(self) -> &'static str {
        match self {
            Self::Heli => "shad_heli",
            Self::Exp => "shad_exp",
            Self::Headlight => "headlight",
            Self::Headlight1 => "headlight1",
        }
    }

    /// A 2dfx shadow texture name (particle.txd); unknown names use shad_exp.
    pub fn from_name(n: &str) -> Self {
        match n.to_ascii_lowercase().as_str() {
            "shad_heli" => Self::Heli,
            "headlight" => Self::Headlight,
            "headlight1" => Self::Headlight1,
            _ => Self::Exp,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Permanent {
    ty: u8,
    tex: ShadowTex,
    pos: Vec3,
    front: Vec2,
    side: Vec2,
    z_dist: f32,
    scale: f32,
    created: u32,
    lifetime: u32,
    intensity: i16,
    rgb: [u8; 3],
}

/// One clipped polygon (<= 7 vertices) with uv quantised to 1/200.
#[derive(Debug, Clone)]
pub struct ShadowPoly {
    pub verts: Vec<(Vec3, Vec2)>,
}

#[derive(Debug, Clone)]
pub struct StaticShadow {
    pub id: u64,
    pub polys: Vec<ShadowPoly>,
    created: u32,
    pos: Vec3,
    front: Vec2,
    side: Vec2,
    z_dist: f32,
    pub tex: ShadowTex,
    pub intensity: i16,
    pub ty: u8,
    pub rgb: [u8; 3],
    just_created: bool,
    temporary: bool,
    /// Lighting byte of the last triangle that produced a polygon.
    pub light: u8,
}

#[derive(Debug, Clone, Default)]
pub struct Shadows {
    permanent: Vec<Option<Permanent>>,
    pub statics: Vec<Option<StaticShadow>>,
    bunches_used: usize,
}

impl Shadows {
    pub fn new() -> Self {
        Self { permanent: vec![None; MAX_PERMANENT], statics: vec![None; MAX_STATIC], bunches_used: 0 }
    }
}

/// `CalcShadowColour` (0x707850).
pub fn shadow_colour(ty: u8, light: u8, rgb: [u8; 3], dn_balance: f32) -> [u8; 3] {
    let k = if ty == 2 {
        1.0
    } else {
        let k1 = (1.0 - dn_balance) * 0.6 + 0.4;
        let k2 = ((light & 15) as f32 / 30.0 * (1.0 - dn_balance) + (light >> 4) as f32 / 30.0 * dn_balance) * 0.7 + 0.3;
        k1.min(k2)
    };
    rgb.map(|c| (c as f32 * k) as i32 as u8)
}

impl World {
    /// `CShadows::AddPermanentShadow` (0x706F60).
    #[allow(clippy::too_many_arguments)]
    pub fn add_permanent_shadow(
        &mut self,
        ty: u8,
        tex: ShadowTex,
        pos: Vec3,
        front: Vec2,
        side: Vec2,
        intensity: i16,
        rgb: [u8; 3],
        z_dist: f32,
        lifetime_ms: u32,
        scale: f32,
    ) {
        let s = &mut self.shadows.permanent;
        let slot = s.iter().position(Option::is_none).or_else(|| {
            // Replace the oldest small shadow (< 0.5 units on both axes).
            s.iter()
                .enumerate()
                .filter_map(|(i, p)| p.map(|p| (i, p)))
                .filter(|(_, p)| p.front.length_squared() < 0.25 && p.side.length_squared() < 0.25)
                .min_by_key(|(_, p)| p.created)
                .map(|(i, _)| i)
        });
        if let Some(i) = slot {
            s[i] = Some(Permanent {
                ty,
                tex,
                pos,
                front,
                side,
                z_dist,
                scale,
                created: self.now_ms,
                lifetime: lifetime_ms,
                intensity,
                rgb,
            });
        }
    }

    /// `UpdatePermanentShadows` (0x70C950), every game frame.
    pub(crate) fn update_permanent_shadows(&mut self) {
        let now = self.now_ms;
        for i in 0..MAX_PERMANENT {
            let Some(s) = self.shadows.permanent[i] else { continue };
            let age = now.wrapping_sub(s.created);
            if age >= s.lifetime {
                self.shadows.permanent[i] = None;
                continue;
            }
            let q = (s.lifetime * 3) >> 2;
            let (intensity, rgb) = if age < q {
                (s.intensity, s.rgb)
            } else {
                let t = 1.0 - (age - q) as f32 / (s.lifetime >> 2) as f32;
                (((s.intensity as f32) * t) as i16, s.rgb.map(|c| (c as f32 * t) as i32 as u8))
            };
            let id = 0x1_0000_0000 | i as u64;
            let ok = self.store_static_shadow(id, s.ty, s.tex, s.pos, s.front, s.side, intensity, rgb, s.z_dist, s.scale, 40.0, false, 0.0);
            if !ok && s.ty != 8 {
                self.shadows.permanent[i] = None;
            }
        }
    }

    /// `CShadows::StoreStaticShadow` (0x70BA00). Returns whether the shadow has polygons.
    #[allow(clippy::too_many_arguments)]
    pub fn store_static_shadow(
        &mut self,
        id: u64,
        ty: u8,
        tex: ShadowTex,
        pos: Vec3,
        front: Vec2,
        side: Vec2,
        mut intensity: i16,
        mut rgb: [u8; 3],
        z_dist: f32,
        _scale: f32,
        draw_dist: f32,
        temporary: bool,
        up_distance: f32,
    ) -> bool {
        let cam = self.camera_pos;
        let d2 = (pos.x - cam.x).powi(2) + (pos.y - cam.y).powi(2);
        if d2 >= draw_dist * draw_dist {
            if draw_dist != 0.0 {
                return true;
            }
        } else if draw_dist != 0.0 {
            let d = d2.sqrt();
            if d >= 0.75 * draw_dist {
                let k = 1.0 - (d - 0.75 * draw_dist) * (4.0 / draw_dist);
                intensity = (intensity as f32 * k) as i16;
                rgb = rgb.map(|c| (c as f32 * k) as i32 as u8);
            }
        }
        let now = self.now_ms;
        let mut fill = None;
        for i in 0..MAX_STATIC {
            let Some(s) = self.shadows.statics[i].as_mut() else { continue };
            if s.id != id || s.polys.is_empty() {
                continue;
            }
            let d = pos - s.pos;
            let same = (d.x.abs() < up_distance && d.y.abs() < up_distance)
                || (d.x.abs() < 0.05 && d.y.abs() < 0.05 && d.z.abs() < 2.0 && front == s.front && side == s.side);
            if same {
                s.tex = tex;
                s.ty = ty;
                s.intensity = intensity;
                s.rgb = rgb;
                s.z_dist = z_dist;
                s.created = now;
                s.just_created = true;
                s.temporary = temporary;
                return true;
            }
            self.shadows.bunches_used -= s.polys.len();
            self.shadows.statics[i] = None;
            fill = Some(i);
            break;
        }
        let Some(i) = fill.or_else(|| self.shadows.statics.iter().position(|s| s.as_ref().is_none_or(|s| s.polys.is_empty())))
        else {
            return true;
        };
        if let Some(old) = self.shadows.statics[i].take() {
            self.shadows.bunches_used -= old.polys.len();
        }
        let mut s = StaticShadow {
            id,
            polys: Vec::new(),
            created: now,
            pos,
            front,
            side,
            z_dist,
            tex,
            intensity,
            ty,
            rgb,
            just_created: true,
            temporary,
            light: 0,
        };
        self.generate_shadow_polys(&mut s);
        let ok = !s.polys.is_empty();
        self.shadows.statics[i] = Some(s);
        ok
    }

    /// `GeneratePolysForStaticShadow` / `CastShadowSectorList`: building collision only.
    fn generate_shadow_polys(&mut self, s: &mut StaticShadow) {
        let ext = Vec2::new(s.front.x.abs() + s.side.x.abs(), s.front.y.abs() + s.side.y.abs());
        let (lo, hi) = (s.pos.truncate() - ext, s.pos.truncate() + ext);
        let scan = self.next_scan_code();
        for y in sector_coord(lo.y)..=sector_coord(hi.y) {
            for x in sector_coord(lo.x)..=sector_coord(hi.x) {
                let list = self.sectors_ref()[(y * SECTORS + x) as usize].clone();
                for bi in list {
                    let Some(b) = self.building_mut(bi) else { continue };
                    if b.scan == scan {
                        continue;
                    }
                    b.scan = scan;
                    let (m, col) = (b.matrix, b.col.clone());
                    if m.up.z <= 0.97 {
                        continue;
                    }
                    // GetBoundRect: world xy rect of the bbox corners.
                    let (mut bmin, mut bmax) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
                    for c in [
                        Vec3::new(col.bbox_min.x, col.bbox_min.y, 0.0),
                        Vec3::new(col.bbox_max.x, col.bbox_min.y, 0.0),
                        Vec3::new(col.bbox_min.x, col.bbox_max.y, 0.0),
                        Vec3::new(col.bbox_max.x, col.bbox_max.y, 0.0),
                    ] {
                        let w = m.transform(c).truncate();
                        bmin = bmin.min(w);
                        bmax = bmax.max(w);
                    }
                    if !(bmin.x < hi.x && bmax.x > lo.x && bmin.y < hi.y && bmax.y > lo.y) {
                        continue;
                    }
                    if !(s.pos.z - s.z_dist < col.bbox_max.z + m.pos.z && col.bbox_min.z + m.pos.z < s.pos.z) {
                        continue;
                    }
                    cast_shadow_entity_xy(&m, &col, s, &mut self.shadows.bunches_used);
                }
            }
        }
    }

    /// `CShadows::StoreCarLightShadow` (0x70C500): a car's headlight pool. Stored as a
    /// static shadow while the car is slow and not the player's; the real-time path for
    /// moving cars is approximated by a temporary static shadow re-cast every frame.
    #[allow(clippy::too_many_arguments)]
    pub fn store_car_light_shadow(
        &mut self,
        car: EntityId,
        id: u64,
        tex: ShadowTex,
        c: Vec3,
        front: Vec2,
        side: Vec2,
        mut rgb: [u8; 3],
        max_view_angle: f32,
    ) {
        let cam = self.camera_pos;
        let d2 = (c.x - cam.x).powi(2) + (c.y - cam.y).powi(2);
        if d2 >= 729.0 {
            return;
        }
        let f = self.camera_fwd;
        if (c.x - cam.x) * f.x + (c.y - cam.y) * f.y <= -max_view_angle {
            return;
        }
        let dist = d2.sqrt();
        if dist >= 20.25 {
            let k = 1.0 - (dist - 18.0) * 0.111_111_11;
            rgb = rgb.map(|v| (v as f32 * k) as i32 as u8);
        }
        let _ = car;
        self.store_static_shadow(id, 2, tex, c, front, side, 128, rgb, 6.0, 1.0, 0.0, false, 0.4);
    }

    /// `UpdateStaticShadows` (0x707F40): drop shadows not re-stored this frame.
    pub(crate) fn update_static_shadows(&mut self) {
        let now = self.now_ms;
        for slot in &mut self.shadows.statics {
            let Some(s) = slot.as_mut() else { continue };
            if !s.polys.is_empty() && !s.just_created && (!s.temporary || now > s.created + 5000) {
                self.shadows.bunches_used -= s.polys.len();
                *slot = None;
                continue;
            }
            s.just_created = false;
        }
    }
}

/// `CastShadowEntityXY` (0x7086B0): clip the shadow quad against each collision triangle
/// in the entity's local XY and project it vertically onto the triangle's plane.
fn cast_shadow_entity_xy(m: &crate::physical::Matrix, col: &crate::collision::ColModel, s: &mut StaticShadow, used: &mut usize) {
    let (r, u, e) = (m.right, m.fwd, m.pos);
    let f = Vec2::new(s.front.x * r.x + s.front.y * r.y, s.front.x * u.x + s.front.y * u.y);
    let sd = Vec2::new(s.side.x * r.x + s.side.y * r.y, s.side.x * u.x + s.side.y * u.y);
    let rel = s.pos - e;
    let c = Vec2::new(rel.x * r.x + rel.y * r.y, rel.x * u.x + rel.y * u.y);
    let zl = s.pos.z - e.z;
    let quad = [
        (c + f - sd, Vec2::new(0.0, 0.0)),
        (c + f + sd, Vec2::new(1.0, 0.0)),
        (c - f + sd, Vec2::new(1.0, 1.0)),
        (c - f - sd, Vec2::new(0.0, 1.0)),
    ];
    let (mut qmin, mut qmax) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for (p, _) in &quad {
        qmin = qmin.min(*p);
        qmax = qmax.max(*p);
    }
    for (ti, t) in col.tris.iter().enumerate() {
        let plane = &col.planes[ti];
        let nz = plane.normal.z;
        if nz.abs() <= 0.1 {
            continue;
        }
        let v = t.v.map(|k| col.verts[k as usize]);
        if !(v.iter().any(|p| p.x > qmin.x)
            && v.iter().any(|p| p.x < qmax.x)
            && v.iter().any(|p| p.y > qmin.y)
            && v.iter().any(|p| p.y < qmax.y))
        {
            continue;
        }
        if !(v.iter().any(|p| p.z < zl) && v.iter().any(|p| p.z > zl - s.z_dist)) {
            continue;
        }
        // Sutherland-Hodgman against the three edges (inside = strictly right of the edge).
        let mut poly: Vec<(Vec2, Vec2)> = quad.to_vec();
        for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
            let side = |p: Vec2| (p.x - a.x) * (b.y - a.y) - (p.y - a.y) * (b.x - a.x);
            let mut out = Vec::with_capacity(poly.len() + 1);
            for k in 0..poly.len() {
                let prev = poly[(k + poly.len() - 1) % poly.len()];
                let cur = poly[k];
                let (dp, dc) = (side(prev.0), side(cur.0));
                if (dp > 0.0) != (dc > 0.0) {
                    let t = dp.abs() / (dc.abs() + dp.abs());
                    out.push((prev.0 * (1.0 - t) + cur.0 * t, prev.1 * (1.0 - t) + cur.1 * t));
                }
                if dc > 0.0 {
                    out.push(cur);
                }
            }
            poly = out;
            if poly.is_empty() {
                break;
            }
        }
        if poly.len() <= 2 {
            continue;
        }
        if *used >= MAX_BUNCHES {
            continue; // pool exhausted: silently dropped
        }
        let n = plane.normal;
        let d = n.dot(v[0]);
        let verts = poly
            .iter()
            .take(7)
            .map(|&(p, uv)| {
                let z = (d - n.x * p.x - n.y * p.y) / n.z;
                let w = Vec3::new(r.x * p.x + u.x * p.y + e.x, r.y * p.x + u.y * p.y + e.y, z + e.z);
                let q = |x: f32| ((x * 200.0) as i32 as u8) as f32 * 0.005;
                (w, Vec2::new(q(uv.x), q(uv.y)))
            })
            .collect();
        s.light = t.light;
        s.polys.insert(0, ShadowPoly { verts });
        *used += 1;
    }
}
