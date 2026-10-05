//! Print the BreakablePlugin data of a model: breakable <model> ["<game dir>"]
fn main() -> anyhow::Result<()> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "dyn_wine_break".into());
    let root = std::path::PathBuf::from(std::env::args().nth(2).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));
    let img = sa_formats::img::Img::open(&root.join("models/gta3.img"))?;
    let data = img.get(&format!("{name}.dff")).ok_or_else(|| anyhow::anyhow!("no {name}.dff"))?;
    let clump = sa_formats::dff::parse(data)?;
    for (gi, g) in clump.geometries.iter().enumerate() {
        let Some(b) = &g.breakable else {
            println!("geo {gi}: no breakable data");
            continue;
        };
        println!(
            "geo {gi}: rule {} verts {} tris {} mats {:?} masks {:?} matcols {:?}",
            b.position_rule,
            b.vertices.len(),
            b.triangles.len(),
            b.tex_names,
            b.mask_names,
            b.mat_colors
        );
        println!("  v0 {:?} uv0 {:?} c0 {:?} t0 {:?} m {:?}", b.vertices.first(), b.uvs.first(), b.colors.first(), b.triangles.first(), &b.tri_material[..b.tri_material.len().min(8)]);
    }
    Ok(())
}
