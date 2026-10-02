//! Compare a car's embedded COL extents with its wheel positions: carcol <model>
use sa_formats::{col, dff, img::Img};

fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let img = Img::open(&root.join("models/gta3.img"))?;
    for name in std::env::args().skip(1) {
        let c = dff::parse(img.get(&format!("{name}.dff")).unwrap())?;
        let m = col::parse_model(c.collision.as_deref().unwrap())?;
        let wz = c.frames.iter().find(|f| f.name == "wheel_lf_dummy").map(|f| f.pos[2]).unwrap_or(0.0);
        let sph_min = m.spheres.iter().map(|s| s.center[2] - s.radius).fold(f32::MAX, f32::min);
        let mesh_min = m.vertices.iter().map(|v| v[2]).fold(f32::MAX, f32::min);
        let mesh_max = m.vertices.iter().map(|v| v[2]).fold(f32::MIN, f32::max);
        println!(
            "{name}: bounds z {:.2}..{:.2} | wheel centre z {wz:.2} | sphere bottom {sph_min:.2} | mesh z {mesh_min:.2}..{mesh_max:.2} ({} verts)",
            m.min[2],
            m.max[2],
            m.vertices.len()
        );
        for s in m.spheres.iter().take(6) {
            println!("   sphere c {:?} r {:.2} mat {}", s.center, s.radius, s.surface.material);
        }
    }
    Ok(())
}
