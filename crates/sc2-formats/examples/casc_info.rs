//! Opens the CASC storage and dumps what the root file looks like.
//! `cargo run -p sc2-formats --example casc_info --release -- "<sc2 dir>"`

use sc2_formats::casc::{Storage, parse_key};

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| r"G:\SC 2\StarCraft II".into());
    let t = std::time::Instant::now();
    let st = Storage::open(dir.as_ref())?;
    println!("index {} entries, encoding {} entries, {:.2?}", st.index_len(), st.encoding_len(), t.elapsed());

    let root = st.read_ckey(&parse_key(st.config_value("root", 0)?)?)?;
    println!("root: {} bytes, magic {:?}", root.len(), String::from_utf8_lossy(&root[..4]));
    for row in root[..256.min(root.len())].chunks(32) {
        println!("  {}", row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    }
    Ok(())
}
