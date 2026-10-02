//! List IMG entries matching a substring: ls <img> <pattern>
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let img = sa_formats::img::Img::open(std::path::Path::new(&a[1]))?;
    for e in img.entries() {
        if e.name.to_ascii_lowercase().contains(&a[2]) {
            println!("{} {}", e.name, e.size);
        }
    }
    Ok(())
}
