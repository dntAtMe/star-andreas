//! StarCraft II `.m3` models (MD34): static geometry, skeleton rest pose and
//! material texture paths. Layouts follow m3studio's `structures.xml`.
//!
//! File layout: a 24-byte `MD34` header (tag, index offset, index count, MODL
//! reference) and a section index of `(tag, offset, count, version)` entries.
//! Every struct field pointing at other data is a `Reference` `(entries, section
//! index, flags)`; section 0 is the header itself, so index 0 means "none".
//!
//! Conventions of the returned [`Model`] (untouched from the file):
//! - Right-handed, **Z-up**, X/Y the ground plane; 1 unit = 1 game cell
//!   (a marine's mesh is ~0.9 tall, a command center's ~5.6 wide).
//! - Triangles are counter-clockwise when seen from the front (outside), i.e.
//!   `cross(b - a, c - a)` points along the vertex normals.
//! - UVs are D3D-style (origin top-left, V down), ready for sampling the DDS
//!   textures as stored: `uv = i16 * uv_multiply / 32768 + uv_offset`, with the
//!   region's `uv_multiply`/`uv_offset` (REGN v5+, otherwise 16 and 0, which is
//!   the classic `i16 / 2048`). Values may exceed 0..1 (texture wrapping).
//! - Matrices are column-major: `m[c]` is column `c` (glam's `from_cols_array_2d`).
//! - Vertices are stored in bind pose (model space). Skinning matrix of bone `b`
//!   = `world(b) * bones[b].inverse_bind`, where `world` chains each bone's local
//!   TRS (`translation * rotation * scale`, relative to the parent) from the root.
//!   The bind pose itself is `inverse(inverse_bind)`. The `rest_*` values are the
//!   bones' default (un-animated) values: a valid pose, but not necessarily the
//!   bind pose (rotations often differ; skinning with them gives the default pose).
//!
//! Animations are not decoded yet. For the record: `MODL.sequences` (SEQS @+16:
//! name, ms range, flags) pairs 1:1 with `MODL.sequence_transformation_groups`
//! (STG_ @+40: list of STC_ indices into `MODL.sequence_transformation_collections`
//! @+28). Each STC_ holds parallel arrays `anim_ids` (u32) and `anim_refs` (u32:
//! track type in the high 16 bits indexing `[sdev, sd2v, sd3v, sd4q, sdcc, sdr3,
//! sdu8, sds6, sdu6, sds3, sdu3, sdfg, sdmb]`, entry index in the low 16); e.g. an
//! SD3V/SD4Q entry has `frames` (i32 ms) and `keys` (VEC3/QUAT). A bone's
//! location/rotation/scale animation references (BONE +24/+60/+104: header
//! `interpolation u16, flags u16, id u32`, then the default value) are found by
//! matching the header `id` against `anim_ids`.

use anyhow::{Context, Result, bail, ensure};

/// Parsed model, see the module docs for conventions.
#[derive(Clone, Debug, Default)]
pub struct Model {
    /// MODL struct version (23 for most Wings of Liberty era units, up to 30).
    pub version: u32,
    /// `MODL.vertex_flags`, describing the vertex layout.
    pub vertex_flags: u32,
    /// Size of one vertex in bytes, derived from `vertex_flags`.
    pub vertex_size: usize,
    pub bones: Vec<Bone>,
    pub meshes: Vec<Mesh>,
    /// One per MATM entry (material reference); [`Mesh::material`] indexes this.
    pub materials: Vec<Material>,
    /// `(min, max)` of the model bounding box.
    pub bounds: ([f32; 3], [f32; 3]),
}

#[derive(Clone, Debug)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    /// Default (non-animated) local transform relative to the parent.
    pub rest_translation: [f32; 3],
    /// Quaternion, xyzw.
    pub rest_rotation: [f32; 4],
    pub rest_scale: [f32; 3],
    /// Model space -> bone space at bind time (IREF), column-major.
    pub inverse_bind: [[f32; 4]; 4],
}

