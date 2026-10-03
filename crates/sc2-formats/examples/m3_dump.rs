//! Parses an `.m3` model from CASC, prints stats + validation, optionally writes OBJ.
//! `cargo run -p sc2-formats --example m3_dump --release -- [name substring | m3 path | --all [substring]] [--pose <seq> <t_ms>] [sc2 dir]`
//! `M3_OBJ=out.obj` writes the geometry (one `o` per region) as Wavefront OBJ, skinned
//! into the `--pose` sequence/time if given (bind pose otherwise).

use std::{collections::BTreeMap, fmt::Write as _};

use sc2_formats::{
    casc::{Storage, parse_key},
    dds, m3,
    root::Root,
};

const MARINE: &str = "mods/liberty.sc2mod/base.sc2assets/assets/units/terran/marine/marine.m3";

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let pose = args.iter().position(|a| a == "--pose").map(|i| {
        let v: Vec<String> = args.drain(i..(i + 3).min(args.len())).skip(1).collect();
        (v.first().cloned().unwrap_or_default(), v.get(1).and_then(|t| t.parse::<f32>().ok()).unwrap_or(0.0))
    });
    let all = args.first().is_some_and(|a| a == "--all");
    let arg = args.first().filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| MARINE.into());
    let dir = args.get(if all { 2 } else { 1 }).cloned().unwrap_or_else(|| r"G:\SC 2\StarCraft II".into());
    let st = Storage::open(dir.as_ref())?;
    let root = Root::parse(&st.read_ckey(&parse_key(st.config_value("root", 0)?)?)?)?;
    if all {
        return scan_all(&st, &root, args.get(1).cloned().unwrap_or_default().to_ascii_lowercase());
    }

    let name = resolve(&root, &arg).ok_or_else(|| anyhow::anyhow!("no .m3 matching {arg:?}"))?;
    let data = st.read_ckey(&root.get(&name).unwrap())?;
    let t = std::time::Instant::now();
    let model = m3::parse(&data)?;
    let dt = t.elapsed();
    println!("{name} ({} bytes, parsed in {dt:.2?})", data.len());

    let verts: usize = model.meshes.iter().map(|m| m.positions.len()).sum();
    let tris: usize = model.meshes.iter().map(|m| m.indices.len() / 3).sum();
    println!(
        "MODL v{}  vertex flags {:#x}  vertex size {}  verts {verts}  tris {tris}  regions {}  bones {}  materials {}",
        model.version,
        model.vertex_flags,
        model.vertex_size,
        model.meshes.len(),
        model.bones.len(),
        model.materials.len()
    );
    println!("bounds {:?} .. {:?}", model.bounds.0, model.bounds.1);
    for (i, m) in model.meshes.iter().enumerate() {
        println!(
            "  region {i}: {} verts, {} tris, material {:?}{}{}{}",
            m.positions.len(),
            m.indices.len() / 3,
            m.material,
            if m.hidden { ", hidden" } else { "" },
            if m.uvs.is_empty() { ", no uvs" } else { "" },
            if m.tangents.is_empty() { "" } else { ", tangents" },
        );
        if !m.uvs.is_empty() {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for t in &m.uvs {
                (0..2).for_each(|k| (lo[k], hi[k]) = (lo[k].min(t[k]), hi[k].max(t[k])));
            }
            println!("    uv range {lo:?} .. {hi:?}");
        }
    }

    let package = name.find(".sc2assets/").map(|i| &name[..i + 11]).unwrap_or("mods/liberty.sc2mod/base.sc2assets/");
    for (i, m) in model.materials.iter().enumerate() {
        println!("  material {i} {:?}: kind {} blend {} flags {:#x}", m.name, m.kind, m.blend_mode, m.flags);
        for (slot, tex) in [("diffuse", &m.diffuse), ("normal", &m.normal), ("specular", &m.specular), ("emissive", &m.emissive)] {
            if let Some(p) = tex {
                println!("    {slot:8} {p}  [{}]", texture_info(&st, &root, package, p));
            }
        }
    }

    validate(&model);
    animations(&model);
    if let Ok(path) = std::env::var("M3_OBJ") {
        let posed = match &pose {
            Some((seq, t)) => {
                let q = model.sequences.iter().find(|q| q.name.eq_ignore_ascii_case(seq));
                let q = q.ok_or_else(|| anyhow::anyhow!("no sequence {seq:?}"))?;
                println!("posing {:?} at {t} ms", q.name);
                Some(skin(&model, &world_from(&model, &pose_locals(&model, q, *t, false))))
            }
            None => None,
        };
        std::fs::write(&path, to_obj(&model, posed.as_deref()))?;
        println!("wrote {path}");
    }
    Ok(())
}

