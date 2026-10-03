//! Parse models/effects.fxp and print a system's structure.
//!
//! cargo run -p sa-formats --example fxinfo -- [system name] ["<game dir>"]

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let want = std::env::args().nth(1);
    let root = PathBuf::from(std::env::args().nth(2).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));
    let text = std::fs::read_to_string(root.join("models/effects.fxp"))?;
    let p = sa_formats::fxp::parse(&text)?;
    println!("{} systems", p.systems.len());
    let txd = sa_formats::txd::parse(&std::fs::read(root.join("models/effectsPC.txd"))?)?;
    let mut missing: Vec<&str> = p
        .systems
        .iter()
        .flat_map(|s| &s.prims)
        .flat_map(|pr| pr.textures.iter().flatten())
        .filter(|t| !txd.iter().any(|x| x.name.eq_ignore_ascii_case(t)))
        .map(|t| t.as_str())
        .collect();
    missing.sort();
    missing.dedup();
    println!("effectsPC.txd: {} textures, missing from it: {missing:?}", txd.len());
    for s in &p.systems {
        if want.as_deref().is_some_and(|w| !s.name.eq_ignore_ascii_case(w)) {
            continue;
        }
        println!("{} len {} mode {} cull {} prims {}", s.name, s.length, s.play_mode, s.cull_dist, s.prims.len());
        if want.is_none() {
            continue;
        }
        for pr in &s.prims {
            println!("  {} tex {:?} blend {}/{} alpha {} lod {}..{}", pr.name, pr.textures, pr.src_blend, pr.dst_blend, pr.alpha_on, pr.lod_start, pr.lod_end);
            for i in &pr.infos {
                println!("    {} prt {:?}", i.kind, i.time_mode_prt);
                for (n, c) in &i.fields {
                    println!("      {n}: {}{:?}", if c.looped { "looped " } else { "" }, c.keys);
                }
            }
        }
    }
    Ok(())
}
