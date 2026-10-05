//! `CClothesBuilder::CreateSkinnedClump` (clothes.md): CJ built from `models/player.img` parts on
//! the `player.dff` skeleton (or `csplay.dff` for the cutscene stand-in). Each part DFF holds the
//! `Normal` / `Fat` / `Ripped` variants, blended by the fat / muscle weights; the parts are merged
//! into one skinned geometry with one material per slot, and the slot textures are composited
//! from the skin bases and the clothes.

use std::collections::HashMap;

use anyhow::{Context, Result};
use bevy::prelude::*;
use sa_formats::{
    dff::{self, Atomic, Clump, Geometry, Material, Skin, Texture, Triangle},
    img::Img,
    txd,
};

use crate::stream::{TexCpu, convert_texture, make_image};

/// The new-game outfit (main.scm MAIN 0xE9BE..: four 087B, then REBUILD_PLAYER): model per
/// slot torso, head, hands, legs, feet.
const DEFAULT_MODELS: [&str; 5] = ["vest", "head", "hands", "jeans", "sneaker"];
/// The slot textures (`matTexName` 0x8D0A7C); hands use the torso texture.
const SLOT_TEX: [&str; 5] = ["torso", "head", "torso", "legs", "feet"];

/// `GetFatMuscleWeights` (0x5A42B0): (normal, fat, ripped). MAIN sets fat 200, muscle 50.
pub fn fat_muscle_weights(fat: f32, muscle: f32) -> (f32, f32, f32) {
    let mut f = ((fat - 200.0).max(0.0) * 0.00125).min(1.0);
    let mut m = (muscle.max(0.0) * 0.001).min(1.0);
    let mut n = 1.0 - (f + m);
    if f + m > 1.0 {
        let s = 1.0 / (f + m);
        f *= s;
        m *= s;
        n = 0.0;
    }
    (n, f, m)
}

/// The HAnim node ids of a clump's hierarchy (skin bone index → node id).
fn node_ids(c: &Clump) -> Vec<i32> {
    c.frames.iter().find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty())).map(|h| h.nodes.iter().map(|n| n.0).collect()).unwrap_or_default()
}

/// The skinned geometry of the clump variant whose atomic frame is named `name`.
fn variant<'a>(clumps: &'a [Clump], name: &str) -> Option<(&'a Clump, &'a Geometry)> {
    clumps.iter().find_map(|c| {
        c.atomics.iter().find_map(|a| {
            let f = c.frames.get(a.frame as usize)?;
            let g = c.geometries.get(a.geometry as usize)?;
            (f.name.trim().eq_ignore_ascii_case(name) && g.skin.is_some()).then_some((c, g))
        })
    })
}

/// One blended part (`BlendGeometry` 0x5A4940): positions, normals, uvs, bone weights merged per
/// bone, its triangles and the node id of each of its bone indices.
struct Part {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    idx: Vec<[u8; 4]>,
    w: Vec<[f32; 4]>,
    tris: Vec<[u16; 3]>,
    nodes: Vec<i32>,
}

fn blend_part(clumps: &[Clump], (n, f, m): (f32, f32, f32)) -> Option<Part> {
    let (nc, a) = variant(clumps, "normal")?;
    let b = variant(clumps, "fat").map(|v| v.1).filter(|g| g.positions.len() == a.positions.len());
    let c = variant(clumps, "ripped").map(|v| v.1).filter(|g| g.positions.len() == a.positions.len());
    let vars: Vec<(&Geometry, f32)> = [(Some(a), n), (b, f), (c, m)].into_iter().filter_map(|(g, w)| g.map(|g| (g, w))).collect();
    let nv = a.positions.len();
    let mut part = Part { pos: Vec::with_capacity(nv), nrm: Vec::with_capacity(nv), uv: Vec::with_capacity(nv), idx: Vec::new(), w: Vec::new(), tris: Vec::new(), nodes: node_ids(nc) };
    for v in 0..nv {
        let mut p = Vec3::ZERO;
        let mut nr = Vec3::ZERO;
        let mut uv = Vec2::ZERO;
        // AddWeightToBoneVertex: skip zero weights, add to an existing bone, else append.
        let mut bw: Vec<(u8, f32)> = Vec::with_capacity(8);
        for &(g, wt) in &vars {
            p += Vec3::from(g.positions[v]) * wt;
            nr += Vec3::from(g.normals.get(v).copied().unwrap_or([0.0, 0.0, 1.0])) * wt;
            uv += Vec2::from(g.uvs.first().and_then(|u| u.get(v)).copied().unwrap_or([0.0; 2])) * wt;
            let s = g.skin.as_ref().unwrap();
            for k in 0..4 {
                let x = wt * s.weights[v][k];
                if x == 0.0 {
                    continue;
                }
                let bone = s.indices[v][k];
                match bw.iter_mut().find(|e| e.0 == bone) {
                    Some(e) => e.1 += x,
                    None => bw.push((bone, x)),
                }
            }
        }
        let mut idx = [0u8; 4];
        let mut w = [0f32; 4];
        for (k, e) in bw.iter().take(4).enumerate() {
            idx[k] = e.0;
            w[k] = e.1;
        }
        if bw.len() > 4 {
            let s: f32 = w.iter().sum();
            if s > 0.0 {
                w = w.map(|x| x / s);
            }
        }
        part.pos.push(p.to_array());
        part.nrm.push(nr.normalize_or(Vec3::Z).to_array());
        part.uv.push(uv.to_array());
        part.idx.push(idx);
        part.w.push(w);
    }
    part.tris = a.triangles.iter().map(|t| t.v).collect();
    Some(part)
}