/// Sequence list, key sanity and posed bounds of Stand / mid-Walk / mid-Attack.
fn animations(m: &m3::Model) {
    println!("sequences ({}):", m.sequences.len());
    let (mut quat_err, mut unsorted, mut past_end, mut tracks) = (0f32, 0, 0, 0);
    for q in &m.sequences {
        let animated = q.bones.iter().filter(|b| !b.is_empty()).count();
        println!(
            "  {:24} {:>5}..{:<5} ms {:8} freq {:3} speed {:6.1} bones {animated:3}{}",
            format!("{:?}", q.name),
            q.start_ms,
            q.end_ms,
            if q.looping { "loop" } else { "once" },
            q.frequency,
            q.movement_speed,
            if q.layered { " layered" } else { "" },
        );
        for b in &q.bones {
            if let Some(t) = &b.rotation {
                quat_err = t.keys.iter().map(|k| (k.iter().map(|x| x * x).sum::<f32>().sqrt() - 1.0).abs()).fold(quat_err, f32::max);
            }
            let times = [b.translation.as_ref().map(|t| &t.times_ms), b.rotation.as_ref().map(|t| &t.times_ms), b.scale.as_ref().map(|t| &t.times_ms)];
            for t in times.into_iter().flatten() {
                tracks += 1;
                unsorted += t.windows(2).any(|w| w[1] < w[0]) as usize;
                past_end += (t.last().is_some_and(|&l| l > q.duration_ms()) || t[0] < 0) as usize;
            }
        }
    }
    println!("  {tracks} tracks: max |quat|-1 = {quat_err:.2e}, {unsorted} with unsorted times, {past_end} with keys outside 0..=duration");

    let r = |b: ([f32; 3], [f32; 3])| b.0.iter().chain(&b.1).map(|x| format!("{x:6.2}")).collect::<Vec<_>>().join(" ");
    let bounds = |p: &[Skinned]| {
        let mut bb = bbox();
        p.iter().zip(&m.meshes).filter(|(_, mesh)| !mesh.hidden).flat_map(|(p, _)| &p.0).for_each(|&v| grow(&mut bb, v));
        bb
    };
    let bind: Vec<_> = m.meshes.iter().map(|mesh| (mesh.positions.clone(), vec![])).collect();
    println!("  posed bounds (min xyz, max xyz):");
    println!("    {:34} {}", "bind", r(bounds(&bind)));
    let pick = |pre: &str| {
        let named = m.sequences.iter().filter(|q| !q.layered && q.name.to_ascii_lowercase().starts_with(pre));
        named.clone().find(|q| q.name.eq_ignore_ascii_case(pre)).or_else(|| named.clone().next())
    };
    for (pre, frac) in [("stand", 0.0), ("walk", 0.5), ("attack", 0.5)] {
        let Some(q) = pick(pre) else { continue };
        let t = q.duration_ms() as f32 * frac;
        for bind_fallback in [false, true] {
            let p = skin(m, &world_from(m, &pose_locals(m, q, t, bind_fallback)));
            let label = format!("{:?} @{t:.0}{}", q.name, if bind_fallback { " (untracked=bind)" } else { "" });
            println!("    {label:34} {}", r(bounds(&p)));
        }
    }
}

