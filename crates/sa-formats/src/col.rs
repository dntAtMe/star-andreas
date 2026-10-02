//! COL collision archives. A `.col` file is a sequence of models, each with
//! its own `COLL`/`COL2`/`COL3`/`COL4` header. Coordinates are GTA Z-up.

use anyhow::{Result, bail};

use crate::bin::{Reader, cstr};

#[derive(Debug, Clone, Copy)]
pub struct Surface {
    pub material: u8,
    pub flags: u8,
    pub brightness: u8,
    pub light: u8,
}

#[derive(Debug, Clone)]
pub struct Sphere {
    pub center: [f32; 3],
    pub radius: f32,
    pub surface: Surface,
}

#[derive(Debug, Clone)]
pub struct Bx {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub surface: Surface,
}

#[derive(Debug, Clone, Copy)]
pub struct Face {
    pub v: [u32; 3],
    pub material: u8,
}

#[derive(Debug, Clone, Default)]
pub struct ColModel {
    pub name: String,
    pub model_id: u16,
    pub version: u8,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub spheres: Vec<Sphere>,
    pub boxes: Vec<Bx>,
    pub vertices: Vec<[f32; 3]>,
    pub faces: Vec<Face>,
}

/// Location of one model inside a COL file, for lazy loading.
#[derive(Debug, Clone)]
pub struct ColEntry {
    pub name: String,
    pub offset: usize,
    pub size: usize,
}

/// Enumerate models in a COL file without parsing their bodies.
pub fn index(data: &[u8]) -> Result<Vec<ColEntry>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 32 <= data.len() {
        let fourcc = &data[pos..pos + 4];
        if !matches!(fourcc, b"COLL" | b"COL2" | b"COL3" | b"COL4") {
            break; // trailing padding
        }
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let total = 8 + size;
        if pos + total > data.len() {
            bail!("COL model at {pos} overruns file");
        }
        out.push(ColEntry { name: cstr(&data[pos + 8..pos + 30]), offset: pos, size: total });
        pos += total;
    }
    Ok(out)
}

fn surface(r: &mut Reader) -> Result<Surface> {
    Ok(Surface { material: r.u8()?, flags: r.u8()?, brightness: r.u8()?, light: r.u8()? })
}

/// Parse one model; `data` starts at its fourcc.
pub fn parse_model(data: &[u8]) -> Result<ColModel> {
    let mut r = Reader::new(data);
    let version = match r.bytes(4)? {
        b"COLL" => 1,
        b"COL2" => 2,
        b"COL3" => 3,
        b"COL4" => 4,
        f => bail!("bad COL fourcc {f:?}"),
    };
    let _size = r.u32()?;
    let name = r.fixed_str(22)?;
    let model_id = r.u16()?;
    let mut m = ColModel { name, model_id, version, ..Default::default() };

    if version == 1 {
        r.f32()?; // radius
        r.vec3()?; // center
        m.min = r.vec3()?;
        m.max = r.vec3()?;
        let n = r.u32()? as usize;
        for _ in 0..n {
            let radius = r.f32()?;
            let center = r.vec3()?;
            m.spheres.push(Sphere { center, radius, surface: surface(&mut r)? });
        }
        let unk = r.u32()? as usize;
        r.skip(unk * 4)?;
        let n = r.u32()? as usize;
        for _ in 0..n {
            let (min, max) = (r.vec3()?, r.vec3()?);
            m.boxes.push(Bx { min, max, surface: surface(&mut r)? });
        }
        let n = r.u32()? as usize;
        m.vertices = (0..n).map(|_| r.vec3()).collect::<Result<_>>()?;
        let n = r.u32()? as usize;
        for _ in 0..n {
            let v = [r.u32()?, r.u32()?, r.u32()?];
            let s = surface(&mut r)?;
            m.faces.push(Face { v, material: s.material });
        }
        return Ok(m);
    }

    m.min = r.vec3()?;
    m.max = r.vec3()?;
    r.vec3()?; // center
    r.f32()?; // radius
    let num_spheres = r.u16()? as usize;
    let num_boxes = r.u16()? as usize;
    let num_faces = r.u16()? as usize;
    r.u16()?; // u8 line count + padding
    r.u32()?; // flags
    let off_spheres = r.u32()? as usize;
    let off_boxes = r.u32()? as usize;
    r.u32()?; // lines
    let off_verts = r.u32()? as usize;
    let off_faces = r.u32()? as usize;

    // Offsets are relative to the size field (fourcc + 4).
    let at = |r: &mut Reader, off: usize| r.seek(off + 4);

    if num_spheres > 0 {
        at(&mut r, off_spheres)?;
        for _ in 0..num_spheres {
            let center = r.vec3()?;
            let radius = r.f32()?;
            m.spheres.push(Sphere { center, radius, surface: surface(&mut r)? });
        }
    }
    if num_boxes > 0 {
        at(&mut r, off_boxes)?;
        for _ in 0..num_boxes {
            let (min, max) = (r.vec3()?, r.vec3()?);
            m.boxes.push(Bx { min, max, surface: surface(&mut r)? });
        }
    }
    if num_faces > 0 {
        at(&mut r, off_faces)?;
        let mut max_index = 0;
        for _ in 0..num_faces {
            let v = [r.u16()? as u32, r.u16()? as u32, r.u16()? as u32];
            let material = r.u8()?;
            r.u8()?; // light
            max_index = max_index.max(v[0]).max(v[1]).max(v[2]);
            m.faces.push(Face { v, material });
        }
        // Vertex count isn't stored; it follows from the faces.
        at(&mut r, off_verts)?;
        m.vertices = (0..=max_index)
            .map(|_| Ok([r.u16()? as i16 as f32 / 128.0, r.u16()? as i16 as f32 / 128.0, r.u16()? as i16 as f32 / 128.0]))
            .collect::<Result<_>>()?;
    }
    Ok(m)
}
