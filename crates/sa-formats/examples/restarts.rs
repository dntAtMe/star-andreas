//! Print main.scm's hospital and police restart points: restarts ["<game dir>"]
fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));
    let scm = std::fs::read(root.join("data/script/main.scm"))?;
    let (h, p) = sa_formats::population::scan_scm_restarts(&scm);
    println!("hospitals {h:?}");
    println!("police {p:?}");
    Ok(())
}
