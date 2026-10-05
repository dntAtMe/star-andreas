//! `cargo run -p sa-formats --example audio -- <game dir> <track id> [bank sound]`: decode check.
fn main() {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().expect("game dir"));
    let au = sa_formats::audio::SaAudio::open(&root).expect("audio config");
    println!("{} sfx paks, {} banks, {} stream paks, {} tracks", au.sfx_paks.len(), au.banks.len(), au.stream_paks.len(), au.tracks.len());
    let t: u16 = a.next().and_then(|v| v.parse().ok()).unwrap_or(703);
    match au.track_ogg(t) {
        Some(o) => println!("track {t}: {} ogg bytes", o.len()),
        None => println!("track {t}: none"),
    }
    if let (Some(b), Some(s)) = (a.next().and_then(|v| v.parse().ok()), a.next().and_then(|v| v.parse().ok())) {
        match au.sound(b, s) {
            Some(x) => println!("bank {b} sound {s}: {} samples @ {} Hz loop {} headroom {}", x.samples.len(), x.rate, x.loop_start, x.headroom_db),
            None => println!("bank {b} sound {s}: none"),
        }
    }
}
