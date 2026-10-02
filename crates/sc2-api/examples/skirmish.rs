//! Launches SC2, spawns marines vs zerglings in the middle of a ladder map
//! and prints the fight. `cargo run -p sc2-api --example skirmish -- [sc2 dir] [map]`

use std::time::{Duration, Instant};

use sc2_api::sim::{Alliance, Command, Event, SimConfig, SimHandle};

const MARINE: u32 = 48;
const ZERGLING: u32 = 105;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| r"G:\SC 2\StarCraft II".into());
    let map = args.next().unwrap_or_else(|| format!(r"{dir}\Maps\AcropolisLE.SC2Map"));

    let sim = SimHandle::spawn(SimConfig::new(&dir, &map));
    let t = Instant::now();
    let mut spawned = false;
    let mut ordered = false;
    let mut center = [0.0; 2];
    let mut last_print = Instant::now();
    while t.elapsed() < Duration::from_secs(120) {
        match sim.events.recv()? {
            Event::Ready(info) => {
                let [cx, cy] = info.center();
                center = [cx, cy];
                println!(
                    "ready after {:.1?}: {} {}x{}, player {}, {} unit types",
                    t.elapsed(),
                    info.name,
                    info.size[0],
                    info.size[1],
                    info.player_id,
                    info.unit_names.len()
                );
                sim.send(Command::Spawn { unit_type: MARINE, owner: 1, at: [cx - 6.0, cy], count: 8 });
                sim.send(Command::Spawn { unit_type: ZERGLING, owner: 2, at: [cx + 6.0, cy], count: 12 });
                spawned = true;
            }
            Event::Snapshot(s) if spawned => {
                let marines: Vec<u64> = s.units.iter().filter(|u| u.unit_type == MARINE).map(|u| u.tag).collect();
                if !ordered && !marines.is_empty() {
                    sim.send(Command::AttackMove { units: marines, to: [center[0] + 6.0, center[1]], queue: false });
                    ordered = true;
                }
                if last_print.elapsed() > Duration::from_millis(500) {
                    last_print = Instant::now();
                    let of = |a, t| s.units.iter().filter(move |u| u.alliance == a && u.unit_type == t);
                    let count = |a| of(a, if a == Alliance::Own { MARINE } else { ZERGLING }).count();
                    let hp = |a| of(a, if a == Alliance::Own { MARINE } else { ZERGLING }).map(|u| u.health).sum::<f32>();
                    println!(
                        "loop {:5}: own {:2} ({:4.0} hp)  enemy {:2} ({:4.0} hp)  dead +{}",
                        s.game_loop,
                        count(Alliance::Own),
                        hp(Alliance::Own),
                        count(Alliance::Enemy),
                        hp(Alliance::Enemy),
                        s.dead.len()
                    );
                    if s.game_loop > 200 && (count(Alliance::Own) == 0 || count(Alliance::Enemy) == 0) {
                        break;
                    }
                }
            }
            Event::Snapshot(_) => {}
            Event::Ended(r) => {
                println!("game ended: {r}");
                break;
            }
            Event::Failed(e) => anyhow::bail!(e),
        }
    }
    Ok(())
}