/// Local matrices at `t_ms`. Untracked bones use `rest_*` (or, for comparison, the
/// bind-relative transform `IREF(parent) * inverse(IREF(bone))`).
fn pose_locals(m: &m3::Model, q: &m3::Sequence, t_ms: f32, bind_fallback: bool) -> Vec<M4> {
    q.local_pose(&m.bones, t_ms)
        .into_iter()
        .enumerate()
        .map(|(i, (t, r, s))| {
            let b = &m.bones[i];
            if bind_fallback && q.bones[i].is_empty() {
                let bind = inverse_affine(&b.inverse_bind);
                b.parent.map_or(bind, |p| mul(&m.bones[p].inverse_bind, &bind))
            } else {
                trs(t, r, s)
            }
        })
        .collect()
}

/// Skinned positions and normals per mesh for the given bone world matrices.
fn skin(m: &m3::Model, world: &[M4]) -> Vec<Skinned> {
    let mats: Vec<M4> = world.iter().zip(&m.bones).map(|(w, b)| mul(w, &b.inverse_bind)).collect();
    let xf = |s: &M4, p: [f32; 3], w: f32| -> [f32; 3] { std::array::from_fn(|a| s[0][a] * p[0] + s[1][a] * p[1] + s[2][a] * p[2] + s[3][a] * w) };
    let blend = |j: &[u16; 4], w: &[f32; 4], p: [f32; 3], h: f32| {
        if mats.is_empty() {
            return p;
        }
        (0..4).filter(|&k| w[k] > 0.0).fold([0.0; 3], |acc, k| {
            let v = xf(&mats[j[k] as usize], p, h);
            [acc[0] + w[k] * v[0], acc[1] + w[k] * v[1], acc[2] + w[k] * v[2]]
        })
    };
    m.meshes
        .iter()
        .map(|mesh| {
            let pos = mesh.positions.iter().enumerate().map(|(i, &p)| blend(&mesh.joints[i], &mesh.weights[i], p, 1.0)).collect();
            let nrm = mesh
                .normals
                .iter()
                .enumerate()
                .map(|(i, &n)| {
                    let v = blend(&mesh.joints[i], &mesh.weights[i], n, 0.0);
                    let l = len(v).max(1e-9);
                    v.map(|x| x / l)
                })
                .collect();
            (pos, nrm)
        })
        .collect()
}

/// Exact path, else the `.m3` whose file name is `<arg>.m3` (preferring Liberty base), else a substring match.
fn resolve(root: &Root, arg: &str) -> Option<String> {
    if root.get(arg).is_some() {
        return Some(arg.to_string());
    }
    let a = arg.to_ascii_lowercase();
    let file = format!("/{a}.m3");
    let mut hits: Vec<&str> = root.names().map(|(n, _)| n).filter(|n| n.ends_with(".m3") && n.contains(&a)).collect();
    hits.sort_by_key(|n| (!n.ends_with(&file), !n.starts_with("mods/liberty.sc2mod/base"), n.len()));
    if hits.len() > 1 {
        println!("{} matches, using the first (next: {:?})", hits.len(), &hits[1..hits.len().min(4)]);
    }
    hits.first().map(|s| s.to_string())
}

fn texture_info(st: &Storage, root: &Root, package: &str, path: &str) -> String {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    let read = |pkg: &str, p: &str| root.get(&format!("{pkg}{p}")).and_then(|k| st.read_ckey(&k).ok());
    for pkg in [package, "mods/liberty.sc2mod/base.sc2assets/", "mods/core.sc2mod/base.sc2assets/"] {
        let (full, low) = (read(pkg, &p), read(pkg, &dds::lvl0_name(&p)));
        if full.is_none() && low.is_none() {
            continue;
        }
        let mut s = format!("{pkg}: .dds {} .lvl0 {}", size(&full), size(&low));
        match dds::best(full.as_deref(), low.as_deref()) {
            Ok(d) => {
                let _ = write!(s, " -> {}x{} {:?} {} mips", d.width, d.height, d.format, d.mip_count);
            }
            Err(e) => {
                let _ = write!(s, " -> {e:#}");
            }
        }
        return s;
    }
    "not found".into()
}

fn size(b: &Option<Vec<u8>>) -> String {
    b.as_ref().map_or("-".into(), |b| b.len().to_string())
}

