//! Print frames/atomics/top-level extension chunks of a DFF in gta3.img: dffinfo <name>
use sa_formats::{bin::Reader, dff, img::Img, rw};
fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let img = Img::open(&root.join(std::env::var("SA_IMG").unwrap_or("models/gta3.img".into())))?;
    let name = std::env::args().nth(1).unwrap();
    let data = img.get(&format!("{name}.dff")).unwrap();
    let c = dff::parse(data)?;
    for (i, f) in c.frames.iter().enumerate() {
        println!("frame {i:2} parent {:2} {:20} pos {:?}", f.parent, f.name, f.pos);
    }
    for a in &c.atomics {
        let g = &c.geometries[a.geometry as usize];
        let mats: Vec<_> = g.materials.iter().map(|m| (m.color, m.texture.as_ref().map(|t| t.name.clone()))).collect();
        println!("atomic frame {} ({}) geo {} verts {} mats {:?}", a.frame, c.frames[a.frame as usize].name, a.geometry, g.positions.len(), mats);
    }
    // list chunk types inside the clump
    let mut r = Reader::new(data);
    let h = rw::header(&mut r)?;
    for ch in rw::children(Reader::new(r.bytes(h.size)?)) {
        let (h, body) = ch?;
        print!("clump child {:#x} size {}", h.ty, h.size);
        if h.ty == 3 { for e in rw::children(body) { let (eh, _) = e?; print!(" [ext {:#x} {}]", eh.ty, eh.size); } }
        println!();
    }
    Ok(())
}