/// Geometry of one mesh region (DIV.regions); the material comes from the
/// region's first batch (BAT_).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    /// Unit normals; empty if the vertex format has none.
    pub normals: Vec<[f32; 3]>,
    /// Tangent xyz + handedness w (+-1, from the vertex sign byte, as written by
    /// m3studio relative to the V-down UVs); empty if not stored.
    pub tangents: Vec<[f32; 4]>,
    /// Vertex colors, RGBA; empty if the vertex format has none.
    pub colors: Vec<[u8; 4]>,
    /// First UV set; empty if the vertex format has none.
    pub uvs: Vec<[f32; 2]>,
    /// Indices into [`Model::bones`] (already resolved through the bone lookup).
    pub joints: Vec<[u16; 4]>,
    /// Bone weights; unused slots are 0 and the rest sums to 1.
    pub weights: Vec<[f32; 4]>,
    /// Triangle list, local to this mesh.
    pub indices: Vec<u32>,
    pub material: Option<usize>,
    /// Region has no batch (never drawn by the game) or is flagged hidden.
    pub hidden: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Material {
    pub name: String,
    /// MATM type: 1 standard, 2 displacement, 3 composite, 4 terrain, 5 volume,
    /// 7 creep, 8 volume noise, 9 splat terrain bake, 10 reflection,
    /// 11 lens flare, 12 buffer. Only standard materials carry all layers;
    /// for the others `diffuse` is their main color layer (composites resolve
    /// to their first section's material).
    pub kind: u32,
    /// Texture paths as stored, e.g. `Assets\Textures\Marine_Diffuse.dds`,
    /// relative to the asset package (`mods/liberty.sc2mod/base.sc2assets/`).
    pub diffuse: Option<String>,
    pub normal: Option<String>,
    pub specular: Option<String>,
    pub emissive: Option<String>,
    /// MAT_ blend mode: 0 opaque, 1 alpha blend, 2 add, 3 alpha add, 4 mod, 5 mod 2x.
    pub blend_mode: u32,
    /// MAT_ flags (0x8 two-sided, 0x10 unshaded, 0x20 no shadow cast, ...).
    pub flags: u32,
}

pub fn parse(data: &[u8]) -> Result<Model> {
    let f = File::new(data)?;
    let modl_ref = f.reference(12)?;
    let modl = f.items(modl_ref, b"MODL")?;
    ensure!(modl.n >= 1, "no MODL");
    let v = modl.ver;
    let m = modl.get(0);
    let at = |o: usize| f.reference_in(m, o);

    // Fields before the first optional MODL field are at fixed offsets.
    let mats_base = if v == 20 { 288 } else { 300 };
    let bone_rests = match v {
        20 => 564,
        21 | 23 => 576,
        24 => 588,
        25 => 600,
        26 => 612,
        28 => 636,
        29 => 648,
        30 => 660,
        _ => bail!("unsupported MODL version {v}"),
    };

    let vertex_flags = u32_at(m, 96);
    let fmt = VertexFormat::new(vertex_flags)?;
    let bounds = (vec3(m, 136), vec3(m, 148));

    let bones = parse_bones(&f, at(80)?, at(bone_rests)?).context("bones")?;
    let bone_lookup = f.u16s(at(124)?).context("bone lookup")?;
    let materials = parse_materials(&f, m, v, mats_base).context("materials")?;

    let verts = f.items(at(100)?, b"U8__")?.bytes();
    ensure!(verts.len() % fmt.size == 0, "vertex buffer {} not a multiple of {}", verts.len(), fmt.size);
    let mut meshes = Vec::new();
    let divs = f.items(at(112)?, b"DIV_")?;
    for d in 0..divs.n {
        let div = divs.get(d);
        let faces = f.u16s(f.reference_in(div, 0)?)?;
        let regions = f.items(f.reference_in(div, 12)?, b"REGN")?;
        let batches = f.items(f.reference_in(div, 24)?, b"BAT_")?;
        let batches: Vec<(usize, usize)> =
            (0..batches.n).map(|i| batches.get(i)).map(|b| (u16_at(b, 4) as usize, u16_at(b, 10) as usize)).collect();
        for r in 0..regions.n {
            let reg = Region::parse(regions.get(r), regions.ver);
            let material = batches.iter().find(|b| b.0 == r).map(|b| b.1).filter(|&i| i < materials.len());
            let hidden = reg.hidden || !batches.iter().any(|b| b.0 == r);
            let mut mesh = build_mesh(&fmt, verts, &faces, &bone_lookup, bones.len(), &reg)
                .with_context(|| format!("region {r}"))?;
            mesh.material = material;
            mesh.hidden = hidden;
            meshes.push(mesh);
        }
    }

    Ok(Model { version: v, vertex_flags, vertex_size: fmt.size, bones, meshes, materials, bounds })
}