fn validate(m: &m3::Model) {
    let mut issues = Vec::new();
    let (mut weight_bad, mut normal_bad, mut max_normal_err) = (0, 0, 0f32);
    let (mut front, mut back, mut degenerate) = (0, 0, 0);
    for (r, mesh) in m.meshes.iter().enumerate() {
        let n = mesh.positions.len();
        if let Some(i) = mesh.indices.iter().find(|&&i| i as usize >= n) {
            issues.push(format!("region {r}: index {i} >= {n}"));
        }
        for c in [mesh.normals.len(), mesh.uvs.len(), mesh.joints.len(), mesh.weights.len()] {
            if c != 0 && c != n {
                issues.push(format!("region {r}: attribute count {c} != {n}"));
            }
        }
        if mesh.uvs.iter().flatten().any(|x| !x.is_finite()) || mesh.positions.iter().flatten().any(|x| !x.is_finite()) {
            issues.push(format!("region {r}: non-finite positions/uvs"));
        }
        weight_bad += mesh.weights.iter().filter(|w| (w.iter().sum::<f32>() - 1.0).abs() > 1e-3).count();
        if let Some(j) = mesh.joints.iter().flatten().find(|&&j| j as usize >= m.bones.len().max(1)) {
            issues.push(format!("region {r}: joint {j} >= {} bones", m.bones.len()));
        }
        for nv in &mesh.normals {
            let e = (len(*nv) - 1.0).abs();
            max_normal_err = max_normal_err.max(e);
            normal_bad += (e > 0.02) as usize;
        }
        if !mesh.normals.is_empty() {
            for t in mesh.indices.as_chunks::<3>().0 {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| mesh.positions[i as usize]);
                let f = cross(sub(b, a), sub(c, a));
                let nv = [t[0], t[1], t[2]].map(|i| mesh.normals[i as usize]);
                let avg = [0, 1, 2].map(|k| nv[0][k] + nv[1][k] + nv[2][k]);
                let d = dot(f, avg);
                if len(f) < 1e-9 {
                    degenerate += 1;
                } else if d > 0.0 {
                    front += 1;
                } else {
                    back += 1;
                }
            }
        }
    }
    for (i, b) in m.bones.iter().enumerate() {
        // Parents must exist and chains must terminate.
        let mut p = b.parent;
        let mut steps = 0;
        while let Some(q) = p {
            if q >= m.bones.len() || steps > m.bones.len() {
                issues.push(format!("bone {i} {:?}: bad parent chain", b.name));
                break;
            }
            p = m.bones[q].parent;
            steps += 1;
        }
    }
    // Bind pose = inverse(IREF). The default TRS is a pose (often not the bind pose), but
    // bone offsets rarely change: compare rest translations with the bind-relative ones.
    let bind: Vec<M4> = m.bones.iter().map(|b| inverse_affine(&b.inverse_bind)).collect();
    let mut t_dev = (0f32, 0);
    for (i, b) in m.bones.iter().enumerate() {
        let rel = b.parent.map_or(bind[i], |p| mul(&m.bones[p].inverse_bind, &bind[i]));
        let d = len(sub([rel[3][0], rel[3][1], rel[3][2]], b.rest_translation));
        if d > t_dev.0 {
            t_dev = (d, i);
        }
    }
    // Skin with the default pose: world(default) * IREF, and compare bounds.
    let rest: Vec<M4> = m.bones.iter().map(|b| trs(b.rest_translation, b.rest_rotation, b.rest_scale)).collect();
    let posed = skin(m, &world_from(m, &rest));
    let (mut bind_bb, mut pose_bb) = (bbox(), bbox());
    for (mesh, p) in m.meshes.iter().zip(&posed).filter(|(m, _)| !m.hidden) {
        mesh.positions.iter().for_each(|&v| grow(&mut bind_bb, v));
        p.0.iter().for_each(|&v| grow(&mut pose_bb, v));
    }
    let r = |b: ([f32; 3], [f32; 3])| b.0.iter().chain(&b.1).map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(" ");

    println!("validation:");
    println!("  winding: {front} tris agree with vertex normals (CCW front), {back} disagree, {degenerate} degenerate");
    println!("  weights: {weight_bad} vertices not summing to 1");
    println!("  normals: {normal_bad} off unit length by >0.02 (max err {max_normal_err:.4})");
    if let Some(b) = m.bones.get(t_dev.1) {
        println!("  bones: max |rest translation - bind-relative translation| = {:.4} ({:?})", t_dev.0, b.name);
    }
    println!("  bounds: MODL {}
          bind {}
          default pose (skinned) {}", r(m.bounds), r(bind_bb), r(pose_bb));
    for i in &issues {
        println!("  ISSUE {i}");
    }
    if issues.is_empty() {
        println!("  indices/attributes/joints/parents OK");
    }
}

