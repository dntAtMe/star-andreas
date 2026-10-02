//! Parse every DFF/TXD/binary IPL in the game's IMG archives and every
//! IDE/IPL in gta.dat, reporting failures.
//!
//! cargo run -p sa-formats --example validate --release -- "<game dir>"

use std::{collections::BTreeMap, path::PathBuf};

use sa_formats::{col, dat, dff, ide, ifp, img::Img, ipl, txd};

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));

    let mut fails: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut ok: BTreeMap<&str, usize> = BTreeMap::new();
    let mut tex_formats: BTreeMap<String, usize> = BTreeMap::new();
    let mut col_stats: BTreeMap<String, usize> = BTreeMap::new();

    for name in ["models/gta3.img", "models/gta_int.img"] {
        let img = Img::open(&root.join(name))?;
        println!("{name}: {} entries", img.entries().len());
        for e in img.entries() {
            let lower = e.name.to_ascii_lowercase();
            let data = img.data(e);
            let (kind, res) = if lower.ends_with(".dff") {
                ("dff", dff::parse(data).map(|_| ()))
            } else if lower.ends_with(".txd") {
                ("txd", txd::parse(data).map(|t| {
                    for t in t {
                        *tex_formats.entry(format!("{:?} alpha={}", t.format, t.has_alpha)).or_default() += 1;
                    }
                }))
            } else if lower.ends_with(".col") {
                ("col", col::index(data).and_then(|idx| {
                    for e in idx {
                        let m = col::parse_model(&data[e.offset..e.offset + e.size])?;
                        // Sanity: mesh vertices must lie (roughly) inside the bounds.
                        for v in &m.vertices {
                            for k in 0..3 {
                                if v[k] < m.min[k] - 2.0 || v[k] > m.max[k] + 2.0 {
                                    anyhow::bail!("{}: vertex {v:?} outside bounds {:?}..{:?}", m.name, m.min, m.max);
                                }
                            }
                        }
                        *col_stats.entry(format!("v{}", m.version)).or_default() += 1;
                    }
                    Ok(())
                }))
            } else if lower.ends_with(".ipl") {
                ("ipl", ipl::parse_binary(data).map(|_| ()))
            } else {
                continue;
            };
            match res {
                Ok(()) => *ok.entry(kind).or_default() += 1,
                Err(err) => fails.entry(kind).or_default().push(format!("{}: {err:#}", e.name)),
            }
        }
    }

    let gta_dat = std::fs::read_to_string(root.join("data/gta.dat"))?;
    let (mut objs, mut insts) = (0, 0);
    for entry in dat::parse(&gta_dat) {
        let (kind, path) = match &entry {
            dat::Entry::Ide(p) => ("ide", p),
            dat::Entry::Ipl(p) => ("text-ipl", p),
            dat::Entry::Img(_) => continue,
        };
        let full = root.join(path.replace('\\', "/"));
        let res = std::fs::read(&full).map_err(anyhow::Error::from).and_then(|b| {
            let text = String::from_utf8_lossy(&b);
            if kind == "ide" {
                objs += ide::parse(&text)?.objects.len();
            } else {
                insts += ipl::parse_text(&text)?.len();
            }
            Ok(())
        });
        match res {
            Ok(()) => *ok.entry(kind).or_default() += 1,
            Err(err) => fails.entry(kind).or_default().push(format!("{path}: {err:#}")),
        }
    }

    let anims = ifp::parse(&std::fs::read(root.join("anim/ped.ifp"))?)?;
    println!("ped.ifp: {} animations", anims.len());
    for want in ["idle_stance", "walk_civi", "run_civi", "sprint_civi", "walk_player", "run_player", "fall_fall"] {
        match anims.iter().find(|a| a.name.eq_ignore_ascii_case(want)) {
            Some(a) => println!("  {}: {} tracks, {:.2}s", a.name, a.tracks.len(), a.duration),
            None => println!("  {want}: MISSING"),
        }
    }
    let gta3 = Img::open(&root.join("models/gta3.img"))?;
    let fam1 = dff::parse(gta3.get("fam1.dff").unwrap())?;
    let skin = fam1.geometries.iter().find_map(|g| g.skin.as_ref()).expect("fam1 skin");
    let hroot = fam1.frames.iter().find_map(|f| f.hanim.as_ref().filter(|h| !h.nodes.is_empty())).expect("hanim root");
    println!("fam1: {} frames, skin bones {}, hanim nodes {}", fam1.frames.len(), skin.num_bones, hroot.nodes.len());
    println!("col models: {col_stats:?}");
    println!("ok: {ok:?}");
    println!("IDE objects: {objs}, text IPL instances: {insts}");
    println!("texture formats: {tex_formats:#?}");
    for (kind, list) in &fails {
        println!("FAILED {kind}: {}", list.len());
        for f in list.iter().take(10) {
            println!("  {f}");
        }
    }
    Ok(())
}