// ---- file / references ----------------------------------------------------

struct Section {
    tag: [u8; 4],
    offset: usize,
    count: usize,
    version: u32,
}

struct File<'a> {
    d: &'a [u8],
    secs: Vec<Section>,
}

#[derive(Clone, Copy)]
struct Ref {
    n: usize,
    idx: usize,
}

/// `n` elements of `size` bytes from one section.
struct Items<'a> {
    d: &'a [u8],
    size: usize,
    n: usize,
    ver: u32,
}

impl<'a> Items<'a> {
    fn get(&self, i: usize) -> &'a [u8] {
        &self.d[i * self.size..(i + 1) * self.size]
    }
    fn bytes(&self) -> &'a [u8] {
        self.d
    }
}

impl<'a> File<'a> {
    fn new(d: &'a [u8]) -> Result<Self> {
        ensure!(d.len() >= 24, "file too small");
        match &d[..4] {
            b"43DM" => {}
            b"33DM" => bail!("MD33 (beta) models are not supported"),
            m => bail!("not an m3 file (magic {:?})", String::from_utf8_lossy(m)),
        }
        let (off, n) = (u32_at(d, 4) as usize, u32_at(d, 8) as usize);
        let idx = d.get(off..off + n * 16).context("section index out of range")?;
        let secs = idx
            .as_chunks::<16>()
            .0
            .iter()
            .map(|e| {
                let mut tag: [u8; 4] = e[..4].try_into().unwrap();
                tag.reverse();
                Section { tag, offset: u32_at(e, 4) as usize, count: u32_at(e, 8) as usize, version: u32_at(e, 12) }
            })
            .collect();
        Ok(Self { d, secs })
    }

    fn reference(&self, at: usize) -> Result<Ref> {
        self.reference_in(self.d, at)
    }

    fn reference_in(&self, b: &[u8], at: usize) -> Result<Ref> {
        let (n, idx) = (u32_at(b, at) as usize, u32_at(b, at + 4) as usize);
        ensure!(idx < self.secs.len(), "reference to section {idx} of {}", self.secs.len());
        Ok(Ref { n: if idx == 0 { 0 } else { n }, idx })
    }

    /// Resolves `r`, which must point at a `tag` section (or be empty).
    fn items(&self, r: Ref, tag: &[u8; 4]) -> Result<Items<'a>> {
        if r.n == 0 {
            return Ok(Items { d: &[], size: 1, n: 0, ver: 0 });
        }
        let s = &self.secs[r.idx];
        ensure!(&s.tag == tag, "expected {} section, got {}", tag.escape_ascii(), s.tag.escape_ascii());
        let size = elem_size(tag, s.version)
            .with_context(|| format!("unsupported {} version {}", tag.escape_ascii(), s.version))?;
        let n = r.n.min(s.count);
        let d = self.d.get(s.offset..s.offset + n * size).context("section out of range")?;
        Ok(Items { d, size, n, ver: s.version })
    }

    fn u16s(&self, r: Ref) -> Result<Vec<u16>> {
        Ok(self.items(r, b"U16_")?.bytes().as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect())
    }

    fn string(&self, r: Ref) -> Result<String> {
        let b = self.items(r, b"CHAR")?.bytes();
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }

    /// Reads the string referenced at `at` within `b`, `None` if empty.
    fn opt_string(&self, b: &[u8], at: usize) -> Result<Option<String>> {
        let s = self.string(self.reference_in(b, at)?)?;
        Ok(Some(s).filter(|s| !s.is_empty()))
    }
}