type M4 = [[f32; 4]; 4];
/// Per-mesh skinned positions and normals.
type Skinned = (Vec<[f32; 3]>, Vec<[f32; 3]>);

/// World matrices from per-bone local matrices (parents are not necessarily first).
fn world_from(m: &m3::Model, local: &[M4]) -> Vec<M4> {
    fn get(m: &m3::Model, local: &[M4], out: &mut Vec<Option<M4>>, i: usize, depth: usize) -> M4 {
        if let Some(w) = out[i] {
            return w;
        }
        let w = match m.bones[i].parent {
            Some(p) if depth < m.bones.len() => mul(&get(m, local, out, p, depth + 1), &local[i]),
            _ => local[i],
        };
        out[i] = Some(w);
        w
    }
    let mut out = vec![None; m.bones.len()];
    (0..m.bones.len()).map(|i| get(m, local, &mut out, i, 0)).collect()
}

fn bbox() -> ([f32; 3], [f32; 3]) {
    ([f32::MAX; 3], [f32::MIN; 3])
}

fn grow(b: &mut ([f32; 3], [f32; 3]), p: [f32; 3]) {
    for (k, &x) in p.iter().enumerate() {
        b.0[k] = b.0[k].min(x);
        b.1[k] = b.1[k].max(x);
    }
}

/// Inverse of an affine column-major matrix.
fn inverse_affine(m: &M4) -> M4 {
    let a = |r: usize, c: usize| m[c][r];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    let inv = |r: usize, c: usize| {
        let (r1, r2, c1, c2) = ((c + 1) % 3, (c + 2) % 3, (r + 1) % 3, (r + 2) % 3);
        (a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)) / det
    };
    let mut o: M4 = std::array::from_fn(|c| std::array::from_fn(|r| if c < 3 && r < 3 { inv(r, c) } else { 0.0 }));
    let t: [f32; 3] = std::array::from_fn(|r| -(0..3).map(|k| o[k][r] * m[3][k]).sum::<f32>());
    o[3] = [t[0], t[1], t[2], 1.0];
    o
}

fn trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> M4 {
    let [x, y, z, w] = q;
    let r = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w)],
        [2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w)],
        [2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    let c = |i: usize| [r[i][0] * s[i], r[i][1] * s[i], r[i][2] * s[i], 0.0];
    [c(0), c(1), c(2), [t[0], t[1], t[2], 1.0]]
}

