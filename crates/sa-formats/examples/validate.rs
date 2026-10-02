//! Parse every DFF/TXD/binary IPL in the game's IMG archives and every
//! IDE/IPL in gta.dat, reporting failures.
//!
//! cargo run -p sa-formats --example validate --release -- "<game dir>"

use std::{collections::BTreeMap, path::PathBuf};

use sa_formats::{dat, dff, ide, img::Img, ipl, txd};

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));

    let mut fails: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut ok: BTreeMap<&str, usize> = BTreeMap::new();
    let mut tex_formats: BTreeMap<String, usize> = BTreeMap::new();

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