/// An RGBA8 texture (base level) from a TXD in player.img.
fn tex(img: &Img, txd_name: &str, tex_name: Option<&str>) -> Option<TexCpu> {
    let list = txd::parse(img.get(&format!("{txd_name}.txd"))?).ok()?;
    let t = match tex_name {
        Some(n) => list.into_iter().find(|t| t.name.eq_ignore_ascii_case(n))?,
        None => list.into_iter().next()?,
    };
    let mut c = convert_texture(t, false)?;
    let n = (c.width * c.height * 4) as usize;
    c.data.truncate(n);
    c.mip_count = 1;
    Some(c)
}

/// `BlendTextures3`: dst.rgb = trunc(dst·a + t1·b + t2·c) (low byte), alpha unchanged.
fn blend3(dst: &mut TexCpu, t1: Option<&TexCpu>, t2: Option<&TexCpu>, a: f32, b: f32, c: f32) {
    let n = dst.data.len();
    for p in (0..n).step_by(4) {
        for ch in 0..3 {
            let x = dst.data[p + ch] as f32 * a
                + t1.and_then(|t| t.data.get(p + ch)).copied().unwrap_or(0) as f32 * b
                + t2.and_then(|t| t.data.get(p + ch)).copied().unwrap_or(0) as f32 * c;
            dst.data[p + ch] = (x as i32) as u8;
        }
    }
}

/// `PlaceTextureOnTopOfTexture`: every source texel with alpha ≠ 0 replaces the destination.
fn stamp(dst: &mut TexCpu, src: &TexCpu) {
    let n = (src.width * src.height * 4) as usize;
    for p in (0..n.min(dst.data.len())).step_by(4) {
        if src.data[p + 3] != 0 {
            dst.data[p..p + 4].copy_from_slice(&src.data[p..p + 4]);
        }
    }
}

/// `ConstructTextures` for the default outfit: torso, legs, head, feet.
fn construct_textures(img: &Img, (n, f, m): (f32, f32, f32), images: &mut Assets<Image>) -> HashMap<String, (Handle<Image>, bool)> {
    let mut out = HashMap::new();
    let mut add = |name: &str, mut t: TexCpu| {
        t.name = name.into();
        t.alpha = false;
        out.insert(name.to_string(), (images.add(make_image(t)), false));
    };
    if let Some(mut t) = tex(img, "player_torso", Some("torso")) {
        blend3(&mut t, tex(img, "player_torso", Some("torso_fat")).as_ref(), tex(img, "player_torso", Some("torso_ripped")).as_ref(), n, f, m);
        if let Some(v) = tex(img, "vest", None) {
            stamp(&mut t, &v);
        }
        add("torso", t);
    }
    if let Some(mut t) = tex(img, "player_legs", Some("legs")) {
        blend3(&mut t, tex(img, "player_legs", Some("legs_fat")).as_ref(), tex(img, "player_legs", Some("legs_ripped")).as_ref(), n, f, m);
        if let Some(j) = tex(img, "jeansdenim", None) {
            stamp(&mut t, &j);
        }
        add("legs", t);
    }
    if let Some(t) = tex(img, "player_face", Some("face")).or_else(|| tex(img, "player_face", None)) {
        add("head", t);
    }
    if let Some(t) = tex(img, "sneakerbincblk", None) {
        add("feet", t);
    }
    out
}

