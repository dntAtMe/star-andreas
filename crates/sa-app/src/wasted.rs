//! The player's death (`CGameLogic::Update` 0x442AD0 / `CPlayerInfo::KillPlayer` 0x56E580):
//! the "WASTED" big message for 4000 ms, a 2.0 s fade to black from 3000 ms, then the
//! restart at the closest hospital with full health, no armour and no weapons.
//!
//! The hospital restart points come from main.scm in SA; this port has no script yet, so
//! the Los Santos ones are listed here [I: coordinates approximate].

use bevy::prelude::*;
use sa_physics::{peddamage::Life, ped::PedLogic};

use crate::{
    player::{Ped, ped_teleport},
    saphys::{SaPhys, SaPhysExt},
    world::{b2g, g2b},
};

pub struct WastedPlugin;

impl Plugin for WastedPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(Update, wasted);
    }
}

/// (x, y, z, heading degrees), GTA space.
const HOSPITALS: [[f32; 4]; 2] = [[1172.9, -1323.3, 15.4, 270.0], [2027.8, -1408.2, 17.0, 140.0]];

#[derive(Component)]
struct WastedText;

#[derive(Component)]
struct Fade;

fn setup(mut commands: Commands) {
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

fn wasted(
    mut sa: ResMut<SaPhys>,
    ped: Single<(&Ped, &Transform)>,
    mut text: Single<&mut Visibility, With<WastedText>>,
    mut fade: Single<&mut BackgroundColor, With<Fade>>,
    mut fade_in: Local<Option<u32>>,
) {
    let (ped, tf) = *ped;
    let now = sa.world.now_ms;
    let Some(life) = sa.logic::<PedLogic>(ped.sa).map(|l| l.tasks.health.life) else { return };
    match life {
        Life::Wasted { since_ms } => {
            let t = now.wrapping_sub(since_ms);
            **text = if t < 4000 { Visibility::Inherited } else { Visibility::Hidden };
            // From 3000 ms: a 2.0 s fade to black.
            let a = ((t as f32 - 3000.0) / 2000.0).clamp(0.0, 1.0);
            fade.0 = Color::srgba(0.0, 0.0, 0.0, a);
            if t >= 5000 {
                // Restart at the closest hospital.
                let p = b2g(tf.translation);
                let h = HOSPITALS
                    .iter()
                    .min_by(|a, b| {
                        let da = (a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2);
                        let db = (b[0] - p[0]).powi(2) + (b[1] - p[1]).powi(2);
                        da.total_cmp(&db)
                    })
                    .unwrap();
                ped_teleport(&mut sa, ped.sa, g2b([h[0], h[1], h[2]]), Some(h[3].to_radians()));
                if let Some(l) = sa.logic_mut::<PedLogic>(ped.sa) {
                    let m = l.tasks.anims.clone();
                    if let (Some(c), Some(m)) = (l.clump.as_deref_mut(), m) {
                        l.tasks.resurrect(c, &m);
                    }
                    l.knocked_down = 0.0;
                }
                *fade_in = Some(now);
                **text = Visibility::Hidden;
            }
        }
        _ => {
            **text = Visibility::Hidden;
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
        }
    }
}
