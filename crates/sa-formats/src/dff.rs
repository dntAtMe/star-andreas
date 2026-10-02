//! DFF models: a RenderWare clump (frames, geometries, materials, atomics).

use anyhow::{Context, Result, bail};

use crate::{
    bin::Reader,
    rw::{self, id},
};

#[derive(Debug, Clone)]
pub struct Frame {
    /// Columns: right, up, at (RW convention, point = r*x + u*y + a*z + pos).
    pub rot: [[f32; 3]; 3],
    pub pos: [f32; 3],
    pub parent: i32,
    pub name: String,
    pub hanim: Option<HAnim>,
}

/// HAnim plugin on a frame. Every bone frame carries its `node_id`; the
/// skeleton root additionally lists all bones in skin-index order.
#[derive(Debug, Clone)]
pub struct HAnim {
    pub node_id: i32,
    /// (node id, node index, flags) in hierarchy order; empty on non-root bones.
    pub nodes: Vec<(i32, i32, u32)>,
}

/// Skin plugin on a geometry.
#[derive(Debug, Clone)]
pub struct Skin {
    pub num_bones: usize,
    /// Per vertex: 4 bone indices (into the HAnim node list) and weights.
    pub indices: Vec<[u8; 4]>,
    pub weights: Vec<[f32; 4]>,
    /// Per bone: inverse of the bone's model-space matrix, column-major 4x4.
    pub inverse_bind: Vec<[f32; 16]>,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub mask: String,
    /// Raw filter/addressing word from the texture struct.
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct Material {
    pub color: [u8; 4],
    pub texture: Option<Texture>,
}

#[derive(Debug, Clone, Copy)]
pub struct Triangle {
    pub v: [u16; 3],
    pub material: u16,
}

#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub flags: u32,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub prelit: Vec<[u8; 4]>,
    /// One UV set per texture coordinate channel.
    pub uvs: Vec<Vec<[f32; 2]>>,
    pub triangles: Vec<Triangle>,
    pub materials: Vec<Material>,
    /// Second prelit set from the Extra Vert Colour plugin (SA day/night blend).
    pub extra_colors: Vec<[u8; 4]>,
    pub skin: Option<Skin>,
}

pub mod geo_flags {
    pub const TRISTRIP: u32 = 0x01;
    pub const POSITIONS: u32 = 0x02;
    pub const TEXTURED: u32 = 0x04;
    pub const PRELIT: u32 = 0x08;
    pub const NORMALS: u32 = 0x10;
    pub const LIGHT: u32 = 0x20;
    pub const MODULATE_MATERIAL_COLOR: u32 = 0x40;
    pub const TEXTURED2: u32 = 0x80;
    pub const NATIVE: u32 = 0x0100_0000;
}

