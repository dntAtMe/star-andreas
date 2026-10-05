//! Breakable objects: `BreakManager_c` / `BreakObject_c` (object_damage.md §3). A broken
//! object with BreakablePlugin data falls apart into pieces (one per material, or one per
//! triangle when smashed) that fly, bounce on the ground plane under the object, settle and
//! fade out.

use std::sync::Arc;

use glam::{Mat3, Vec3};
use sa_formats::dff::Breakable;

use crate::{
    damage::Rand,
    effects::{FrameFx, PrtMult},
    physical::Matrix,
};

/// `g_breakMan`: 64 BreakObject_c slots.
pub const MAX_BREAK_OBJECTS: usize = 64;

/// One triangle of a piece (piece-local positions, baked RGB).
#[derive(Debug, Clone)]
pub struct BreakTri {
    pub pos: [Vec3; 3],
    pub uv: [[f32; 2]; 3],
    pub col: [[u8; 3]; 3],
}

/// A piece (0x74 bytes).
#[derive(Debug, Clone)]
pub struct BreakPiece {
    pub matrix: Matrix,
    pub vel: Vec3,
    pub landed: bool,
    pub tris: Vec<BreakTri>,
    /// Breakable material index of the texture.
    pub material: Option<usize>,
    /// 0 right, 1 forward (RW up), 2 up (RW at).
    thin_axis: u8,
    half_thickness: f32,
    /// Degrees per tick.
    spin_speed: f32,
    spin_axis: Vec3,
    /// Frames.
    pub life: i32,
}

impl BreakPiece {
    /// Render alpha: smash pieces fade over their last 32 frames, others over 128.
    pub fn alpha(&self, smash: bool) -> u8 {
        (if smash { self.life * 8 } else { self.life * 2 }).clamp(0, 255) as u8
    }
}

/// `BreakObject_c`.
#[derive(Debug, Clone)]
pub struct BreakObject {
    /// Unique per added object (for the app's render entities).
    pub uid: u32,
    pub smash: bool,
    pub sparks: bool,
    /// Draw in the last pass (entity flag 0x4000).
    pub draw_last: bool,
    pub pieces: Vec<BreakPiece>,
    pub data: Arc<Breakable>,
    frames_alive: i32,
    ground_z: f32,
    ground_normal: Vec3,
}

/// `BreakManager_c`.
#[derive(Debug, Default)]
pub struct BreakManager {
    pub objects: Vec<BreakObject>,
    next_uid: u32,
}

/// The arguments of `BreakManager_c::Add` that the world resolves.
#[derive(Debug, Clone)]
pub struct BreakRequest {
    pub data: Arc<Breakable>,
    /// The object's frame LTM.
    pub matrix: Matrix,
    /// Its col model bbox (model space).
    pub bbox: (Vec3, Vec3),
    pub vel: Vec3,
    pub vel_rand: f32,
    pub smash: bool,
    pub sparks: bool,
}

fn rand_unit_vec(rng: &mut Rand) -> Vec3 {
    // Each component is `rand*k + rand*k - 1` with the SAME rand value.
    let mut c = || {
        let u = rng.rand01();
        u + u - 1.0
    };
    let v = Vec3::new(c(), c(), c());
    v.normalize_or_zero()
}

/// `RwMatrixRotate(m, axis, angle_deg, combine)` on the rotation part.
fn rotate(m: &mut Matrix, axis: Vec3, deg: f32, pre: bool) {
    let r = Mat3::from_axis_angle(axis, deg.to_radians());
    let cur = Mat3::from_cols(m.right, m.fwd, m.up);
    // PRECONCAT rotates about the piece's local axis, POSTCONCAT about the world axis.
    let out = if pre { cur * r } else { r * cur };
    m.right = out.x_axis;
    m.fwd = out.y_axis;
    m.up = out.z_axis;
}

