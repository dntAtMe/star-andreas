//! `SC2_DEMO=1`: scripted skirmish once SC2 is ready (10 marines vs 14
//! zerglings attack-moving into each other at the map centre).
//! `SC2_SHOT=<png>` additionally saves a screenshot mid-fight and exits.

use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
};
use sc2_api::sim::{Alliance, Command};

use crate::{Sc2Control, Sc2Link, Sc2Unit};

const MARINE: u32 = 48;
const ZERGLING: u32 = 105;

pub(crate) fn demo(
    mut commands: Commands,
    time: Res<Time>,
    link: Option<Res<Sc2Link>>,
    mut ctl: ResMut<Sc2Control>,
    units: Query<&Sc2Unit>,
    mut stage: Local<(u8, f32)>,
    mut exit: MessageWriter<AppExit>,
) {
    if std::env::var_os("SC2_DEMO").is_none() {
        return;
    }
    let Some(link) = link else { return };
    let Some(info) = link.ready() else { return };
    let [cx, cy] = info.center();
    let now = time.elapsed_secs();
    let of = |t: u32| units.iter().filter(move |u| u.unit_type == t).map(|u| u.tag).collect::<Vec<_>>();
    match stage.0 {
        0 => {
            let enemy = 3 - info.player_id as i32;
            link.send(Command::Spawn { unit_type: MARINE, owner: info.player_id as i32, at: [cx - 8.0, cy], count: 10 });
            link.send(Command::Spawn { unit_type: ZERGLING, owner: enemy, at: [cx + 8.0, cy], count: 14 });
            *stage = (1, now);
        }
        1 if of(MARINE).len() >= 10 && of(ZERGLING).len() >= 14 => {
            let marines = of(MARINE);
            ctl.selected = marines.iter().copied().collect();
            link.send(Command::AttackMove { units: marines, to: [cx + 8.0, cy], queue: false });
            link.send(Command::AttackMove { units: of(ZERGLING), to: [cx - 8.0, cy], queue: false });
            *stage = (2, now);
        }
        2 if now - stage.1 > 3.0 => {
            if let Ok(path) = std::env::var("SC2_SHOT") {
                info!("sc2 demo: screenshot -> {path}");
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            }
            *stage = (3, now);
        }
        3 if now - stage.1 > 1.0 => {
            let n = |a| units.iter().filter(|u| u.alliance == a).count();
            info!("sc2 demo: own {} enemy {} neutral {}", n(Alliance::Own), n(Alliance::Enemy), n(Alliance::Neutral));
            if std::env::var_os("SC2_SHOT").is_some() {
                exit.write(AppExit::Success);
            }
            *stage = (4, now);
        }
        _ => {}
    }
}
