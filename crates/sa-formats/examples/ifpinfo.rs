//! List the IFP packages in anim.img (or one package's anims and bones): ifpinfo [package]
use sa_formats::{ifp, img::Img};
fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let img = Img::open(&root.join("anim/anim.img"))?;
    match std::env::args().nth(1) {
        None => {
            for e in img.entries() {
                let anims = ifp::parse(img.data(e))?;
                let names: Vec<_> = anims.iter().map(|a| a.name.as_str()).collect();
                println!("{} ({}): {}", e.name, anims.len(), names.join(" "));
            }
        }
        Some(p) => {
            let data = if p == "ped" { std::fs::read(root.join("anim/ped.ifp"))? } else { img.get(&format!("{p}.ifp")).unwrap().to_vec() };
            for a in ifp::parse(&data)? {
                let bones: Vec<_> = a.tracks.iter().map(|t| format!("{}:{}({})", t.bone_id, t.bone_name, t.keys.len())).collect();
                println!("{} {:.3}s: {}", a.name, a.duration, bones.join(" "));
            }
        }
    }
    Ok(())
}
