//! Print the 2dfx lights of a model: lights2d <model> ["<game dir>"]
fn main() -> anyhow::Result<()> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "lamppost1".into());
    let root = std::path::PathBuf::from(std::env::args().nth(2).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));
    let img = sa_formats::img::Img::open(&root.join("models/gta3.img"))?;
    let data = img.get(&format!("{name}.dff")).ok_or_else(|| anyhow::anyhow!("no {name}.dff"))?;
    let clump = sa_formats::dff::parse(data)?;
    for (gi, g) in clump.geometries.iter().enumerate() {
        for l in &g.lights {
            println!("geo {gi}: {l:?}");
        }
    }
    Ok(())
}
