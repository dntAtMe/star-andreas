//! The player's death and arrest (`CGameLogic::Update` 0x442AD0, wanted.md §7):
//! WASTED — the big message, a 2.0 s fade to black from 3000 ms, then the restart at the
//! closest hospital (−$100); BUSTED — the same fade from 3000 ms, the restart after 4000 ms at
//! the closest police station with the fine by wanted level and the weapons taken.
//! Both restore the player (full health, no armour) and reset the wanted level. The restart
//! points are main.scm's ADD_HOSPITAL_RESTART / ADD_POLICE_RESTART.

use bevy::prelude::*;
use sa_physics::{ped::PedLogic, peddamage::Life};

use crate::{
    player::{GameRoot, Ped, ped_teleport},
    saphys::{SaPhys, SaPhysExt},
    world::{b2g, g2b},
};

pub struct WastedPlugin;

impl Plugin for WastedPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(Update, wasted);
    }
}

/// main.scm restart points (x, y, z, heading degrees), GTA space.
#[derive(Resource, Default)]
struct Restarts {
    hospitals: Vec<[f32; 4]>,
    police: Vec<[f32; 4]>,
}

#[derive(Component)]
struct WastedText;

#[derive(Component)]
struct Fade;

fn setup(mut commands: Commands, root: Res<GameRoot>) {
    let (hospitals, police) = std::fs::read(root.0.join("data/script/main.scm"))
        .map(|scm| sa_formats::population::scan_scm_restarts(&scm))
        .unwrap_or_default();
    if hospitals.is_empty() || police.is_empty() {
        warn!("main.scm: {} hospital / {} police restarts", hospitals.len(), police.len());
    }
    commands.insert_resource(Restarts { hospitals, police });
    commands.spawn((
        Text::new("WASTED"),
        TextFont { font_size: bevy::text::FontSize::Px(64.0), ..default() },
        TextColor(Color::srgb_u8(180, 25, 29)),
        TextLayout { justify: Justify::Center, ..default() },
        Node {
            position_type: PositionType::Absolute,
            width: percent(100),
            top: percent(42),
            justify_content: JustifyContent::Center,
            ..default()
        },
        Visibility::Hidden,
        GlobalZIndex(10),
        WastedText,
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
        GlobalZIndex(9),
        Fade,
    ));
}

fn closest(points: &[[f32; 4]], p: [f32; 3]) -> Option<[f32; 4]> {
    points
        .iter()
        .min_by(|a, b| {
            let da = (a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2);
            let db = (b[0] - p[0]).powi(2) + (b[1] - p[1]).powi(2);
            da.total_cmp(&db)
        })
        .copied()
}

#[allow(clippy::too_many_arguments)]
fn wasted(
    mut sa: ResMut<SaPhys>,
    restarts: Option<Res<Restarts>>,
    time: Res<Time>,
    ped: Single<(&mut Ped, &Transform)>,
    mut text: Single<(&mut Visibility, &mut Text), With<WastedText>>,
    mut fade: Single<&mut BackgroundColor, With<Fade>>,
    mut fade_in: Local<Option<u32>>,
    mut busted_at: Local<Option<u32>>,
) {
    let (mut ped, tf) = ped.into_inner();
    let now = sa.world.now_ms;
    let Some((life, arrested)) = sa.logic::<PedLogic>(ped.sa).map(|l| (l.tasks.health.life, l.tasks.arrested)) else { return };
    let (vis, txt) = &mut *text;
    // GameState 1 WASTED / 2 BUSTED: the time of the event.
    let event = match life {
        Life::Wasted { since_ms } => Some((since_ms, false)),
        _ if arrested => Some((*busted_at.get_or_insert(now), true)),
        _ => None,
    };
    let Some((t0, busted)) = event else {
        *busted_at = None;
        **vis = Visibility::Hidden;
        match *fade_in {
            Some(t0) => {
                let a = 1.0 - (now.wrapping_sub(t0) as f32 / 1000.0).clamp(0.0, 1.0);
                fade.0 = Color::srgba(0.0, 0.0, 0.0, a);
                if a <= 0.0 {
                    *fade_in = None;
                }
            }
            None => fade.0 = Color::srgba(0.0, 0.0, 0.0, 0.0),
        }
        return;
    };
    let t = now.wrapping_sub(t0);
    txt.0 = if busted { "BUSTED".into() } else { "WASTED".into() };
    **vis = if t < 4000 { Visibility::Inherited } else { Visibility::Hidden };
    // From 3000 ms: a 2.0 s fade to black.
    let a = ((t as f32 - 3000.0) / 2000.0).clamp(0.0, 1.0);
    fade.0 = Color::srgba(0.0, 0.0, 0.0, a);
    if t < 5000 {
        return;
    }
    // Restart: the closest hospital / police station, the penalty, the player restored.
    let p = b2g(tf.translation);
    let pts = restarts.as_ref().map(|r| if busted { &r.police } else { &r.hospitals });
    let level = sa.world.wanted.level;
    if let Some(r) = pts.and_then(|v| closest(v, p)) {
        ped_teleport(&mut sa, ped.sa, g2b([r[0], r[1], r[2]]), Some(r[3].to_radians()));
        // Wait for the area's collision before gravity (as at the start).
        ped.frozen = true;
        ped.frozen_at = time.elapsed_secs();
        if let Some(b) = sa.world.body_mut(ped.sa) {
            b.phys.eflags |= sa_physics::physical::ef::IS_STATIC;
        }
    }
    let fine = if busted {
        match level {
            0 | 1 => 100,
            2 => 200,
            3 => 400,
            4 => 600,
            5 => 900,
            _ => 1500,
        }
    } else {
        100
    };
    sa.world.money = (sa.world.money - fine).max(0);
    sa.world.wanted.reset();
    if let Some(l) = sa.logic_mut::<PedLogic>(ped.sa) {
        let m = l.tasks.anims.clone();
        if let (Some(c), Some(m)) = (l.clump.as_deref_mut(), m) {
            l.tasks.resurrect(c, &m);
        }
        l.tasks.arrested = false;
        l.knocked_down = 0.0;
    }
    *busted_at = None;
    *fade_in = Some(now);
    **vis = Visibility::Hidden;
}
