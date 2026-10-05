//! `cargo run -p sa-formats --example gxt -- <american.gxt> [KEY...]`: table stats and lookups
//! (keys missing from MAIN are searched in every mission table).
fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("gxt path");
    let d = std::fs::read(&path).expect("read");
    let mut g = sa_formats::gxt::Gxt::parse(&d).expect("parse");
    println!("{} tables, MAIN {} keys", g.tables.len(), g.main.len());
    let tables: Vec<String> = g.tables.iter().map(|t| t.0.clone()).collect();
    for k in args {
        if g.lookup(&k).is_none() {
            for t in &tables {
                g.load_mission(&d, t);
                if g.lookup(&k).is_some() {
                    print!("[{t}] ");
                    break;
                }
            }
        }
        println!("{k} = {:?}", String::from_utf8_lossy(g.get(&k)));
    }
}
