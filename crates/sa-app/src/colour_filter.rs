//! `CPostEffects::ColourFilter` with the timecyc PostFx1 / PostFx2 colours (timecycle.md §6),
//! evaluated in the post-effect shader (heat_haze.wgsl). The light-adaptation factor is 1
//! outdoors (the player's lighting total is above the 0.35 cap), so it is not modelled.

use bevy::prelude::*;

use crate::{debug::DebugUi, heat_haze::HeatHaze, saphys::SaPhys};

pub struct ColourFilterPlugin;

impl Plugin for ColourFilterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, update);
    }
}

fn update(sa: Res<SaPhys>, dbg: Res<DebugUi>, mut cams: Query<&mut HeatHaze>) {
    let Some(tc) = sa.world.timecycle.as_ref() else { return };
    let c = &tc.current;
    // ftol of the interpolated colours; alpha 254 on PC.
    let k = |p: [f32; 4]| {
        let a = (p[3] as i32) as f32 / 255.0;
        Vec3::new((p[0] as i32) as f32, (p[1] as i32) as f32, (p[2] as i32) as f32) / 255.0 * a
    };
    let on = if dbg.colour_filter { 1.0 } else { 0.0 };
    for mut h in &mut cams {
        h.k1 = k(c.post_fx1).extend(on);
        h.k2 = k(c.post_fx2).extend(0.0);
    }
}