/// Element size of the sections this module reads, per struct version.
fn elem_size(tag: &[u8; 4], v: u32) -> Option<usize> {
    Some(match (tag, v) {
        (b"CHAR" | b"U8__", _) => 1,
        (b"U16_", _) => 2,
        (b"MODL", 20) => 748,
        (b"MODL", 21) => 760,
        (b"MODL", 23) => 784,
        (b"MODL", 24) => 796,
        (b"MODL", 25) => 808,
        (b"MODL", 26) => 820,
        (b"MODL", 28) => 844,
        (b"MODL", 29) => 856,
        (b"MODL", 30) => 868,
        (b"BONE", 1) => 160,
        (b"IREF", 0) => 64,
        (b"DIV_", 2) => 52,
        (b"REGN", 2) => 28,
        (b"REGN", 3) => 36,
        (b"REGN", 4) => 40,
        (b"REGN", 5) => 48,
        (b"BAT_", 1) => 14,
        (b"MATM", 0) => 8,
        (b"MAT_", 15) => 268,
        (b"MAT_", 16..=18) => 280,
        (b"MAT_", 19) => 340,
        (b"MAT_", 20) => 352,
        (b"LAYR", 20..=22) => 356,
        (b"LAYR", 23) => 428,
        (b"LAYR", 24) => 436,
        (b"LAYR", 25) => 468,
        (b"LAYR", 26) => 464,
        (b"DIS_", 4) => 68,
        (b"CMP_", 2) => 28,
        (b"CMS_", 0) => 24,
        (b"TER_", 0) | (b"CREP", 0) => 24,
        (b"TER_", 1) | (b"CREP", 1) => 28,
        (b"VOL_", 0) => 84,
        (b"HAI_", 0) => 116,
        (b"VON_", 0) => 268,
        (b"STBM", 0) => 48,
        (b"REF_", 1) => 84,
        (b"REF_", 2) => 156,
        (b"REF_", 3) => 160,
        (b"LFLR", 2) => 80,
        (b"LFLR", 3) => 152,
        (b"MADD", 1) => 140,
        (b"MADD", 2) => 152,
        (b"MADD", 3) => 160,
        (b"SCHR", 0) => 12,
        _ => return None,
    })
}

// ---- bones -----------------------------------------------------------------

fn parse_bones(f: &File, bones: Ref, irefs: Ref) -> Result<Vec<Bone>> {
    let bones = f.items(bones, b"BONE")?;
    let irefs = f.items(irefs, b"IREF")?;
    ensure!(irefs.n == bones.n || irefs.n == 0, "{} IREFs for {} bones", irefs.n, bones.n);
    (0..bones.n)
        .map(|i| {
            let b = bones.get(i);
            let parent = i16::from_le_bytes([b[20], b[21]]);
            let mut inverse_bind = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
            if irefs.n > 0 {
                let m = irefs.get(i);
                for (c, col) in inverse_bind.iter_mut().enumerate() {
                    *col = std::array::from_fn(|r| f32_at(m, c * 16 + r * 4));
                }
            }
            Ok(Bone {
                name: f.string(f.reference_in(b, 4)?)?,
                parent: usize::try_from(parent).ok().filter(|&p| p < bones.n),
                // Animation reference = 8-byte header, then the default value.
                rest_translation: vec3(b, 24 + 8),
                rest_rotation: std::array::from_fn(|k| f32_at(b, 60 + 8 + k * 4)),
                rest_scale: vec3(b, 104 + 8),
                inverse_bind,
            })
        })
        .collect()
}

