//! Dump the HAnim nodes / skin of player.dff and a player.img part: clothesinfo <part>
use sa_formats::{dff, img::Img};
fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let gta3 = Img::open(&root.join("models/gta3.img"))?;
    let pimg = Img::open(&root.join("models/player.img"))?;
    let part = std::env::args().nth(1).unwrap_or("vest".into());
    let show = |c: &dff::Clump, tag: &str| {
        for f in &c.frames {
            if let Some(h) = f.hanim.as_ref().filter(|h| !h.nodes.is_empty()) {
                println!("{tag}: hier at '{}' nodes {:?}", f.name, h.nodes.iter().map(|n| (n.0, n.1)).collect::<Vec<_>>());
            }
        }
        for a in &c.atomics {
            let g = &c.geometries[a.geometry as usize];
            if let Some(s) = &g.skin {
                let mut used = std::collections::BTreeSet::new();
                for (i, w) in s.indices.iter().zip(&s.weights) {
                    for k in 0..4 { if w[k] > 0.0 { used.insert(i[k]); } }
                }
                println!("{tag}: atomic frame '{}' verts {} bones {} used {:?}", c.frames[a.frame as usize].name, g.positions.len(), s.num_bones, used);
            }
        }
    };
    {
        let c = dff::parse_all(pimg.get(&format!("{part}.dff")).unwrap())?.remove(2);
        let v: Vec<String> = c.frames.iter().filter_map(|f| f.hanim.as_ref().map(|h| format!("{}={}", f.name.trim(), h.node_id))).enumerate().map(|(i, s)| format!("{i}:{s}")).collect();
        println!("{part} frame order: {}", v.join(" "));
    }
    for n in ["player.dff", "fam1.dff", "csplay.dff"] {
        let c = dff::parse(gta3.get(n).unwrap())?;
        let h = c.frames.iter().find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty())).unwrap();
        // RW HAnim: flag 2 PUSH (parent stays for the next sibling), flag 1 POP (last child).
        let mut stack: Vec<usize> = Vec::new();
        let mut parent = None;
        let mut out = Vec::new();
        for (i, n) in h.nodes.iter().enumerate() {
            out.push(format!("{}<{}", n.0, parent.map_or(-1, |p: usize| h.nodes[p].0)));
            if n.2 & 2 != 0 { if let Some(p) = parent { stack.push(p); } }
            if n.2 & 1 != 0 { parent = stack.pop(); } else { parent = Some(i); }
        }
        println!("{n} hanim parents: {}", out.join(" "));
        let v: Vec<String> = c.frames.iter().filter_map(|f| f.hanim.as_ref().map(|h| format!("{}={}", f.name.trim(), h.node_id))).collect();
        println!("{n}: {}", v.join(" "));
    }
    show(&dff::parse(gta3.get(&std::env::var("BASE").unwrap_or("player.dff".into())).unwrap())?, "base");
    for name in ["player.dff", "fam1.dff"] {
        let c = dff::parse(gta3.get(name).unwrap())?;
        let h = c.frames.iter().find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty())).unwrap();
        let s = c.geometries.iter().find_map(|g| g.skin.as_ref()).unwrap();
        for (k, n) in h.nodes.iter().enumerate().take(20) {
            let fi = c.frames.iter().position(|f| f.hanim.as_ref().is_some_and(|x| x.node_id == n.0)).unwrap();
            let (r, p) = c.frame_world(fi);
            let ib = s.inverse_bind[k];
            // world point of the bone origin mapped by invBind should be ~0
            let x = [ib[0]*p[0]+ib[4]*p[1]+ib[8]*p[2]+ib[12], ib[1]*p[0]+ib[5]*p[1]+ib[9]*p[2]+ib[13], ib[2]*p[0]+ib[6]*p[1]+ib[10]*p[2]+ib[14]];
            println!("{name} node {} '{}' world {:?} invbind*origin {:?} rotx {:?}", n.0, c.frames[fi].name.trim(), p.map(|v| (v*100.0).round()/100.0), x.map(|v| (v*100.0).round()/100.0), r[0].map(|v| (v*100.0).round()/100.0));
        }
    }
    {
        let pl = dff::parse(gta3.get("player.dff").unwrap())?;
        let ph: Vec<i32> = pl.frames.iter().find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty())).unwrap().nodes.iter().map(|n| n.0).collect();
        let ps = pl.geometries.iter().find_map(|g| g.skin.clone()).unwrap();
        let parts = dff::parse_all(pimg.get(&format!("{part}.dff")).unwrap())?;
        let c = &parts[2];
        let vh: Vec<i32> = c.frames.iter().find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty())).unwrap().nodes.iter().map(|n| n.0).collect();
        let vs = c.geometries.iter().find_map(|g| g.skin.clone()).unwrap();
        for id in [1, 31, 32, 33, 21, 22, 23] {
            let a = &ps.inverse_bind[ph.iter().position(|&x| x == id).unwrap()];
            let b = &vs.inverse_bind[vh.iter().position(|&x| x == id).unwrap()];
            let r = |m: &[f32; 16]| m.map(|v| (v * 100.0).round() / 100.0);
            println!("id {id}
  player {:?}
  {part}   {:?}", r(a), r(b));
        }
    }
    {
        let parts = dff::parse_all(pimg.get(&format!("{part}.dff")).unwrap())?;
        let c = &parts[2];
        let g = c.geometries.iter().find(|g| g.skin.is_some()).unwrap();
        let s = g.skin.as_ref().unwrap();
        let mut v: Vec<usize> = (0..g.positions.len()).filter(|&i| s.indices[i].iter().zip(s.weights[i]).any(|(&b, w)| (b == 11 || b == 12 || b == 10) && w > 0.3)).collect();
        v.sort_by(|&a, &b| g.positions[a][0].partial_cmp(&g.positions[b][0]).unwrap());
        for &i in v.iter().step_by(6) {
            println!("v{i} pos {:?} idx {:?} w {:?}", g.positions[i].map(|x| (x * 100.0).round() / 100.0), s.indices[i], s.weights[i].map(|x| (x * 100.0).round() / 100.0));
        }
    }
    for (k, c) in dff::parse_all(pimg.get(&format!("{part}.dff")).unwrap())?.iter().enumerate() {
        show(c, &format!("{part}[{k}]"));
    }
    Ok(())
}
