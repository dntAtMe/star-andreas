//! `CCollision`: collision models and contact generation.
//!
//! Normals of model-vs-model contacts point from model B towards model A.
//! Strictness of comparisons follows the original exactly.

use glam::Vec3;

use crate::{
    colpoint::ColPoint,
    physical::{Matrix, normalise},
};

/// Surface bytes carried by every primitive (material = surface type).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Surf {
    pub material: u8,
    pub piece: u8,
    pub lighting: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ColSphere {
    pub center: Vec3,
    pub radius: f32,
    pub surf: Surf,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ColBox {
    pub min: Vec3,
    pub max: Vec3,
    pub surf: Surf,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ColLine {
    pub start: Vec3,
    pub end: Vec3,
}

#[derive(Debug, Clone, Copy)]
pub struct ColTriangle {
    pub v: [u16; 3],
    pub material: u8,
    pub light: u8,
}

/// Quantised plane (`CColTrianglePlane`), stored dequantised.
#[derive(Debug, Clone, Copy)]
pub struct TrianglePlane {
    pub normal: Vec3,
    pub dist: f32,
    /// Dominant axis: 0 +X, 1 -X, 2 +Y, 3 -Y, 4 +Z, 5 -Z.
    pub orient: u8,
}

impl TrianglePlane {
    /// `CColTrianglePlane::Set` (0x411660).
    pub fn new(a: Vec3, b: Vec3, c: Vec3) -> Self {
        let n = normalise((c - a).cross(b - a));
        // ftol truncates toward zero.
        let q = |x: f32| (x * 4096.0).trunc();
        let nq = Vec3::new(q(n.x), q(n.y), q(n.z)) * (1.0 / 4096.0);
        let dist = (a.dot(nq) * 128.0).trunc() * (1.0 / 128.0);
        let (ax, ay, az) = (nq.x.abs(), nq.y.abs(), nq.z.abs());
        let orient = if ax > ay && ax > az {
            if nq.x > 0.0 { 0 } else { 1 }
        } else if az < ay {
            if nq.y > 0.0 { 2 } else { 3 }
        } else if nq.z > 0.0 {
            4
        } else {
            5
        };
        Self { normal: nq, dist, orient }
    }
}

/// `CColModel` + `CCollisionData`, with triangle planes precomputed.
#[derive(Debug, Clone, Default)]
pub struct ColModel {
    pub bbox_min: Vec3,
    pub bbox_max: Vec3,
    pub bound_center: Vec3,
    pub bound_radius: f32,
    pub spheres: Vec<ColSphere>,
    pub boxes: Vec<ColBox>,
    /// Suspension / wheel probe lines (vehicles; built at runtime, not in COL files).
    pub lines: Vec<ColLine>,
    pub verts: Vec<Vec3>,
    pub tris: Vec<ColTriangle>,
    pub planes: Vec<TrianglePlane>,
}

impl ColModel {
    /// Build from a parsed COL model (GTA space). Vertices are already at the
    /// game's 1/128 quantisation.
    pub fn from_col(m: &sa_formats::col::ColModel) -> Self {
        let q = |v: [f32; 3]| Vec3::new(v[0], v[1], v[2]);
        let surf = |s: &sa_formats::col::Surface| Surf { material: s.material, piece: s.flags, lighting: s.light };
        let verts: Vec<Vec3> = m.vertices.iter().map(|&v| q(v)).collect();
        let tris: Vec<ColTriangle> = m
            .faces
            .iter()
            .map(|f| ColTriangle { v: [f.v[0] as u16, f.v[1] as u16, f.v[2] as u16], material: f.material, light: f.light })
            .collect();
        let planes = tris
            .iter()
            .map(|t| TrianglePlane::new(verts[t.v[0] as usize], verts[t.v[1] as usize], verts[t.v[2] as usize]))
            .collect();
        Self {
            bbox_min: q(m.min),
            bbox_max: q(m.max),
            bound_center: q(m.center),
            bound_radius: m.radius,
            spheres: m
                .spheres
                .iter()
                .map(|s| ColSphere { center: q(s.center), radius: s.radius, surf: surf(&s.surface) })
                .collect(),
            boxes: m.boxes.iter().map(|b| ColBox { min: q(b.min), max: q(b.max), surf: surf(&b.surface) }).collect(),
            lines: Vec::new(),
            verts,
            tris,
            planes,
        }
    }
}

// ---------------------------------------------------------------- matrices

impl Matrix {
    /// Orthonormal inverse (`0x59B920`).
    pub fn inverse(&self) -> Matrix {
        let (r, f, u) = (self.right, self.fwd, self.up);
        let right = Vec3::new(r.x, f.x, u.x);
        let fwd = Vec3::new(r.y, f.y, u.y);
        let up = Vec3::new(r.z, f.z, u.z);
        let m = Matrix { right, fwd, up, pos: Vec3::ZERO };
        let pos = -m.rotate(self.pos);
        Matrix { right, fwd, up, pos }
    }

    /// `self * rhs`: apply `rhs` first, then `self`.
    pub fn mul(&self, rhs: &Matrix) -> Matrix {
        Matrix {
            right: self.rotate(rhs.right),
            fwd: self.rotate(rhs.fwd),
            up: self.rotate(rhs.up),
            pos: self.transform(rhs.pos),
        }
    }
}

// ---------------------------------------------------------------- primitives

fn sphere_overlaps_box(c: Vec3, r: f32, min: Vec3, max: Vec3) -> bool {
    // TestSphereBox (0x4120C0): inclusive, x then y then z.
    c.x + r >= min.x && c.x - r <= max.x && c.y + r >= min.y && c.y - r <= max.y && c.z + r >= min.z && c.z - r <= max.z
}

fn set_a(cp: &mut ColPoint, s: Surf) {
    cp.surface_a = s.material;
    cp.piece_a = s.piece;
    cp.lighting_a = s.lighting;
}

fn set_b(cp: &mut ColPoint, s: Surf) {
    cp.surface_b = s.material;
    cp.piece_b = s.piece;
    cp.lighting_b = s.lighting;
}

/// 0x416450
pub fn process_sphere_sphere(a: &ColSphere, b: &ColSphere, cp: &mut ColPoint, min_dist_sq: &mut f32) -> bool {
    let d = a.center - b.center;
    let dist = d.dot(d).sqrt() - b.radius;
    let depth = a.radius - dist;
    let dc = if dist < 0.0 { 0.0 } else { dist };
    if !(dc * dc < *min_dist_sq) || !(dc < a.radius) {
        return false;
    }
    let n = normalise(d);
    cp.point = a.center - n * dc;
    cp.normal = n;
    set_a(cp, a.surf);
    set_b(cp, b.surf);
    cp.depth = depth;
    *min_dist_sq = dc * dc;
    true
}

/// 0x411EC0: nearest-face push-out for a point inside a box.
fn process_point_in_box(min: Vec3, max: Vec3, p: Vec3, cp: &mut ColPoint) {
    let axis = |pc: f32, mn: f32, mx: f32| {
        let off = pc - (mx + mn) * 0.5;
        if off <= 0.0 { (pc - mn, -1.0) } else { (mx - pc, 1.0) }
    };
    let (dx, sx) = axis(p.x, min.x, max.x);
    let (dy, sy) = axis(p.y, min.y, max.y);
    let (dz, sz) = axis(p.z, min.z, max.z);
    cp.point = p;
    if dx < dy && dx < dz {
        cp.normal = Vec3::new(sx, 0.0, 0.0);
        cp.depth = dx;
    } else if dy < dx && dy < dz {
        cp.normal = Vec3::new(0.0, sy, 0.0);
        cp.depth = dy;
    } else {
        cp.normal = Vec3::new(0.0, 0.0, sz);
        cp.depth = dz;
    }
}

/// 0x412130
pub fn process_sphere_box(s: &ColSphere, b: &ColBox, cp: &mut ColPoint, min_dist_sq: &mut f32) -> bool {
    let (c, r) = (s.center, s.radius);
    if c.x + r < b.min.x || c.x - r > b.max.x || c.y + r < b.min.y || c.y - r > b.max.y || c.z + r < b.min.z || c.z - r > b.max.z {
        return false;
    }
    let clamp = |v: f32, mn: f32, mx: f32| if v < mn { mn } else if v > mx { mx } else { v };
    let q = Vec3::new(clamp(c.x, b.min.x, b.max.x), clamp(c.y, b.min.y, b.max.y), clamp(c.z, b.min.z, b.max.z));
    let inside = b.min.x <= c.x && c.x <= b.max.x && b.min.y <= c.y && c.y <= b.max.y && b.min.z <= c.z && c.z <= b.max.z;
    if inside {
        // No comparison against min_dist_sq on this path (original behaviour).
        process_point_in_box(b.min, b.max, c, cp);
        cp.depth += r;
        cp.point -= cp.normal * r;
        set_a(cp, s.surf);
        set_b(cp, b.surf);
        *min_dist_sq = 0.0;
        return true;
    }
    let diff = c - q;
    let dist_sq = diff.dot(diff);
    if !(dist_sq < *min_dist_sq) {
        return false;
    }
    let dist = dist_sq.sqrt();
    if !(dist <= r) {
        return false;
    }
    cp.point = q;
    cp.normal = diff * (1.0 / dist);
    set_a(cp, s.surf);
    set_b(cp, b.surf);
    cp.depth = r - dist;
    *min_dist_sq = dist_sq;
    true
}

/// Shared geometry of TestSphereTriangle (0x4165B0) / ProcessSphereTriangle (0x416BA0).
/// Returns (distance from the centre to the triangle, closest point), or None (k == 0 or plane miss).
fn sphere_triangle_geometry(c: Vec3, r: f32, a: Vec3, b: Vec3, cc: Vec3, pl: &TrianglePlane) -> Option<(f32, f32, Vec3)> {
    let n = pl.normal;
    let s0 = n.dot(c) - pl.dist;
    if s0.abs() > r {
        return None;
    }
    let mut e = b - a;
    let len = e.length();
    e *= 1.0 / len;
    let perp = e.cross(n);
    let cx = (cc - a).dot(e);
    let cy = (cc - a).dot(perp);
    let px = (c - a).dot(e);
    let py = (c - a).dot(perp);
    let in_ab = (py * len - px * 0.0) >= 0.0;
    let in_ca = (px * cy - py * cx) >= 0.0;
    let in_bc = ((cx - len) * py - (px - len) * cy) >= 0.0;
    let k = in_ab as u8 + in_ca as u8 + in_bc as u8;
    let edge = |start: Vec3, end: Vec3, t: f32, h: f32, d_lo: Vec3, d_hi: Vec3| -> (f32, Vec3) {
        if t <= 0.0 {
            ((c - d_lo).length(), d_lo)
        } else if t < 1.0 {
            ((s0 * s0 + h * h).sqrt(), start + (end - start) * t)
        } else {
            ((c - d_hi).length(), d_hi)
        }
    };
    let (dist, p) = match k {
        3 => (s0.abs(), c - n * s0),
        1 => {
            let v = if in_ab { cc } else if in_ca { b } else { a };
            ((c - v).length(), v)
        }
        2 => {
            if in_ab && in_ca {
                let (dx, dy) = (cx - len, cy);
                let l2 = dx * dx + dy * dy;
                let t = (py * cy + (px - len) * (cx - len)) / l2;
                let h = ((cx - len) * py - (px - len) * cy) / l2.sqrt();
                edge(b, cc, t, h, b, cc)
            } else if in_ab {
                let l2 = cx * cx + cy * cy;
                let t = (px * cx + py * cy) / l2;
                let h = (py * cx - px * cy) / l2.sqrt();
                edge(a, cc, t, h, a, cc)
            } else {
                let t = (py * 0.0 + px * len) / (len * len);
                let h = (py * len - px * 0.0) / (len * len).sqrt();
                edge(a, b, t, h, a, b)
            }
        }
        _ => return None,
    };
    Some((s0, dist, p))
}

pub fn test_sphere_triangle(s: &ColSphere, m: &ColModel, ti: usize) -> bool {
    let t = &m.tris[ti];
    let (a, b, c) = (m.verts[t.v[0] as usize], m.verts[t.v[1] as usize], m.verts[t.v[2] as usize]);
    matches!(sphere_triangle_geometry(s.center, s.radius, a, b, c, &m.planes[ti]), Some((_, d, _)) if d < s.radius)
}

/// 0x416BA0
pub fn process_sphere_triangle(s: &ColSphere, m: &ColModel, ti: usize, cp: &mut ColPoint, min_dist_sq: &mut f32) -> bool {
    let t = &m.tris[ti];
    let pl = &m.planes[ti];
    let s0 = pl.normal.dot(s.center) - pl.dist;
    if s0.abs() <= s.radius && s0 * s0 > *min_dist_sq {
        return false;
    }
    let (a, b, c) = (m.verts[t.v[0] as usize], m.verts[t.v[1] as usize], m.verts[t.v[2] as usize]);
    let Some((_, dist, p)) = sphere_triangle_geometry(s.center, s.radius, a, b, c, pl) else { return false };
    if !(dist < s.radius) || !(dist * dist < *min_dist_sq) {
        return false;
    }
    cp.point = p;
    cp.normal = normalise(s.center - p);
    set_a(cp, s.surf);
    cp.surface_b = t.material;
    cp.piece_b = 0;
    // lighting_b is not written by the original.
    cp.depth = s.radius - dist;
    *min_dist_sq = dist * dist;
    true
}

/// 0x412AA0. Writes only B surface and zeroes surface/piece A.
pub fn process_line_sphere(l: &ColLine, s: &ColSphere, cp: &mut ColPoint, min_t: &mut f32) -> bool {
    let d = l.end - l.start;
    let a = d.dot(d);
    let m = s.center - l.start;
    let bneg = -m.dot(d);
    let disc = bneg * bneg - (m.dot(m) - s.radius * s.radius) * a;
    if disc < 0.0 {
        return false;
    }
    let t = (-bneg - disc.sqrt()) / a;
    if !(t >= 0.0 && t <= 1.0 && t < *min_t) {
        return false;
    }
    let p = l.start + d * t;
    cp.point = p;
    cp.normal = normalise(p - s.center);
    set_b(cp, s.surf);
    cp.surface_a = 0;
    cp.piece_a = 0;
    *min_t = t;
    true
}

/// 0x413100
pub fn process_line_box(l: &ColLine, b: &ColBox, cp: &mut ColPoint, min_t: &mut f32) -> bool {
    let s = l.start;
    let strictly_inside = b.min.x < s.x && s.x < b.max.x && b.min.y < s.y && s.y < b.max.y && b.min.z < s.z && s.z < b.max.z;
    if strictly_inside {
        process_point_in_box(b.min, b.max, s, cp);
        cp.surface_a = 0;
        cp.piece_a = 0;
        set_b(cp, b.surf);
        *min_t = 0.0;
        return true;
    }
    let d = l.end - l.start;
    let mut best = 1.0f32;
    let mut hit: Option<(Vec3, Vec3)> = None;
    // Faces in order -X, +X, -Y, +Y, -Z, +Z.
    for axis in 0..3 {
        for side in 0..2 {
            let (f0, f1, plane, sign) = if side == 0 {
                (b.min[axis] - s[axis], b.min[axis] - l.end[axis], b.min[axis], -1.0)
            } else {
                (s[axis] - b.max[axis], l.end[axis] - b.max[axis], b.max[axis], 1.0)
            };
            if f1 * f0 < 0.0 {
                let t = f0 / (f0 - f1);
                let mut p = s + d * t;
                p[axis] = plane;
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                if b.min[u] < p[u] && p[u] < b.max[u] && b.min[v] < p[v] && p[v] < b.max[v] && t < best {
                    best = t;
                    let mut n = Vec3::ZERO;
                    n[axis] = sign;
                    hit = Some((p, n));
                }
            }
        }
    }
    if *min_t <= best {
        return false;
    }
    let Some((p, n)) = hit else { return false };
    cp.point = p;
    cp.normal = n;
    set_b(cp, b.surf);
    cp.surface_a = 0;
    cp.piece_a = 0;
    *min_t = best;
    true
}

/// 0x4140F0
pub fn process_line_triangle(l: &ColLine, m: &ColModel, ti: usize, cp: &mut ColPoint, min_t: &mut f32) -> bool {
    let tri = &m.tris[ti];
    let pl = &m.planes[ti];
    let n = pl.normal;
    let s0 = n.dot(l.start) - pl.dist;
    let s1 = n.dot(l.end) - pl.dist;
    if s0 * s1 > 0.0 {
        return false;
    }
    let dl = l.end - l.start;
    let t = (pl.dist - n.dot(l.start)) / dl.dot(n);
    let p = l.start + dl * t;
    let (a, b, c) = (m.verts[tri.v[0] as usize], m.verts[tri.v[1] as usize], m.verts[tri.v[2] as usize]);
    let (uv, verts): (fn(Vec3) -> (f32, f32), [Vec3; 3]) = match pl.orient {
        0 => (|v| (v.y, v.z), [a, c, b]),
        1 => (|v| (v.y, v.z), [a, b, c]),
        2 => (|v| (v.z, v.x), [a, c, b]),
        3 => (|v| (v.z, v.x), [a, b, c]),
        4 => (|v| (v.x, v.y), [a, c, b]),
        _ => (|v| (v.x, v.y), [a, b, c]),
    };
    let (pu, pv) = uv(p);
    let (v0, v1, v2) = (uv(verts[0]), uv(verts[1]), uv(verts[2]));
    let e1 = (v1.0 - v0.0) * (pv - v0.1) - (v1.1 - v0.1) * (pu - v0.0);
    let e2 = (v2.0 - v0.0) * (pv - v0.1) - (v2.1 - v0.1) * (pu - v0.0);
    let e3 = (v2.0 - v1.0) * (pv - v1.1) - (v2.1 - v1.1) * (pu - v1.0);
    if !(e1 >= 0.0 && e2 <= 0.0 && e3 >= 0.0 && t < *min_t) {
        return false;
    }
    cp.point = l.start + dl * t;
    cp.normal = n;
    cp.surface_b = tri.material;
    cp.piece_b = 0;
    cp.lighting_b = tri.light;
    cp.surface_a = 0;
    cp.piece_a = 0;
    *min_t = t;
    true
}

// ---------------------------------------------------------------- ProcessColModels

pub const MAX_COLPOINTS: usize = 32;

fn sphere_xf(m: &Matrix, s: &ColSphere) -> ColSphere {
    ColSphere { center: m.transform(s.center), ..*s }
}

/// 0x4185C0. `line_points` / `line_values` must have one entry per line of A
/// (values are the in/out best fraction along each line, normally 1.0).
/// Returns the number of contacts written to `points` (world space, normal B -> A).
#[allow(clippy::too_many_arguments)]
pub fn process_col_models(
    mat_a: &Matrix,
    a: &ColModel,
    mat_b: &Matrix,
    b: &ColModel,
    points: &mut [ColPoint; MAX_COLPOINTS],
    line_points: &mut [ColPoint],
    line_values: &mut [f32],
    multi_points_per_sphere: bool,
) -> usize {
    let m_ab = mat_b.inverse().mul(mat_a);
    let m_ba = mat_a.inverse().mul(mat_b);

    // Step 0
    let bs_a = m_ab.transform(a.bound_center);
    if !sphere_overlaps_box(bs_a, a.bound_radius, b.bbox_min, b.bbox_max) {
        return 0;
    }

    // Step 1: candidates.
    let sa: Vec<ColSphere> = a.spheres.iter().map(|s| sphere_xf(&m_ab, s)).collect();
    let list_a: Vec<usize> =
        (0..sa.len()).filter(|&i| sphere_overlaps_box(sa[i].center, sa[i].radius, b.bbox_min, b.bbox_max)).collect();
    let sb: Vec<ColSphere> = b.spheres.iter().map(|s| sphere_xf(&m_ba, s)).collect();
    let list_b: Vec<usize> =
        (0..sb.len()).filter(|&j| sphere_overlaps_box(sb[j].center, sb[j].radius, a.bbox_min, a.bbox_max)).collect();
    if list_a.is_empty() && a.lines.is_empty() && list_b.is_empty() {
        return 0;
    }
    let ra = a.bound_radius;
    let box_b: Vec<usize> = (0..b.boxes.len())
        .filter(|&k| {
            let bx = &b.boxes[k];
            bx.min.x <= bs_a.x + ra
                && bx.min.y <= bs_a.y + ra
                && bx.min.z <= bs_a.z + ra
                && bx.max.x >= bs_a.x - ra
                && bx.max.y >= bs_a.y - ra
                && bx.max.z >= bs_a.z - ra
        })
        .take(64)
        .collect();
    let bound_sphere_a = ColSphere { center: bs_a, radius: ra, surf: Surf::default() };
    let tri_b: Vec<usize> = (0..b.tris.len()).filter(|&t| test_sphere_triangle(&bound_sphere_a, b, t)).take(599).collect();
    if list_b.is_empty() && box_b.is_empty() && tri_b.is_empty() {
        return 0;
    }

    // Step 2: A spheres vs B (in B space).
    let mut n = 0usize;
    points[0].depth = -1.0;
    'spheres: for &i in &list_a {
        let mut hit = false;
        let mut min_d = 1e24f32;
        let orig = a.spheres[i];
        for &j in &list_b {
            if process_sphere_sphere(&sa[i], &b.spheres[j], &mut points[n], &mut min_d) {
                hit = true;
            }
        }
        for &k in &box_b {
            if process_sphere_box(&sa[i], &b.boxes[k], &mut points[n], &mut min_d) {
                set_a(&mut points[n], orig.surf);
                if multi_points_per_sphere && orig.surf.piece <= 2 && n < 31 {
                    n += 1;
                    hit = false;
                    min_d = 1e24;
                    points[n].depth = -1.0;
                } else {
                    hit = true;
                }
            }
        }
        for &t in &tri_b {
            if process_sphere_triangle(&sa[i], b, t, &mut points[n], &mut min_d) {
                if multi_points_per_sphere && orig.surf.piece <= 2 && n < 31 {
                    n += 1;
                    hit = false;
                    min_d = 1e24;
                    points[n].depth = -1.0;
                } else {
                    hit = true;
                }
            }
        }
        if hit {
            if n >= 31 {
                break 'spheres;
            }
            n += 1;
            points[n].depth = -1.0;
        }
    }

    // Step 3a: lines of A (wheel probes), results to world.
    for (l, line) in a.lines.iter().enumerate() {
        let lb = ColLine { start: m_ab.transform(line.start), end: m_ab.transform(line.end) };
        let mut hit = false;
        for &j in &list_b {
            if process_line_sphere(&lb, &b.spheres[j], &mut line_points[l], &mut line_values[l]) {
                hit = true;
            }
        }
        for &k in &box_b {
            if process_line_box(&lb, &b.boxes[k], &mut line_points[l], &mut line_values[l]) {
                hit = true;
            }
        }
        for &t in &tri_b {
            if process_line_triangle(&lb, b, t, &mut line_points[l], &mut line_values[l]) {
                hit = true;
            }
        }
        if hit {
            line_points[l].point = mat_b.transform(line_points[l].point);
            line_points[l].normal = mat_b.rotate(line_points[l].normal);
        }
    }

    // Step 4: to world.
    for p in points.iter_mut().take(n) {
        p.point = mat_b.transform(p.point);
        p.normal = mat_b.rotate(p.normal);
    }

    // Step 5: B spheres vs A boxes and triangles (in A space, roles swapped).
    if !list_b.is_empty() && (!a.tris.is_empty() || !a.boxes.is_empty()) {
        let bs_b = m_ba.transform(b.bound_center);
        let rb = b.bound_radius;
        let bound_sphere_b = ColSphere { center: bs_b, radius: rb, surf: Surf::default() };
        let tri_a: Vec<usize> = (0..a.tris.len()).filter(|&t| test_sphere_triangle(&bound_sphere_b, a, t)).take(599).collect();
        let box_a: Vec<usize> = (0..a.boxes.len())
            .filter(|&k| {
                let bx = &a.boxes[k];
                bx.min.x <= bs_b.x + rb
                    && bx.min.y <= bs_b.y + rb
                    && bx.min.z <= bs_b.z + rb
                    && bx.max.x >= bs_b.x - rb
                    && bx.max.y >= bs_b.y - rb
                    && bx.max.z >= bs_b.z - rb
            })
            .collect();
        let mut rev = 0usize;
        points[n].depth = -1.0;
        if !tri_a.is_empty() {
            for &j in &list_b {
                let mut min_d = 1e24f32;
                let mut hit = false;
                for &t in &tri_a {
                    if process_sphere_triangle(&sb[j], a, t, &mut points[n], &mut min_d) {
                        hit = true;
                    }
                }
                if hit {
                    points[n].normal *= -1.0;
                    if n >= 31 {
                        break;
                    }
                    n += 1;
                    rev += 1;
                    points[n].depth = -1.0;
                }
            }
        }
        if !box_a.is_empty() {
            for &j in &list_b {
                let mut min_d = 1e24f32;
                for &k in &box_a {
                    if process_sphere_box(&sb[j], &a.boxes[k], &mut points[n], &mut min_d) {
                        set_a(&mut points[n], a.boxes[k].surf);
                        set_b(&mut points[n], b.spheres[j].surf);
                        points[n].normal *= -1.0;
                        if n >= 31 {
                            continue;
                        }
                        n += 1;
                        rev += 1;
                        points[n].depth = -1.0;
                    }
                }
            }
        }
        for p in points[n - rev..n].iter_mut() {
            p.point = mat_a.transform(p.point);
            p.normal = mat_a.rotate(p.normal);
            // Swap the A and B surface groups (box contacts end up swapped twice: original quirk).
            std::mem::swap(&mut p.surface_a, &mut p.surface_b);
            std::mem::swap(&mut p.piece_a, &mut p.piece_b);
            std::mem::swap(&mut p.lighting_a, &mut p.lighting_b);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ground() -> ColModel {
        // A 20x20 quad at z = 0 made of two triangles, normal +Z.
        let verts = vec![
            Vec3::new(-10.0, -10.0, 0.0),
            Vec3::new(10.0, -10.0, 0.0),
            Vec3::new(10.0, 10.0, 0.0),
            Vec3::new(-10.0, 10.0, 0.0),
        ];
        let tris = vec![
            ColTriangle { v: [0, 2, 1], material: 1, light: 0 },
            ColTriangle { v: [0, 3, 2], material: 1, light: 0 },
        ];
        let planes = tris.iter().map(|t| TrianglePlane::new(verts[t.v[0] as usize], verts[t.v[1] as usize], verts[t.v[2] as usize])).collect();
        ColModel {
            bbox_min: Vec3::new(-10.0, -10.0, -0.1),
            bbox_max: Vec3::new(10.0, 10.0, 0.1),
            bound_center: Vec3::ZERO,
            bound_radius: 14.2,
            verts,
            tris,
            planes,
            ..Default::default()
        }
    }

    fn ball(r: f32) -> ColModel {
        ColModel {
            bbox_min: Vec3::splat(-r),
            bbox_max: Vec3::splat(r),
            bound_radius: r,
            spheres: vec![ColSphere { center: Vec3::ZERO, radius: r, surf: Surf { material: 7, piece: 0, lighting: 0 } }],
            ..Default::default()
        }
    }

    #[test]
    fn plane_quantisation() {
        let p = TrianglePlane::new(Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(p.normal, Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(p.orient, 4);
    }

    #[test]
    fn sphere_resting_on_ground_gives_upward_normal() {
        let g = ground();
        let b = ball(0.5);
        let mat_a = Matrix { pos: Vec3::new(1.0, 2.0, 0.4), ..Matrix::IDENTITY };
        let mut pts = [ColPoint::default(); MAX_COLPOINTS];
        let n = process_col_models(&mat_a, &b, &Matrix::IDENTITY, &g, &mut pts, &mut [], &mut [], false);
        assert_eq!(n, 1);
        assert!((pts[0].normal - Vec3::Z).length() < 1e-3, "{:?}", pts[0].normal);
        assert!((pts[0].depth - 0.1).abs() < 1e-3);
        assert_eq!(pts[0].surface_a, 7);
        assert_eq!(pts[0].surface_b, 1);
    }

    #[test]
    fn wheel_line_hits_ground_at_fraction() {
        let g = ground();
        let mut car = ball(0.5);
        // Like real vehicles, the bounding sphere encloses the wheel lines.
        car.bound_radius = 2.0;
        car.lines.push(ColLine { start: Vec3::new(0.0, 0.0, 0.0), end: Vec3::new(0.0, 0.0, -2.0) });
        let mat_a = Matrix { pos: Vec3::new(0.0, 0.0, 1.5), ..Matrix::IDENTITY };
        let mut pts = [ColPoint::default(); MAX_COLPOINTS];
        let mut lp = [ColPoint::default(); 1];
        let mut lv = [1.0f32; 1];
        process_col_models(&mat_a, &car, &Matrix::IDENTITY, &g, &mut pts, &mut lp, &mut lv, false);
        assert!((lv[0] - 0.75).abs() < 1e-4, "{}", lv[0]);
        assert!((lp[0].point.z).abs() < 1e-4);
    }
}
