//! `cargo run -p sa-formats --example gxt -- <american.gxt> [KEY...]`: table stats and lookups.
fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("gxt path");
    let d = std::fs::read(&path).expect("read");
    let g = sa_formats::gxt::Gxt::parse(&d).expect("parse");
    println!("{} tables, MAIN {} keys", g.tables.len(), g.main.len());
    for k in args {
        println!("{k} = {:?}", String::from_utf8_lossy(g.get(&k)));
    }
}
