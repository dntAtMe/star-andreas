//! Per-entity lighting of peds, vehicles and held weapons (timecycle.md §5.3, lights.md §3.2):
//! `CPhysical::SetupLighting` (0x553E40) scales the ambient light by
//! `m = (L·0.95 + 0.05) · dark`, where
//! * `L = GetLightingTotal()` (0x544850) = `GetLightingFromCol(interior)` (0x5447B0) + the point
//!   light term `+0x130`;
//! * `GetLightingFromCol` = contact-surface brightness (+0x12C, the ground's day/night collision
//!   lighting) + (ambient before brightness r+g+b)·0.333333, and outdoors
//!   `L·((PostFx1 r+g+b)/765·0.5 + 0.5) + (PostFx2 r+g+b)/765·0.5`;
//! * `dark` and `+0x130` come from `CPointLights::GenerateLightsAffectingObject` (0x6FFBB0).
//!
//! Bevy has one global ambient, so the ambient term is applied here as the material's emissive
//! (base colour · texture · AmbObj·m) and the global ambient is zero. Point lights stay
//! Bevy lights (SA adds them as extra directional lights, not scaled by `m`).
//!
//! Vehicles use the average of their wheel lighting bytes (0x6D0CF0).
//!
//! Not ported: BrightnessAddedToAmbient, the
//! directional light being scaled by `m` (DirMult is 0 on PC), interiors.

use bevy::{prelude::*, transform::TransformSystems};
use sa_physics::{automobile::Automobile, effects::PointLight, ped::PedLogic};

use crate::{
    player::Ped,
    saphys::{SaPhys, SaPhysExt, SaSync},
    vehicle::Vehicle,
};

pub struct DynLightPlugin;

impl Plugin for DynLightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, apply.after(SaSync).after(TransformSystems::Propagate));
    }
}

/// A lit mesh (its own material); the ped or vehicle it belongs to is found through its
/// ancestors.
#[derive(Component)]
pub struct DynLit;

/// `GenerateLightsAffectingObject`: (dynamic lighting `+0x130`, darkness factor).
pub fn lights_affecting(lights: &[PointLight], pos: Vec3) -> (f32, f32) {
    let (mut dynl, mut dark) = (0.0, 1.0);
    for l in lights {
        if l.ty == 3 || l.ty == 4 {
            continue;
        }
        let d = l.pos - pos;
        if d.x.abs() >= l.radius || d.y.abs() >= l.radius || d.z.abs() >= l.radius {
            continue;
        }
        let dist = d.length();
        if dist >= l.radius {
            continue;
        }
        let t = dist / l.radius;
        if l.ty == 2 {
            dark *= t;
            continue;
        }
        // PointLight colours are 0..1 here.
        dynl += (1.0 - t) * (l.color.x + l.color.y + l.color.z) / 3.0;
    }
    (dynl, dark)
}

fn apply(
    sa: Res<SaPhys>,
    mut ambient: ResMut<GlobalAmbientLight>,
    lit: Query<(Entity, &MeshMaterial3d<StandardMaterial>), With<DynLit>>,
    parents: Query<&ChildOf>,
    peds: Query<&Ped>,
    npcs: Query<&crate::peds::NpcPed>,
    vehicles: Query<&Vehicle>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    ambient.brightness = 0.0;
    let Some(tc) = sa.world.timecycle.as_ref() else { return };
    let c = &tc.current;
    let amb_obj = if sa.world.weather.lightning_flash { Vec3::ONE } else { c.ambient_obj * tc.lights_mult };
    let amb_before = c.ambient_before_brightness;
    let pfx1 = (c.post_fx1[0] + c.post_fx1[1] + c.post_fx1[2]) / 765.0;
    let pfx2 = (c.post_fx2[0] + c.post_fx2[1] + c.post_fx2[2]) / 765.0;
    let lights = &sa.world.effects.lights;
    let dn = sa.world.clock.dn_balance();

    // m for a body (contact brightness, position).
    let m_of = |contact: f32, pos: Vec3| {
        let mut l = contact + (amb_before.x + amb_before.y + amb_before.z) * 0.333_333;
        l = l * (pfx1 * 0.5 + 0.5) + pfx2 * 0.5;
        let (dynl, dark) = lights_affecting(lights, pos);
        ((l + dynl) * 0.95 + 0.05) * dark
    };
    for (e, mat) in &lit {
        // Owner: the nearest ancestor with a Ped or Vehicle.
        let mut owner = None;
        let mut cur = e;
        for _ in 0..64 {
            if let Ok(p) = peds.get(cur) {
                owner = Some((p.sa, true));
                break;
            }
            if let Ok(p) = npcs.get(cur) {
                owner = Some((p.sa, true));
                break;
            }
            if let Ok(v) = vehicles.get(cur) {
                owner = Some((v.sa, false));
                break;
            }
            match parents.get(cur) {
                Ok(c) => cur = c.parent(),
                Err(_) => break,
            }
        }
        let m = match owner {
            Some((id, is_ped)) => {
                let Some(b) = sa.world.body(id) else { continue };
                let contact = if is_ped {
                    sa.logic::<PedLogic>(id).map_or(1.0, |p| p.lighting)
                } else {
                    // CBoat::PreRender sets the contact brightness to 0.5.
                    sa.logic::<Automobile>(id).map_or(if sa.logic::<sa_physics::boat::Boat>(id).is_some() { 0.5 } else { 1.0 }, |a| a.lighting(dn))
                };
                m_of(contact, b.phys.matrix.pos)
            }
            None => m_of(1.0, Vec3::ZERO),
        };
        let k = (amb_obj * m).min(Vec3::ONE);
        let kl = Color::srgb(k.x, k.y, k.z).to_linear();
        let Some(cur) = materials.get(&mat.0) else { continue };
        if cur.unlit {
            continue;
        }
        let b = cur.base_color.to_linear();
        let em = LinearRgba::new(b.red * kl.red, b.green * kl.green, b.blue * kl.blue, 1.0);
        let old = cur.emissive;
        let tex_same = cur.emissive_texture == cur.base_color_texture;
        if tex_same
            && (old.red - em.red).abs() < 1e-4
            && (old.green - em.green).abs() < 1e-4
            && (old.blue - em.blue).abs() < 1e-4
        {
            continue;
        }
        let Some(mut mm) = materials.get_mut(&mat.0) else { continue };
        mm.emissive = em;
        if !tex_same {
            mm.emissive_texture = mm.base_color_texture.clone();
        }
    }
}
