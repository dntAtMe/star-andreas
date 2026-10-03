//! Print a texture's alpha as a coarse grid: texalpha <txd> <name>
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let txd = sa_formats::txd::parse(&std::fs::read(&a[1])?)?;
    let t = txd.iter().find(|t| t.name.eq_ignore_ascii_case(&a[2])).expect("texture");
    println!("{} {}x{} {:?} alpha {}", t.name, t.width, t.height, t.format, t.has_alpha);
    let rgba = match t.format {
        sa_formats::txd::Format::Rgba8 => t.mips[0].clone(),
        f => sa_formats::txd::decode_dxt(f, &t.mips[0], t.width, t.height),
    };
    let step = (t.width / 16).max(1);
    for y in (0..t.height).step_by(step as usize) {
        let row: String = (0..t.width)
            .step_by(step as usize)
            .map(|x| {
                let i = ((y * t.width + x) * 4) as usize;
                let (c, al) = (rgba[i], rgba[i + 3]);
                format!("{:3}/{:3} ", c, al)
            })
            .collect();
        println!("{row}");
    }
    Ok(())
}