fn mul(a: &M4, b: &M4) -> M4 {
    std::array::from_fn(|c| std::array::from_fn(|r| (0..4).map(|k| a[k][r] * b[c][k]).sum()))
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// OBJ (Z-up as stored; OBJ viewers assume Y-up, so expect the model lying on its back
/// unless the viewer has a Z-up option). V is flipped to OBJ's bottom-left convention.
fn to_obj(m: &m3::Model, posed: Option<&[Skinned]>) -> String {
    let mut s = String::from("# m3_dump\n");
    let mut base = 1;
    for (r, mesh) in m.meshes.iter().enumerate() {
        let _ = writeln!(s, "o region{r}{}", mesh.material.map(|i| format!("_{}", m.materials[i].name)).unwrap_or_default());
        let (pos, nrm) = posed.map_or((&mesh.positions, &mesh.normals), |p| (&p[r].0, &p[r].1));
        for p in pos {
            let _ = writeln!(s, "v {} {} {}", p[0], p[1], p[2]);
        }
        for n in nrm {
            let _ = writeln!(s, "vn {} {} {}", n[0], n[1], n[2]);
        }
        for t in &mesh.uvs {
            let _ = writeln!(s, "vt {} {}", t[0], 1.0 - t[1]);
        }
        let (hn, ht) = (!mesh.normals.is_empty(), !mesh.uvs.is_empty());
        for t in mesh.indices.as_chunks::<3>().0 {
            s.push('f');
            for &i in t {
                let i = i as usize + base;
                let _ = match (ht, hn) {
                    (true, true) => write!(s, " {i}/{i}/{i}"),
                    (true, false) => write!(s, " {i}/{i}"),
                    (false, true) => write!(s, " {i}//{i}"),
                    (false, false) => write!(s, " {i}"),
                };
            }
            s.push('\n');
        }
        base += mesh.positions.len();
    }
    s
}

/// Parses every `.m3` (optionally filtered) and reports version histograms and failures.
fn scan_all(st: &Storage, root: &Root, filter: String) -> anyhow::Result<()> {
    let names: Vec<(String, _)> =
        root.names().filter(|(n, _)| n.ends_with(".m3") && n.contains(&filter)).map(|(n, k)| (n.to_string(), k)).collect();
    let (mut versions, mut flags, mut errors) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::<String, Vec<String>>::new());
    let (mut ok, mut read_fail, mut unbatched, mut verts) = (0, 0, 0, 0usize);
    let (mut seqs, mut layered, mut offset, mut quat_err) = (0, 0, BTreeMap::<&str, usize>::new(), 0f32);
    let mut seq_versions = BTreeMap::new();
    let t = std::time::Instant::now();
    for (n, k) in &names {
        let Ok(d) = st.read_ckey(k) else {
            read_fail += 1;
            continue;
        };
        match m3::parse(&d) {
            Ok(m) => {
                ok += 1;
                *versions.entry(m.version).or_insert(0) += 1;
                *flags.entry(m.vertex_flags).or_insert(0) += 1;
                unbatched += m.meshes.iter().filter(|m| m.hidden).count();
                verts += m.meshes.iter().map(|m| m.positions.len()).sum::<usize>();
                *seq_versions.entry(m.sequences.len().min(1)).or_insert(0) += 1;
                for q in &m.sequences {
                    seqs += 1;
                    layered += q.layered as usize;
                    let times = q.bones.iter().flat_map(|b| {
                        [b.translation.as_ref().map(|t| &t.times_ms), b.rotation.as_ref().map(|t| &t.times_ms), b.scale.as_ref().map(|t| &t.times_ms)]
                    });
                    let (lo, hi) = times.flatten().fold((i32::MAX, i32::MIN), |(lo, hi), t| (lo.min(t[0]), hi.max(*t.last().unwrap())));
                    // Times are stored minus start: classify where they land.
                    if q.start_ms != 0 && lo <= hi {
                        let k = if lo >= 0 { "start!=0, keys within 0..(after rebasing)" } else if lo >= -q.start_ms { "start!=0, keys before start (stored relative?)" } else { "start!=0, other" };
                        *offset.entry(k).or_insert(0) += 1;
                    }
                    for b in &q.bones {
                        if let Some(t) = &b.rotation {
                            quat_err = t.keys.iter().map(|k| (k.iter().map(|x| x * x).sum::<f32>().sqrt() - 1.0).abs()).fold(quat_err, f32::max);
                        }
                    }
                }
            }
            Err(e) => {
                let key = format!("{e:#}");
                let key = key.split(|c: char| c.is_ascii_digit()).collect::<Vec<_>>().join("#");
                errors.entry(key).or_default().push(n.clone());
            }
        }
    }
    println!("{} .m3 files, {ok} parsed, {read_fail} unreadable, in {:.1?}", names.len(), t.elapsed());
    println!("MODL versions: {versions:?}");
    println!("vertex flags: {:?}", flags.iter().map(|(f, c)| format!("{f:#x}:{c}")).collect::<Vec<_>>());
    println!("hidden/unbatched regions: {unbatched}, total vertices {verts}");
    println!("models without/with sequences: {seq_versions:?}; {seqs} sequences, {layered} layered; max |quat|-1 {quat_err:.2e}; {offset:?}");
    for (e, files) in &errors {
        println!("{} x {e}\n    e.g. {:?}", files.len(), &files[..files.len().min(3)]);
    }
    Ok(())
}