#[derive(Debug, Clone, Copy)]
pub struct Atomic {
    pub frame: u32,
    pub geometry: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Clump {
    pub frames: Vec<Frame>,
    pub geometries: Vec<Geometry>,
    pub atomics: Vec<Atomic>,
    /// Embedded COL model (vehicles), raw bytes starting at its fourcc.
    pub collision: Option<Vec<u8>>,
}

impl Clump {
    /// World-space (model-space) matrix of a frame as (rot columns, pos),
    /// accumulated through its parents.
    pub fn frame_world(&self, mut idx: usize) -> ([[f32; 3]; 3], [f32; 3]) {
        let mut rot = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut pos = [0.0f32; 3];
        let mut guard = 0;
        while idx < self.frames.len() && guard < 64 {
            let f = &self.frames[idx];
            // child-to-world = parent * current
            let new_pos = apply(&f.rot, pos);
            pos = [new_pos[0] + f.pos[0], new_pos[1] + f.pos[1], new_pos[2] + f.pos[2]];
            rot = [apply(&f.rot, rot[0]), apply(&f.rot, rot[1]), apply(&f.rot, rot[2])];
            if f.parent < 0 {
                break;
            }
            idx = f.parent as usize;
            guard += 1;
        }
        (rot, pos)
    }
}

/// Multiply a column-major 3x3 matrix by a vector.
pub fn apply(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// Parse a DFF file. Skips any leading non-clump chunks (e.g. UV anim dicts).
pub fn parse(data: &[u8]) -> Result<Clump> {
    let mut r = Reader::new(data);
    while r.remaining() >= 12 {
        let h = rw::header(&mut r)?;
        let body = Reader::new(r.bytes(h.size)?);
        if h.ty == id::CLUMP {
            return parse_clump(body);
        }
    }
    bail!("no clump chunk found")
}

fn parse_clump(mut r: Reader) -> Result<Clump> {
    let (_, mut s) = rw::sub(&mut r, id::STRUCT)?;
    let num_atomics = s.u32()? as usize;

    let (_, fl) = rw::sub(&mut r, id::FRAME_LIST)?;
    let frames = parse_frame_list(fl).context("frame list")?;

    let (_, gl) = rw::sub(&mut r, id::GEOMETRY_LIST)?;
    let geometries = parse_geometry_list(gl).context("geometry list")?;

    let mut atomics = Vec::with_capacity(num_atomics);
    let mut collision = None;
    for child in rw::children(r) {
        let (h, mut body) = child?;
        match h.ty {
            id::ATOMIC => {
                let (_, mut s) = rw::sub(&mut body, id::STRUCT)?;
                atomics.push(Atomic { frame: s.u32()?, geometry: s.u32()?, flags: s.u32()? });
            }
            id::EXTENSION => {
                for ext in rw::children(body) {
                    let (eh, mut eb) = ext?;
                    if eh.ty == id::COLLISION {
                        collision = Some(eb.bytes(eh.size)?.to_vec());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Clump { frames, geometries, atomics, collision })
}

fn parse_frame_list(mut r: Reader) -> Result<Vec<Frame>> {
    let (_, mut s) = rw::sub(&mut r, id::STRUCT)?;
    let n = s.u32()? as usize;
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        let rot = [s.vec3()?, s.vec3()?, s.vec3()?];
        let pos = s.vec3()?;
        let parent = s.i32()?;
        s.u32()?; // matrix flags
        frames.push(Frame { rot, pos, parent, name: String::new(), hanim: None });
    }
    // One extension per frame; may carry the node name plugin.
    for frame in frames.iter_mut() {
        if r.remaining() < 12 {
            break;
        }
        let (_, ext) = rw::sub(&mut r, id::EXTENSION)?;
        for child in rw::children(ext) {
            let (h, mut body) = child?;
            match h.ty {
                id::NODE_NAME => frame.name = String::from_utf8_lossy(body.bytes(h.size)?).into_owned(),
                id::HANIM => frame.hanim = Some(parse_hanim(body)?),
                _ => {}
            }
        }
    }
    Ok(frames)
}

fn parse_geometry_list(mut r: Reader) -> Result<Vec<Geometry>> {
    let (_, mut s) = rw::sub(&mut r, id::STRUCT)?;
    let n = s.u32()? as usize;
    (0..n)
        .map(|i| {
            let (h, body) = rw::sub(&mut r, id::GEOMETRY)?;
            parse_geometry(body, h.version).with_context(|| format!("geometry {i}"))
        })
        .collect()
}

fn parse_geometry(mut r: Reader, version: u32) -> Result<Geometry> {
    use geo_flags::*;
    let (_, mut s) = rw::sub(&mut r, id::STRUCT)?;
    let flags = s.u32()?;
    let num_tris = s.u32()? as usize;
    let num_verts = s.u32()? as usize;
    let num_morph = s.u32()? as usize;
    if version < 0x34000 {
        s.skip(12)?; // ambient, specular, diffuse
    }
    let mut g = Geometry { flags, ..Default::default() };

    if flags & NATIVE == 0 {
        if flags & PRELIT != 0 {
            g.prelit = (0..num_verts)
                .map(|_| Ok([s.u8()?, s.u8()?, s.u8()?, s.u8()?]))
                .collect::<Result<_>>()?;
        }
        let mut num_uv = ((flags >> 16) & 0xFF) as usize;
        if num_uv == 0 {
            num_uv = if flags & TEXTURED2 != 0 {
                2
            } else if flags & TEXTURED != 0 {
                1
            } else {
                0
            };
        }
        for _ in 0..num_uv {
            g.uvs.push((0..num_verts).map(|_| Ok([s.f32()?, s.f32()?])).collect::<Result<_>>()?);
        }
        g.triangles = (0..num_tris)
            .map(|_| {
                let v2 = s.u16()?;
                let v1 = s.u16()?;
                let material = s.u16()?;
                let v3 = s.u16()?;
                Ok(Triangle { v: [v1, v2, v3], material })
            })
            .collect::<Result<_>>()?;
    }

    // Only the first morph target is used by the game for static models.
    for m in 0..num_morph {
        s.skip(16)?; // bounding sphere
        let has_verts = s.u32()? != 0;
        let has_normals = s.u32()? != 0;
        let read = |s: &mut Reader| -> Result<Vec<[f32; 3]>> {
            (0..num_verts).map(|_| s.vec3()).collect()
        };
        let verts = if has_verts { read(&mut s)? } else { Vec::new() };
        let normals = if has_normals { read(&mut s)? } else { Vec::new() };
        if m == 0 {
            g.positions = verts;
            g.normals = normals;
        }
    }

    let (_, ml) = rw::sub(&mut r, id::MATERIAL_LIST)?;
    g.materials = parse_material_list(ml).context("material list")?;

    if r.remaining() >= 12 {
        let (_, ext) = rw::sub(&mut r, id::EXTENSION)?;
        for child in rw::children(ext) {
            let (h, mut body) = child?;
            match h.ty {
                id::EXTRA_VERT_COLOUR if body.u32()? != 0 => {
                    g.extra_colors = (0..num_verts)
                        .map(|_| Ok([body.u8()?, body.u8()?, body.u8()?, body.u8()?]))
                        .collect::<Result<_>>()?;
                }
                id::SKIN => g.skin = Some(parse_skin(body, num_verts).context("skin")?),
                _ => {}
            }
        }
    }
    Ok(g)
}

fn parse_material_list(mut r: Reader) -> Result<Vec<Material>> {
    let (_, mut s) = rw::sub(&mut r, id::STRUCT)?;
    let n = s.u32()? as usize;
    let indices: Vec<i32> = (0..n).map(|_| s.i32()).collect::<Result<_>>()?;
    let mut out: Vec<Material> = Vec::with_capacity(n);
    for idx in indices {
        if idx >= 0 {
            let m = out.get(idx as usize).cloned().context("material instance index")?;
            out.push(m);
        } else {
            let (_, body) = rw::sub(&mut r, id::MATERIAL)?;
            out.push(parse_material(body)?);
        }
    }
    Ok(out)
}

fn parse_material(mut r: Reader) -> Result<Material> {
    let (_, mut s) = rw::sub(&mut r, id::STRUCT)?;
    s.u32()?; // flags
    let color = [s.u8()?, s.u8()?, s.u8()?, s.u8()?];
    s.i32()?; // unused
    let textured = s.u32()? != 0;
    let texture = if textured {
        let (_, mut t) = rw::sub(&mut r, id::TEXTURE)?;
        let (_, mut ts) = rw::sub(&mut t, id::STRUCT)?;
        let flags = ts.u32()?;
        let name = rw::string(&mut t)?;
        let mask = rw::string(&mut t)?;
        Some(Texture { name, mask, flags })
    } else {
        None
    };
    Ok(Material { color, texture })
}

fn parse_hanim(mut r: Reader) -> Result<HAnim> {
    let _version = r.u32()?;
    let node_id = r.i32()?;
    let count = r.u32()? as usize;
    let mut nodes = Vec::with_capacity(count);
    if count > 0 {
        r.u32()?; // flags
        r.u32()?; // key frame size
        for _ in 0..count {
            nodes.push((r.i32()?, r.i32()?, r.u32()?));
        }
    }
    Ok(HAnim { node_id, nodes })
}

fn parse_skin(mut r: Reader, num_verts: usize) -> Result<Skin> {
    let num_bones = r.u8()? as usize;
    let num_used = r.u8()? as usize;
    let max_weights = r.u8()?;
    r.u8()?;
    r.skip(num_used)?; // used bone list
    let indices = (0..num_verts).map(|_| Ok([r.u8()?, r.u8()?, r.u8()?, r.u8()?])).collect::<Result<_>>()?;
    let weights = (0..num_verts).map(|_| Ok([r.f32()?, r.f32()?, r.f32()?, r.f32()?])).collect::<Result<_>>()?;
    let mut inverse_bind = Vec::with_capacity(num_bones);
    for _ in 0..num_bones {
        if max_weights == 0 {
            r.u32()?; // 0xDEADDEAD marker in older skins
        }
        let mut m = [0f32; 16];
        for v in m.iter_mut() {
            *v = r.f32()?;
        }
        // RW leaves junk in the w components; force an affine matrix.
        m[3] = 0.0;
        m[7] = 0.0;
        m[11] = 0.0;
        m[15] = 1.0;
        inverse_bind.push(m);
    }
    Ok(Skin { num_bones, indices, weights, inverse_bind })
}