// ---- materials -------------------------------------------------------------

fn parse_materials(f: &File, modl: &[u8], v: u32, base: usize) -> Result<Vec<Material>> {
    let matm = f.items(f.reference_in(modl, base)?, b"MATM")?;
    let refs: Vec<(u32, usize)> = (0..matm.n).map(|i| matm.get(i)).map(|m| (u32_at(m, 0), u32_at(m, 4) as usize)).collect();
    (0..refs.len()).map(|i| material(f, modl, v, base, &refs, i, 0).with_context(|| format!("material {i}"))).collect()
}

fn material(f: &File, modl: &[u8], v: u32, base: usize, refs: &[(u32, usize)], i: usize, depth: u32) -> Result<Material> {
    let (kind, idx) = refs[i];
    // MODL reference to the material list of each type, relative to `base`.
    let (delta, tag, since): (usize, &[u8; 4], u32) = match kind {
        1 => (12, b"MAT_", 0),
        2 => (24, b"DIS_", 0),
        3 => (36, b"CMP_", 0),
        4 => (48, b"TER_", 0),
        5 => (60, b"VOL_", 0),
        6 => (72, b"HAI_", 0),
        7 => (84, b"CREP", 0),
        8 => (96, b"VON_", 25),
        9 => (108, b"STBM", 26),
        10 => (120, b"REF_", 28),
        11 => (132, b"LFLR", 29),
        12 => (144, b"MADD", 30),
        _ => bail!("unknown material type {kind}"),
    };
    ensure!(v >= since, "material type {kind} in MODL v{v}");
    let list = f.items(f.reference_in(modl, base + delta)?, tag)?;
    ensure!(idx < list.n, "material index {idx} of {}", list.n);
    let (m, mv) = (list.get(idx), list.ver);
    let mut out = Material { name: f.string(f.reference_in(m, 0)?)?, kind, ..Default::default() };
    let layer = |at: usize| -> Result<Option<String>> {
        let l = f.items(f.reference_in(m, at)?, b"LAYR")?;
        if l.n == 0 { Ok(None) } else { f.opt_string(l.get(0), 4) }
    };
    match kind {
        1 => {
            out.flags = u32_at(m, 16);
            out.blend_mode = u32_at(m, 20);
            // diff, spec, emis1, norm
            let [d, s, e, n] = match mv {
                15 => [52, 76, 88, 160],
                16..=19 => [52, 76, 100, 172],
                _ => [64, 88, 112, 184],
            };
            out.diffuse = layer(d)?;
            out.specular = layer(s)?;
            out.emissive = layer(e)?;
            out.normal = layer(n)?;
        }
        2 => out.normal = layer(36)?,
        3 => {
            let secs = f.items(f.reference_in(m, 16)?, b"CMS_")?;
            if secs.n > 0 && depth < 4 {
                let sub = u32_at(secs.get(0), 0) as usize;
                ensure!(sub < refs.len(), "composite section -> material {sub}");
                let s = material(f, modl, v, base, refs, sub, depth + 1)?;
                out = Material { name: out.name, kind, ..s };
            }
        }
        4 | 7 | 11 => out.diffuse = layer(12)?,
        5 => out.diffuse = layer(40)?,
        6 => out.diffuse = layer(12)?,
        8 => out.diffuse = layer(80)?,
        9 => {
            out.diffuse = layer(12)?;
            out.normal = layer(24)?;
            out.specular = layer(36)?;
        }
        10 => out.normal = layer(if mv == 1 { 56 } else { 116 })?,
        12 => {
            let paths = f.items(f.reference_in(m, if mv == 1 { 36 } else { 48 })?, b"SCHR")?;
            if paths.n > 0 {
                out.diffuse = f.opt_string(paths.get(0), 0)?;
            }
        }
        _ => {}
    }
    Ok(out)
}

// ---- geometry --------------------------------------------------------------