/// CJ's clump and textures: `player.dff` (or `csplay.dff` with `cs_head` / `cs_hands`, the
/// CUTS rules of clothes.dat) skeleton and inverse bone matrices, the blended parts merged
/// in slot order with remapped bone indices.
pub fn build_cj(
    world: &crate::world::World,
    player_img: &Img,
    images: &mut Assets<Image>,
    cutscene: bool,
) -> Result<(Clump, HashMap<String, (Handle<Image>, bool)>)> {
    let base_name = if cutscene { "csplay" } else { "player" };
    let mut base = dff::parse(world.file(&format!("{base_name}.dff")).with_context(|| format!("{base_name}.dff"))?)?;
    let base_nodes = node_ids(&base);
    let base_skin = base.geometries.iter().find_map(|g| g.skin.clone()).context("base model has no skin")?;
    let root_frame = base.frames.iter().position(|f| f.hanim.as_ref().is_some_and(|h| !h.nodes.is_empty())).context("no HAnim hierarchy")?;
    let weights = fat_muscle_weights(200.0, 50.0);

    let mut g = Geometry::default();
    g.materials = ["torso", "head", "legs", "feet"]
        .iter()
        .map(|t| Material { color: [255; 4], texture: Some(Texture { name: (*t).into(), mask: String::new(), flags: 0 }) })
        .collect();
    let mut uv = Vec::new();
    let mut indices = Vec::new();
    let mut ws = Vec::new();
    for (slot, model) in DEFAULT_MODELS.iter().enumerate() {
        let model = match (cutscene, *model) {
            (true, "head") => "cs_head",
            (true, "hands") => "cs_hands",
            (_, m) => m,
        };
        let data = player_img.get(&format!("{model}.dff")).with_context(|| format!("player.img {model}.dff"))?;
        let clumps = dff::parse_all(data)?;
        let part = blend_part(&clumps, weights).with_context(|| format!("{model}: no normal variant"))?;
        // Part bone index → node id → index in the base hierarchy (missing → 0).
        let remap: Vec<u8> = part.nodes.iter().map(|id| base_nodes.iter().position(|b| b == id).unwrap_or(0) as u8).collect();
        let off = g.positions.len() as u16;
        let mat = ["torso", "head", "legs", "feet"].iter().position(|t| *t == SLOT_TEX[slot]).unwrap() as u16;
        g.positions.extend(&part.pos);
        g.normals.extend(&part.nrm);
        uv.extend(&part.uv);
        indices.extend(part.idx.iter().map(|ix| ix.map(|i| remap.get(i as usize).copied().unwrap_or(0))));
        ws.extend(&part.w);
        g.triangles.extend(part.tris.iter().map(|t| Triangle { v: t.map(|i| i + off), material: mat }));
    }
    g.uvs = vec![uv];
    g.skin = Some(Skin { num_bones: base_skin.num_bones, indices, weights: ws, inverse_bind: base_skin.inverse_bind.clone(), ..base_skin });
    base.geometries = vec![g];
    base.atomics = vec![Atomic { frame: root_frame as u32, geometry: 0, flags: 0 }];
    let textures = construct_textures(player_img, weights, images);
    Ok((base, textures))
}

/// `CPed::ShoulderBoneRotation` (0x5DF560, breast_bones.md), run for the player model (0) and
/// csplay after the anim: each shoulder pad (302 / 301) takes the upper arm's (32 / 22)
/// model matrix with the X angle of its rotation relative to the clavicle (31 / 21), split as
/// Rz·Ry·Rx, halved. `world(tag)` gives a bone's model-space matrix; returns (pad tag, its new
/// model-space matrix).
pub fn shoulder_pads(world: impl Fn(i32) -> Option<Mat4>) -> Vec<(i32, Mat4)> {
    let mut out = Vec::new();
    for (clav, arm, pad) in [(31, 32, 302), (21, 22, 301)] {
        let (Some(c), Some(a)) = (world(clav), world(arm)) else { continue };
        let rel = c.inverse() * a;
        let (_, q, t) = rel.to_scale_rotation_translation();
        let (z, y, x) = q.to_euler(EulerRot::ZYX);
        let q2 = Quat::from_euler(EulerRot::ZYX, z, y, x * 0.5);
        out.push((pad, c * Mat4::from_rotation_translation(q2, t)));
    }
    out
}
