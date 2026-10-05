//! Prints population data statistics (checks the parsers against the shipped files).
use sa_formats::population::*;

fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("game dir"));
    let read = |p: &str| std::fs::read(root.join(p)).unwrap();
    let (mut peds, mut hist) = (0usize, [0usize; 4]);
    for i in 0..64 {
        let a = parse_nodes(&read(&format!("data/Paths/NODES{i}.DAT"))).expect("nodes");
        peds += a.nodes.len() - a.num_veh_nodes;
        for n in &a.nodes[a.num_veh_nodes..] {
            for k in 0..n.num_links() {
                let x = a.intersections[n.base_link as usize + k];
                hist[(x & 3) as usize] += 1;
            }
        }
    }
    println!("ped nodes {peds}, ped link intersections {hist:?}");
    let s = scan_scm_zone_settings(&read("data/script/main.scm"));
    println!("scm: popType {} race {} gang {}", s.pop_type.len(), s.race.len(), s.gang.len());
    println!("GAN1 {:?} {:?} {:?}", s.pop_type.get("GAN1"), s.race.get("GAN1"), s.gang.get("GAN1"));
    let z = parse_zones(&String::from_utf8_lossy(&read("data/info.zon")));
    println!("zones {}", z.len());
    let g = parse_pedgrp(&String::from_utf8_lossy(&read("data/pedgrp.dat")));
    println!("ped groups {}", g.len());
    let st = parse_pedstats(&String::from_utf8_lossy(&read("data/pedstats.dat")));
    println!("pedstats {} [38]={}", st.len(), st[38].name);
    let pd = parse_peds_ide(&String::from_utf8_lossy(&read("data/peds.ide")));
    println!("peds {}", pd.len());
}