struct Region {
    first_vertex: usize,
    vertex_count: usize,
    first_face: usize,
    face_count: usize,
    first_lookup: usize,
    lookup_count: usize,
    lookups_used: usize,
    /// Faces index the whole vertex buffer (REGN v2) instead of the region.
    absolute_faces: bool,
    hidden: bool,
    uv_multiply: f32,
    uv_offset: f32,
}

impl Region {
    fn parse(r: &[u8], v: u32) -> Self {
        let (first_vertex, vertex_count, o) = if v <= 2 {
            (u16_at(r, 4) as usize, u16_at(r, 6) as usize, 8)
        } else {
            (u32_at(r, 8) as usize, u32_at(r, 12) as usize, 16)
        };
        Region {
            first_vertex,
            vertex_count,
            first_face: u32_at(r, o) as usize,
            face_count: u32_at(r, o + 4) as usize,
            first_lookup: u16_at(r, o + 10) as usize,
            lookup_count: u16_at(r, o + 12) as usize,
            lookups_used: r[o + 16] as usize,
            absolute_faces: v <= 2,
            hidden: v >= 4 && u32_at(r, 36) & 1 != 0,
            uv_multiply: if v >= 5 { f32_at(r, 40) } else { 16.0 },
            uv_offset: if v >= 5 { f32_at(r, 44) } else { 0.0 },
        }
    }
}

/// Byte offsets of the vertex components, from `MODL.vertex_flags`.
struct VertexFormat {
    size: usize,
    /// Number of weight/lookup pairs (0, 2 or 4): weights then lookups, after the position.
    skin: usize,
    normal: Option<usize>,
    normal_f32: Option<usize>,
    color: Option<usize>,
    uv: Option<usize>,
    uv_f32: Option<usize>,
    tangent: Option<usize>,
}

impl VertexFormat {
    fn new(flags: u32) -> Result<Self> {
        ensure!(flags & 1 != 0, "vertex format {flags:#x} without position");
        ensure!(flags & 0x8000_0002 == 0, "unknown vertex flags {flags:#x}");
        let has = |m: u32| flags & m != 0;
        let mut o = 12;
        let skin = 2 * (has(0x20) as usize + has(0x40) as usize);
        o += skin * 2;
        let mut normal_f32 = None;
        let take = |o: &mut usize, mask: u32, len: usize| {
            has(mask).then(|| {
                *o += len;
                *o - len
            })
        };
        if let Some(p) = take(&mut o, 0x80, 12) {
            normal_f32 = Some(p);
        }
        let normal = take(&mut o, 0x80_0000, 4);
        take(&mut o, 0x100, 4);
        let color = take(&mut o, 0x200, 4);
        for m in [0x400, 0x800, 0x1000] {
            take(&mut o, m, 4);
        }
        let fuv: Vec<usize> = [0x2000, 0x4000, 0x8000, 0x1_0000].into_iter().filter_map(|m| take(&mut o, m, 8)).collect();
        let uv_sets = [0x2_0000, 0x4_0000, 0x8_0000, 0x10_0000, 0x4000_0000].into_iter().filter(|&m| has(m)).count();
        let uv = (uv_sets > 0).then_some(o);
        o += uv_sets * 4;
        if let Some(p) = take(&mut o, 0x20_0000, 12) {
            normal_f32 = normal_f32.or(Some(p));
        }
        take(&mut o, 0x40_0000, 12);
        let tangent = take(&mut o, 0x100_0000, 4);
        for (m, len) in [(0x200_0000, 4), (0x400_0000, 12), (0x800_0000, 12), (0x1000_0000, 4), (0x2000_0000, 4)] {
            take(&mut o, m, len);
        }
        Ok(Self { size: o, skin, normal, normal_f32, color, uv, uv_f32: fuv.first().copied(), tangent })
    }
}

fn unpack(b: u8) -> f32 {
    b as f32 / 255.0 * 2.0 - 1.0
}

