//! Dump a cutscene from anim/cuts.img: `cargo run -p sa-formats --example cutinfo -- <game dir> prolog3`
use std::path::Path;

use sa_formats::{cutscene, ifp, img::Img};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let game = Path::new(&args[1]);
    let name = args.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_else(|| "prolog3".into());
    let img = Img::open(&game.join("anim").join("cuts.img"))?;
    let cut = cutscene::parse_cut(&String::from_utf8_lossy(img.get(&format!("{name}.cut")).unwrap()));
    println!("offset {:?} extracol {:?}", cut.offset, cut.extra_colour);
    for m in &cut.models {
        println!("model {} {:?}", m.model, m.anims);
    }
    for t in cut.texts.iter().take(5) {
        println!("text {} {} {}", t.start_ms, t.duration_ms, t.key);
    }
    println!("{} texts, uncompress {:?}, attach {:?}", cut.texts.len(), cut.uncompress, cut.attach);
    let sp = cutscene::parse_dat(&String::from_utf8_lossy(img.get(&format!("{name}.dat")).unwrap()));
    println!("splines fov {} roll {} pos {} target {} (lens {} {} {} {})", sp.fov[0], sp.roll[0], sp.pos[0], sp.target[0], sp.fov.len(), sp.roll.len(), sp.pos.len(), sp.target.len());
    println!("finish {} ms", cutscene::Flyby::finish_ms(&sp));
    let mut fb = cutscene::Flyby::default();
    let mut t = 0.0;
    while t < cutscene::Flyby::finish_ms(&sp) as f32 + 100.0 {
        let f = fb.step(&sp, cut.offset, if t == 0.0 { 0.0 } else { 2000.0 });
        println!("t {:>6.0} src {:?} tgt {:?} roll {:.1} fov {:.1}", fb.timer_ms(), f.source, f.target, f.roll, f.fov);
        t += 2000.0;
        if t > 20000.0 {
            break;
        }
    }
    let anims = ifp::parse(img.get(&format!("{name}.ifp")).unwrap())?;
    for a in &anims {
        println!("anim {} dur {:.2} tracks {} first {:?}", a.name, a.duration, a.tracks.len(), a.tracks.first().map(|t| (&t.bone_name, t.bone_id, t.keys.len(), t.keys[0].pos)));
    }
    Ok(())
}
