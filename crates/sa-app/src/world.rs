//! Static world database: object definitions and map placements from
//! gta.dat, built once at startup and shared with loader threads.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use bevy::prelude::*;
use sa_formats::{col, dat, ide, img::Img, ipl, objdat};

/// GTA is Z-up, Bevy is Y-up: (x, y, z) -> (x, z, -y). A proper rotation,
/// so winding and handedness are preserved.
pub fn g2b(v: [f32; 3]) -> Vec3 {
    Vec3::new(v[0], v[2], -v[1])
}

pub fn b2g(v: Vec3) -> [f32; 3] {
    [v.x, -v.z, v.y]
}

/// IPL quaternions are stored inverted; conjugate, then change basis.
fn ipl_rot(q: [f32; 4]) -> Quat {
    let [x, y, z, w] = q;
    Quat::from_xyzw(-x, -z, y, w).normalize()
}

pub struct ObjectInfo {
    pub model: String,
    pub txd: String,
    pub draw_distance: f32,
}

pub struct Instance {
    pub id: u32,
    pub pos: Vec3,
    pub rot: Quat,
    /// Visible when the camera distance is in [near, far).
    pub near: f32,
    pub far: f32,
}

pub struct World {
    pub imgs: Vec<Img>,
    pub objects: HashMap<u32, ObjectInfo>,
    pub txd_parent: HashMap<String, String>,
    pub instances: Vec<Instance>,
    /// Collision model name -> (img index, absolute offset, size).
    pub cols: HashMap<String, (usize, usize, usize)>,
    /// object.dat physics for movable / breakable props, by model name.
    pub physics: HashMap<String, objdat::ObjectPhysics>,
}

#[derive(Resource, Clone)]
pub struct WorldRes(pub Arc<World>);

impl World {
    /// Look up a file across all loaded IMG archives.
    pub fn file(&self, name: &str) -> Option<&[u8]> {
        self.imgs.iter().find_map(|img| img.get(name))
    }

    pub fn col(&self, model: &str) -> Option<&[u8]> {
        let &(img, offset, size) = self.cols.get(model)?;
        Some(self.imgs[img].slice(offset, size))
    }

    pub fn load(root: &Path) -> Result<Self> {
        let imgs = ["models/gta3.img", "models/gta_int.img"]
            .iter()
            .map(|p| Img::open(&root.join(p)))
            .collect::<Result<Vec<_>>>()?;

        let gta_dat = std::fs::read_to_string(root.join("data/gta.dat")).context("gta.dat")?;
        let entries = dat::parse(&gta_dat);
        let path = |p: &str| -> PathBuf { root.join(p.replace('\\', "/")) };

        let mut objects = HashMap::new();
        let mut timed = HashMap::new();
        let mut txd_parent = HashMap::new();
        for e in &entries {
            if let dat::Entry::Ide(p) = e {
                let text = std::fs::read(path(p)).with_context(|| p.clone())?;
                let ide = ide::parse(&String::from_utf8_lossy(&text))?;
                for o in ide.objects {
                    if let Some(t) = o.time {
                        timed.insert(o.id, t);
                    }
                    objects.insert(
                        o.id,
                        ObjectInfo { model: o.model, txd: o.txd, draw_distance: o.draw_distance.max(30.0) },
                    );
                }
                txd_parent.extend(ide.txd_parents);
            }
        }

        // Raw placements; LOD indices made global per IPL group.
        struct Raw {
            inst: ipl::Instance,
            lod: Option<usize>,
        }
        let mut raw: Vec<Raw> = Vec::new();
        for e in &entries {
            let dat::Entry::Ipl(p) = e else { continue };
            let text = std::fs::read(path(p)).with_context(|| p.clone())?;
            let base = raw.len();
            let globalize = |lod: i32| (lod >= 0).then(|| base + lod as usize);
            for inst in ipl::parse_text(&String::from_utf8_lossy(&text))? {
                let lod = globalize(inst.lod);
                raw.push(Raw { inst, lod });
            }
            // Binary stream IPLs in gta3.img extend the text IPL of the same name.
            let stem = Path::new(&p.replace('\\', "/"))
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            for n in 0.. {
                let Some(data) = imgs[0].get(&format!("{stem}_stream{n}.ipl")) else { break };
                for inst in ipl::parse_binary(data)? {
                    let lod = globalize(inst.lod);
                    raw.push(Raw { inst, lod });
                }
            }
        }

        // An instance referenced as a LOD becomes visible where its HD fades out.
        let mut lod_near = vec![0.0f32; raw.len()];
        for r in &raw {
            if let (Some(l), Some(obj)) = (r.lod, objects.get(&r.inst.id)) {
                if l < lod_near.len() {
                    lod_near[l] = lod_near[l].max(obj.draw_distance);
                }
            }
        }

        let noon = |id: u32| match timed.get(&id) {
            Some(&(on, off)) if on < off => on <= 12 && 12 < off,
            Some(&(on, off)) => 12 >= on || 12 < off,
            None => true,
        };
        let instances = raw
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let obj = objects.get(&r.inst.id)?;
                // Interiors live in the sky; only the outside world (0) and "everywhere" (13).
                let interior = r.inst.interior & 0xFF;
                if !(interior == 0 || interior == 13) || !noon(r.inst.id) {
                    return None;
                }
                let far = obj.draw_distance;
                let near = if lod_near[i] < far { lod_near[i] } else { 0.0 };
                Some(Instance { id: r.inst.id, pos: g2b(r.inst.pos), rot: ipl_rot(r.inst.rot), near, far })
            })
            .collect();

        let mut cols = HashMap::new();
        for (i, img) in imgs.iter().enumerate() {
            for e in img.entries().iter().filter(|e| e.name.to_ascii_lowercase().ends_with(".col")) {
                for c in col::index(img.data(e)).with_context(|| e.name.clone())? {
                    cols.insert(c.name.to_ascii_lowercase(), (i, e.offset + c.offset, c.size));
                }
            }
        }

        let physics = objdat::parse(&String::from_utf8_lossy(&std::fs::read(root.join("data/object.dat")).context("object.dat")?));

        Ok(Self { imgs, objects, txd_parent, instances, cols, physics })
    }
}
