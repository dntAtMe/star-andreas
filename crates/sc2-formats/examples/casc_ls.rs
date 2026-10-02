//! Lists file names from the MNDX root and dumps the start of a few known files.
//! `cargo run -p sc2-formats --example casc_ls --release -- [substring] [sc2 dir]`

use sc2_formats::{
    casc::{Storage, key_hex, parse_key},
    root::Root,
};

fn main() -> anyhow::Result<()> {
    let filter = std::env::args().nth(1).filter(|s| !s.is_empty()).map(|s| s.to_ascii_lowercase());
    let dir = std::env::args().nth(2).unwrap_or_else(|| r"G:\SC 2\StarCraft II".into());
    let st = Storage::open(dir.as_ref())?;
    let data = st.read_ckey(&parse_key(st.config_value("root", 0)?)?)?;
    let t = std::time::Instant::now();
    let root = Root::parse(&data)?;
    let parse_time = t.elapsed();
    let local = root.names().filter(|(_, k)| st.ekey_for(k).is_some()).count();
    println!("root: {} names ({local} in local encoding table), parsed in {parse_time:.2?}", root.len());

    if let Some(f) = &filter {
        let hits: Vec<_> = root.names().filter(|(n, _)| n.contains(f.as_str())).collect();
        for (n, k) in hits.iter().take(200) {
            println!("  {} {n}", key_hex(k));
        }
        println!("{} matching {f:?}", hits.len());
    }

    // Mixed case and backslashes on purpose: lookups are normalized.
    let model = r"Mods\Liberty.SC2Mod\Base.SC2Assets\Assets\Units\Terran\Marine\Marine.m3";
    let mut picks = vec!["mods/liberty.sc2mod/base.sc2data/GameData/UnitData.xml".to_string(), model.to_string()];
    // Texture paths referenced by the model (relative to the asset package).
    if let Some(m3) = root.get(model).and_then(|k| st.read_ckey(&k).ok()) {
        let text = String::from_utf8_lossy(&m3);
        picks.extend(
            text.split(|c: char| !c.is_ascii_graphic())
                .filter(|s| s.to_ascii_lowercase().ends_with(".dds"))
                .map(|s| format!(r"mods\liberty.sc2mod\base.sc2assets\{s}"))
                .take(3),
        );
    }
    for name in picks {
        let Some(ckey) = root.get(&name) else {
            println!("\n{name}: not in root");
            continue;
        };
        match st.read_ckey(&ckey) {
            Ok(bytes) => {
                let head = &bytes[..300.min(bytes.len())];
                println!("\n{name} ({} bytes, ckey {}):\n{}", bytes.len(), key_hex(&ckey), String::from_utf8_lossy(head).escape_debug());
            }
            Err(e) => println!("\n{name}: {e:#}"),
        }
    }
    Ok(())
}