fn build_mesh(fmt: &VertexFormat, verts: &[u8], faces: &[u16], lookup: &[u16], nbones: usize, r: &Region) -> Result<Mesh> {
    let vcount = verts.len() / fmt.size;
    ensure!(r.first_vertex + r.vertex_count <= vcount, "vertices {}+{} of {vcount}", r.first_vertex, r.vertex_count);
    ensure!(r.first_face + r.face_count <= faces.len(), "faces {}+{} of {}", r.first_face, r.face_count, faces.len());
    ensure!(r.face_count.is_multiple_of(3), "face count {} not a multiple of 3", r.face_count);
    ensure!(r.first_lookup + r.lookup_count <= lookup.len(), "bone lookup {}+{} of {}", r.first_lookup, r.lookup_count, lookup.len());
    let lut = &lookup[r.first_lookup..r.first_lookup + r.lookup_count];
    if let Some(&b) = lut.iter().find(|&&b| b as usize >= nbones) {
        bail!("bone lookup -> bone {b} of {nbones}");
    }

    let n = r.vertex_count;
    let mut m = Mesh { positions: Vec::with_capacity(n), ..Default::default() };
    let used = r.lookups_used.clamp(1, 4);
    let uv_scale = r.uv_multiply / 32768.0;
    for i in r.first_vertex..r.first_vertex + n {
        let v = &verts[i * fmt.size..(i + 1) * fmt.size];
        m.positions.push(vec3(v, 0));
        if let Some(o) = fmt.normal {
            m.normals.push([unpack(v[o]), unpack(v[o + 1]), unpack(v[o + 2])]);
        } else if let Some(o) = fmt.normal_f32 {
            m.normals.push(vec3(v, o));
        }
        if let Some(o) = fmt.tangent {
            let w = if fmt.normal.is_some_and(|n| v[n + 3] < 128) { -1.0 } else { 1.0 };
            m.tangents.push([unpack(v[o]), unpack(v[o + 1]), unpack(v[o + 2]), w]);
        }
        if let Some(o) = fmt.color {
            m.colors.push([v[o + 2], v[o + 1], v[o], v[o + 3]]);
        }
        if let Some(o) = fmt.uv {
            let c = |k| i16::from_le_bytes([v[o + k], v[o + k + 1]]) as f32 * uv_scale + r.uv_offset;
            m.uvs.push([c(0), c(2)]);
        } else if let Some(o) = fmt.uv_f32 {
            m.uvs.push([f32_at(v, o), f32_at(v, o + 4)]);
        }
        // Lookups are relative to the region's slice of MODL.bone_lookup.
        let bone = |l: usize| -> Result<u16> {
            lut.get(l).copied().with_context(|| format!("vertex {i}: lookup {l} of {}", lut.len()))
        };
        let (mut j, mut w) = ([0u16; 4], [0f32; 4]);
        if fmt.skin == 0 {
            if !lut.is_empty() {
                j[0] = bone(0)?;
            }
            w[0] = 1.0;
        } else {
            let mut sum = 0.0;
            for k in 0..fmt.skin.min(used) {
                let wk = v[12 + k] as f32 / 255.0;
                if wk > 0.0 {
                    j[k] = bone(v[12 + fmt.skin + k] as usize)?;
                    w[k] = wk;
                    sum += wk;
                }
            }
            if sum > 0.0 {
                w.iter_mut().for_each(|x| *x /= sum);
            } else {
                j[0] = lut.first().copied().unwrap_or(0);
                w[0] = 1.0;
            }
        }
        m.joints.push(j);
        m.weights.push(w);
    }

    let base = if r.absolute_faces { r.first_vertex } else { 0 };
    m.indices = faces[r.first_face..r.first_face + r.face_count]
        .iter()
        .map(|&f| (f as usize).checked_sub(base).filter(|&f| f < n).map(|f| f as u32))
        .collect::<Option<_>>()
        .with_context(|| format!("face index out of range for {n} vertices"))?;
    Ok(m)
}

// ---- primitives ------------------------------------------------------------

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn vec3(b: &[u8], o: usize) -> [f32; 3] {
    [f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8)]
}