impl BreakObject {
    /// `CreatePieces` (0x59D7F0) + `SetPieceMatrixAndVelocity` (0x59D570).
    fn create(uid: u32, req: &BreakRequest, ambient255: Vec3, rng: &mut Rand) -> Self {
        let br = &req.data;
        let smash = req.smash;
        let n = if smash { br.triangles.len() } else { br.tex_names.len() };
        let mut pieces: Vec<BreakPiece> = (0..n)
            .map(|_| {
                let life = 256 - (rng.unit() * -32.0) as i32;
                BreakPiece {
                    matrix: req.matrix,
                    vel: Vec3::ZERO,
                    landed: false,
                    tris: Vec::new(),
                    material: None,
                    thin_axis: 0,
                    half_thickness: 0.0,
                    spin_speed: 0.0,
                    spin_axis: Vec3::Z,
                    life,
                }
            })
            .collect();
        for (t, tri) in br.triangles.iter().enumerate() {
            let m = br.tri_material.get(t).copied().unwrap_or(0) as usize;
            let p = if smash { t } else { m };
            let Some(piece) = pieces.get_mut(p) else { continue };
            piece.material = Some(m);
            let mc = br.mat_colors.get(m).copied().unwrap_or([1.0; 3]);
            let mut out = BreakTri { pos: [Vec3::ZERO; 3], uv: [[0.0; 2]; 3], col: [[0; 3]; 3] };
            for k in 0..3 {
                let v = tri[k] as usize;
                out.pos[k] = Vec3::from(br.vertices.get(v).copied().unwrap_or_default());
                out.uv[k] = br.uvs.get(v).copied().unwrap_or_default();
                let c = br.colors.get(v).copied().unwrap_or([255; 4]);
                for ch in 0..3 {
                    out.col[k][ch] = (c[ch] as f32 * mc[ch] + ambient255[ch]).min(255.0) as i32 as u8;
                }
            }
            piece.tris.push(out);
        }
        // SetPieceMatrixAndVelocity: centre each piece, find its thin axis, random velocity / spin.
        let r = req.vel_rand;
        for piece in &mut pieces {
            let (mut lo, mut hi) = (Vec3::splat(9_999_999.0), Vec3::splat(-9_999_999.0));
            for t in &piece.tris {
                for p in t.pos {
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
            if !piece.tris.is_empty() {
                let c = (lo + hi) * 0.5;
                for t in &mut piece.tris {
                    for p in &mut t.pos {
                        *p -= c;
                    }
                }
                piece.matrix.pos += piece.matrix.rotate(c);
                let ext = hi - lo;
                piece.thin_axis = if ext.x <= ext.y && ext.x <= ext.z {
                    0
                } else if ext.y <= ext.z {
                    1
                } else {
                    2
                };
                piece.half_thickness = ext[piece.thin_axis as usize] * 0.5;
            }
            let mut u = || (r - -r) * rng.rand01() + -r;
            piece.vel = req.vel;
            if r != 0.0 {
                piece.vel += Vec3::new(u(), u(), u());
            }
            piece.spin_speed = rng.rand01() * 3.0 + 3.0;
            piece.spin_axis = rand_unit_vec(rng);
            piece.landed = false;
        }
        Self {
            uid,
            smash,
            sparks: req.sparks,
            draw_last: false,
            pieces,
            data: req.data.clone(),
            frames_alive: 0,
            ground_z: -1000.0,
            ground_normal: Vec3::Z,
        }
    }

    fn axis(m: &Matrix, a: u8) -> Vec3 {
        match a {
            0 => m.right,
            1 => m.fwd,
            _ => m.up,
        }
    }

    /// `BreakObject_c::Update` (0x59E220). Returns false when every piece is dead (`Exit`).
    fn update(&mut self, ts: f32, f: &mut FrameFx) -> bool {
        let mut all_dead = true;
        let (gz, gn) = (self.ground_z, self.ground_normal);
        for i in 0..self.pieces.len() {
            let p = &mut self.pieces[i];
            if !p.landed {
                p.vel.z -= ts * 0.008;
                p.matrix.pos += p.vel * ts;
                if self.frames_alive < 5 {
                    rotate(&mut p.matrix, p.spin_axis, ts * p.spin_speed, true);
                } else {
                    let a = Self::axis(&p.matrix, p.thin_axis);
                    let ang = gn.dot(a).clamp(-1.0, 1.0).acos();
                    if ang.abs() > 0.01 {
                        let ax = a.cross(gn).normalize_or_zero();
                        if ax != Vec3::ZERO {
                            rotate(&mut p.matrix, ax, ang * 57.295_78 * ts * 0.05, false);
                        }
                    }
                }
                if p.matrix.pos.z - p.half_thickness < gz {
                    self.collision_response(i, ts, f);
                }
            }
            let p = &mut self.pieces[i];
            p.life -= 1;
            if p.life < 1 {
                p.life = 0;
            } else {
                all_dead = false;
            }
        }
        self.frames_alive += 1;
        !(all_dead || self.pieces.is_empty())
    }

    /// `DoCollisionResponse` (0x59DE40).
    fn collision_response(&mut self, i: usize, ts: f32, f: &mut FrameFx) {
        let (gz, n, smash, sparks) = (self.ground_z, self.ground_normal, self.smash, self.sparks);
        let p = &mut self.pieces[i];
        let r = p.vel - n * (2.0 * (0.85 * p.vel.dot(n)));
        let j = rand_unit_vec(f.rng) * (0.05 * ts);
        let s = r.length();
        let r = (r + j).normalize_or_zero() * s;
        p.spin_speed = 0.0;
        p.vel = r * 0.8;
        p.matrix.pos.z = gz + p.half_thickness;
        if s < 0.05 {
            p.landed = true;
            if smash {
                p.life = 32 - (f.rng.unit() * -32.0) as i32;
            }
        }
        if smash {
            return;
        }
        let pos = p.matrix.pos;
        let speed = p.vel.length();
        let mult = PrtMult::new(1.0, 1.0, 1.0, 0.1, 0.3, 0.0, 0.15);
        for _ in 0..4 {
            let at = pos + Vec3::new(f.rng.rand01() - 0.5, f.rng.rand01() - 0.5, 0.0);
            let v = Vec3::new(0.3 * f.rng.rand01() - 0.15, 0.3 * f.rng.rand01() - 0.15, 0.0);
            f.fx.add_particle("prt_smokeII_3_expand", at, v, 0.0, mult, -1.0, 1.2, 0.6, false);
        }
        if sparks {
            f.add_sparks(pos, Vec3::Z, 2.0, (speed * 100.0) as i32, Vec3::ZERO, true, 0.4, 1.0);
        }
    }
}

impl BreakManager {
    /// `BreakManager_c::Add` → `BreakObject_c::Init` (0x59E750); `ground` = the
    /// ProcessVerticalLine result below the probe point (buildings only).
    pub fn add(&mut self, req: &BreakRequest, ambient255: Vec3, rng: &mut Rand, ground: impl FnOnce(Vec3) -> Option<(f32, Vec3)>) -> bool {
        if self.objects.len() >= MAX_BREAK_OBJECTS {
            return false;
        }
        self.next_uid = self.next_uid.wrapping_add(1);
        let mut o = BreakObject::create(self.next_uid, req, ambient255, rng);
        let m = &req.matrix;
        let (lo, hi) = req.bbox;
        let probe = if req.data.position_rule == 0 {
            Vec3::new(m.pos.x, m.pos.y, m.pos.z + lo.z + 0.25)
        } else {
            m.transform(Vec3::new((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5, lo.z + 0.5 * (hi.z - lo.z) + 0.25))
        };
        if let Some((z, mut n)) = ground(probe) {
            if n.x.abs() < 0.01 && n.y.abs() < 0.01 && n.z.abs() < 0.01 {
                n = Vec3::Z;
            }
            o.ground_z = z;
            o.ground_normal = n;
        }
        self.objects.push(o);
        true
    }

    /// `BreakManager_c::Update` (0x59E670).
    pub fn update(&mut self, ts: f32, f: &mut FrameFx) {
        self.objects.retain_mut(|o| o.update(ts, f));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad_data() -> Arc<Breakable> {
        Arc::new(Breakable {
            position_rule: 1,
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
            uvs: vec![[0.0; 2]; 4],
            colors: vec![[100, 100, 100, 255]; 4],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            tri_material: vec![0, 1],
            tex_names: vec!["a".into(), "b".into()],
            mask_names: vec![String::new(), String::new()],
            mat_colors: vec![[1.0; 3]; 2],
        })
    }

    #[test]
    fn pieces_fall_land_and_die() {
        let mut man = BreakManager::default();
        let mut rng = Rand::new(7);
        let req = BreakRequest {
            data: quad_data(),
            matrix: Matrix { pos: Vec3::new(0.0, 0.0, 2.0), ..Matrix::IDENTITY },
            bbox: (Vec3::ZERO, Vec3::new(1.0, 0.1, 1.0)),
            vel: Vec3::new(0.0, 0.0, 0.05),
            vel_rand: 0.02,
            smash: false,
            sparks: false,
        };
        assert!(man.add(&req, Vec3::splat(20.0), &mut rng, |_| Some((0.0, Vec3::Z))));
        let o = &man.objects[0];
        assert_eq!(o.pieces.len(), 2);
        assert_eq!(o.pieces[0].tris[0].col[0], [120, 120, 120]);
        let mut fx = crate::effects::Effects::default();
        let mut reqs = Vec::new();
        let surfaces = crate::world::World::default().surfaces.clone();
        for _ in 0..400 {
            let mut f = FrameFx {
                fx: &mut fx,
                requests: &mut reqs,
                now_ms: 0,
                frame: 0,
                ts: 1.0,
                rng: &mut rng,
                cam: Vec3::ZERO,
                cam_planes: [(Vec3::ZERO, 0.0); 4],
                wet_roads: 0.0,
                foggyness: 0.0,
                hours: 12,
                minutes: 0,
                cam_fwd: Vec3::Y,
                sprite_brightness: 10.0,
                player_in_vehicle: false,
                surfaces: &surfaces,
            };
            man.update(1.0, &mut f);
            if let Some(o) = man.objects.first() {
                for p in &o.pieces {
                    assert!(p.matrix.pos.z >= p.half_thickness - 0.05, "below ground {}", p.matrix.pos.z);
                }
            }
        }
        assert!(man.objects.is_empty());
    }
}
