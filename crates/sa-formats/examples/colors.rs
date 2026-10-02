//! Print prelit / extra vertex colour averages for the given models.
use sa_formats::{dff, img::Img};

fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let img = Img::open(&root.join("models/gta3.img"))?;
    let avg = |c: &[[u8; 4]]| {
        let n = c.len().max(1) as f32;
        let s = c.iter().fold([0f32; 4], |a, p| [a[0] + p[0] as f32, a[1] + p[1] as f32, a[2] + p[2] as f32, a[3] + p[3] as f32]);
        s.map(|x| (x / n) as u32)
    };
    for name in std::env::args().skip(1) {
        let Some(data) = img.get(&format!("{name}.dff")) else { println!("{name}: not found"); continue };
        let c = dff::parse(data)?;
        for g in &c.geometries {
            println!("{name}: flags {:#x} verts {} prelit {:?} extra {:?} normals {}", g.flags, g.positions.len(), avg(&g.prelit), avg(&g.extra_colors), g.normals.len());
        }
    }
    Ok(())
}
