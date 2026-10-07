//! `C3dMarkers` (0x725040 and friends): the 32-slot pool of 3D markers — the yellow enex
//! cones, the blip cones above mission entities, the red locate / sphere cylinders.
//!
//! Callers queue placements during the frame (`place`, `place_set`, `place_cone`); `process`
//! runs PlaceMarker for each, then Update + Render: markers not re-placed since the last Render
//! are destroyed, the rest are drawn with the diamond_3 / cylinder / hoop models.

use std::collections::HashMap;

use bevy::{prelude::*, transform::TransformSystems};
use sa_physics::{automobile::Automobile, ped::PedLogic};

use crate::{
    colstore::ColStore,
    radar::{BlipType, Radar},
    saphys::{SaPhys, SaPhysExt},
    stream::{Cache, Loader, ModelState, request_model},
    world::{WorldRes, g2b},
    world_material::WorldMaterial,
};

pub struct MarkersPlugin;

impl Plugin for MarkersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Markers>().add_systems(PostUpdate, process.before(TransformSystems::Propagate));
    }
}

pub mod ty {
    pub const ARROW: u16 = 0;
    pub const CYLINDER: u16 = 1;
    pub const TUBE: u16 = 2;
    pub const ARROW2: u16 = 3;
    pub const TORUS: u16 = 4;
    pub const CONE: u16 = 5;
    pub const CONE_NO_COLLISION: u16 = 6;
    pub const NA: u16 = 0x101;
}

/// IDE ids of the marker models: diamond_3, cylinder, hoop.
const DIAMOND: u32 = 1559;
const CYLINDER_MODEL: u32 = 1317;
const HOOP: u32 = 1316;

/// One `C3dMarker` (0xA0).
#[derive(Clone)]
struct Marker {
    pos: Vec3,
    /// The accumulated RotateZ (radians).
    yaw: f32,
    ty: u16,
    used: bool,
    must_render: bool,
    id: u32,
    colour: [u8; 4],
    rotate_rate: i16,
    std_size: f32,
    size: f32,
    camera_range: f32,
    normal: Vec3,
    last_map: (i16, i16),
    roof: f32,
    last_pos: Vec3,
    onscreen_time: u32,
}

impl Default for Marker {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            yaw: 0.0,
            ty: ty::NA,
            used: false,
            must_render: false,
            id: 0,
            colour: [255; 4],
            rotate_rate: 5,
            std_size: 1.0,
            size: 1.0,
            camera_range: 0.0,
            normal: Vec3::Z,
            last_map: (30000, 0),
            roof: 65535.0,
            last_pos: Vec3::ZERO,
            onscreen_time: 0,
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    #[allow(dead_code)]
    Plain,
    /// `PlaceMarkerSet`: alpha / 3, rotate 1.
    Set,
    /// `PlaceMarkerCone`: type 5/6, alpha 255, rotate 0, not within 1.6 m of the camera.
    Cone,
}

#[derive(Clone, Copy)]
struct Req {
    kind: Kind,
    id: u32,
    ty: u16,
    pos: Vec3,
    size: f32,
    rgba: [u8; 4],
    rotate: i16,
    normal: Vec3,
    zcheck: bool,
}

/// `C3dMarkers` statics.
#[derive(Resource)]
pub struct Markers {
    slots: Vec<Marker>,
    /// `m_angleDiamond` (degrees, never wrapped).
    angle: f32,
    reqs: Vec<Req>,
    /// The rendered entity of each slot (with the part materials), and its model.
    ents: Vec<Option<(Entity, u32, Vec<Handle<WorldMaterial>>)>>,
    /// Byte 0x8D5E44: the last alpha given to a marker material.
    last_alpha: u8,
    radius: HashMap<u32, f32>,
    /// `CTheScripts::OnAMissionFlag` (contact points are hidden on a mission).
    pub on_mission: bool,
}

impl Default for Markers {
    fn default() -> Self {
        Self { slots: vec![Marker::default(); 32], angle: 0.0, reqs: Vec::new(), ents: vec![None; 32], last_alpha: 255, radius: HashMap::new(), on_mission: false }
    }
}

impl Markers {
    /// `C3dMarkers::PlaceMarker` (0x725120), queued.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub fn place(&mut self, id: u32, ty: u16, pos: Vec3, size: f32, rgba: [u8; 4], rotate: i16, normal: Vec3, zcheck: bool) {
        self.reqs.push(Req { kind: Kind::Plain, id, ty, pos, size, rgba, rotate, normal, zcheck });
    }

    /// `PlaceMarkerSet` (0x725BA0): alpha × 1/3, rotate rate 1, no normal, no z check.
    pub fn place_set(&mut self, id: u32, ty: u16, pos: Vec3, size: f32, rgba: [u8; 4]) {
        let rgba = [rgba[0], rgba[1], rgba[2], (rgba[3] as f32 * 0.333_333_3) as u8];
        self.reqs.push(Req { kind: Kind::Set, id, ty, pos, size, rgba, rotate: 1, normal: Vec3::ZERO, zcheck: false });
    }

    /// `PlaceMarkerCone` (0x726D40): CONE (collision) or CONE_NO_COLLISION, opaque, no spin.
    pub fn place_cone(&mut self, id: u32, pos: Vec3, size: f32, rgb: [u8; 3], collision: bool) {
        let ty = if collision { ty::CONE } else { ty::CONE_NO_COLLISION };
        self.reqs.push(Req { kind: Kind::Cone, id, ty, pos, size, rgba: [rgb[0], rgb[1], rgb[2], 255], rotate: 0, normal: Vec3::ZERO, zcheck: false });
    }

    /// `CTheScripts::HighlightImportantArea` (0x485E00): three nested red cylinders.
    pub fn highlight_area(&mut self, sa: &mut SaPhys, id: u32, a: Vec2, b: Vec2, z: f32) {
        let (min, max) = (a.min(b), a.max(b));
        let c = (min + max) * 0.5;
        let z = if z > -100.0 { z } else { sa.world.find_ground_z(c.extend(1000.0)).unwrap_or(0.0) + 2.0 };
        let p = c.extend(z);
        let r = (max.x - c.x).max(max.y - c.y);
        for k in [0.8, 0.9, 1.0] {
            self.place_set(id, ty::CYLINDER, p, r * k, [255, 0, 0, 255]);
        }
    }

    fn delete(&mut self, i: usize) {
        let m = &mut self.slots[i];
        m.id = 0;
        m.used = false;
        m.must_render = false;
        m.ty = ty::NA;
    }
}

fn cell(p: Vec3) -> (i16, i16) {
    (p.x as i32 as i16, p.y as i32 as i16)
}

/// The world queries PlaceMarker needs.
struct Ctx<'a> {
    sa: &'a mut SaPhys,
    col: Option<&'a ColStore>,
    centre: Vec3,
    cam: Vec3,
}

impl Ctx<'_> {
    fn water(&self, p: Vec3) -> Option<f32> {
        self.sa.world.water.as_ref()?.level_no_waves(p.x, p.y, p.z).map(|r| r.z)
    }

    /// `C3dMarker::UpdateZCoordinate` (0x724D40).
    fn update_z(&mut self, m: &mut Marker, p: Vec3, zdist: f32) {
        if cell(m.pos) == m.last_map {
            return;
        }
        let near = (p.x - m.pos.x).powi(2) + (p.y - m.pos.y).powi(2) < 10000.0;
        if near && self.col.is_none_or(|c| c.has_collision_loaded(m.pos)) {
            if let Some(g) = self.sa.world.find_ground_z(m.pos + Vec3::Z) {
                m.pos.z = g - zdist * 0.05;
            }
            m.last_map = cell(m.pos);
        }
    }

    /// Water first, else the ground under the player centre (CYLINDER).
    fn snap_cylinder(&mut self, m: &mut Marker, size: f32) {
        match self.water(m.pos) {
            Some(w) if w >= m.pos.z => m.pos.z = w,
            _ => {
                let c = self.centre;
                self.update_z(m, c, size);
            }
        }
    }
}

/// `StdSizeAndAlpha` of PlaceMarker.
fn std_size_and_alpha(m: &mut Marker, ty: u16, size: f32, a: u8, dist: f32) {
    if matches!(ty, ty::ARROW | ty::ARROW2 | ty::CONE | ty::CONE_NO_COLLISION) {
        m.std_size = if dist >= 25.0 {
            size
        } else if dist <= 5.0 {
            size - size * 0.3
        } else {
            size - (25.0 - dist) * size * 0.015
        };
    }
    if ty != ty::ARROW2 {
        let a = a as f32;
        m.colour[3] = if dist >= size + 12.0 {
            a as u8
        } else if dist <= size + 1.0 {
            (a * 0.65) as u8
        } else {
            ((1.0 - (size + 12.0 - dist) * 0.031_818_1) * a) as u8
        };
    }
}

/// `C3dMarkers::PlaceMarker` (0x725120).
fn place_marker(mk: &mut Markers, cx: &mut Ctx, r: Req, roof_radius: f32) {
    let mut pos = r.pos;
    let (id, ty, mut size) = (r.id, r.ty, r.size);
    let a = r.rgba[3];
    let mut dist = (pos.truncate() - cx.centre.truncate()).length();
    if ty == ty::TUBE {
        dist *= 0.25;
    } else if ty > ty::CONE_NO_COLLISION {
        return;
    }
    let now = cx.sa.world.now_ms;
    let ts = cx.sa.world.ts();
    // Slot: an un-used one with this id, a free one, or steal the farthest arrow/cone.
    let slot = mk.slots.iter().position(|m| !m.used && m.id == id).or_else(|| mk.slots.iter().position(|m| m.ty == ty::NA));
    let slot = match slot {
        Some(s) => s,
        None => {
            if !matches!(ty, 0 | 3 | 5 | 6) {
                return;
            }
            let steal = mk
                .slots
                .iter()
                .enumerate()
                .filter(|(_, m)| matches!(m.ty, 0 | 3 | 5 | 6) && m.camera_range > dist)
                .max_by(|a, b| a.1.camera_range.total_cmp(&b.1.camera_range))
                .map(|(i, _)| i);
            let Some(s) = steal else { return };
            mk.slots[s].ty = ty::NA;
            s
        }
    };
    let angle = mk.angle;
    let m = &mut mk.slots[slot];
    m.camera_range = dist;
    if m.id != id || m.ty != ty {
        // ---- a new marker ----
        let mut m2 = Marker { last_map: (30000, 0), ..Default::default() };
        if matches!(ty, ty::CONE | ty::CONE_NO_COLLISION) {
            pos.z += (angle.to_radians()).sin() * 0.3;
        }
        m2.pos = pos;
        // AddMarker.
        m2.id = id;
        m2.std_size = size;
        m2.size = size;
        m2.colour = r.rgba;
        m2.rotate_rate = r.rotate;
        m2.ty = ty;
        m2.roof = 65535.0;
        m2.onscreen_time = now;
        m2.camera_range = dist;
        if ty == ty::CYLINDER {
            cx.snap_cylinder(&mut m2, size);
        }
        std_size_and_alpha(&mut m2, ty, size, a, dist);
        m2.normal = r.normal;
        m2.used = true;
        *m = m2;
        return;
    }
    // ---- the same marker again ----
    if matches!(ty, ty::ARROW | ty::CONE) {
        if m.last_pos.length_squared() < 0.0001 || now.wrapping_sub(m.onscreen_time) >= 2000 {
            m.onscreen_time = now;
            if pos != m.last_pos {
                m.last_pos = pos;
                let hit = cx.sa.world.line_of_sight(pos - Vec3::Z * 1.5, pos, true, None);
                m.roof = hit.map_or(65535.0, |(_, _, cp)| cp.point.z);
            }
        }
        if m.roof < 65535.0 {
            size *= 0.5;
            pos.z = m.roof - roof_radius * 0.1;
        }
    }
    if matches!(ty, ty::CONE | ty::CONE_NO_COLLISION) {
        pos.z += angle.to_radians().sin() * if m.roof < 65535.0 { 0.15 } else { 0.3 };
    }
    std_size_and_alpha(m, ty, size, a, dist);
    // No pulse: sin(0) (§1.6).
    m.size = m.std_size;
    if m.rotate_rate != 0 {
        m.yaw += (m.rotate_rate as f32 * ts).to_radians();
    }
    let changed = cell(pos) != m.last_map;
    match ty {
        ty::ARROW | ty::CYLINDER | ty::CONE | ty::CONE_NO_COLLISION => {
            m.pos.x = pos.x;
            m.pos.y = pos.y;
            if changed {
                m.pos.z = pos.z;
            }
        }
        ty::TORUS | ty::TUBE => {
            m.pos.x = pos.x;
            m.pos.y = pos.y;
            if ty == ty::TUBE && r.zcheck {
                if changed {
                    m.pos.z = pos.z;
                }
                let mut mm = m.clone();
                cx.update_z(&mut mm, pos, 10.0);
                *m = mm;
            } else {
                m.pos.z = pos.z;
            }
        }
        _ => {}
    }
    if ty == ty::CYLINDER {
        let mut mm = m.clone();
        cx.snap_cylinder(&mut mm, size);
        *m = mm;
    }
    m.normal = r.normal;
    m.used = true;
}

/// `CRadar::Draw3dMarkers` (0x585BF0): cones over blip entities, cylinders at contact points.
fn draw_3d_markers(mk: &mut Markers, radar: &Radar, sa: &SaPhys) {
    let w = &sa.world;
    for (i, t) in radar.traces.iter().enumerate() {
        let Some(t) = t else { continue };
        if !matches!(t.display, 1 | 3) {
            continue;
        }
        let id = (t.counter as u32) << 16 | i as u32;
        let c = crate::radar::trace_colour(t.colour, t.bright, t.friendly);
        let rgb = [c[0], c[1], c[2]];
        match t.ty {
            BlipType::Car => {
                let Some(b) = t.entity.and_then(|e| w.body(e)) else { continue };
                let model = b.logic.as_any().downcast_ref::<Automobile>().map(|a| a.model);
                let k = if model == Some(553) { 0.6 } else { 1.2 };
                let p = b.phys.matrix.pos + Vec3::Z * (b.col.bbox_max.z * k + 2.0);
                mk.place_cone(id, p, 2.0, rgb, true);
            }
            BlipType::Char => {
                let Some(e) = t.entity else { continue };
                let e = sa.logic::<PedLogic>(e).and_then(|l| l.vehicle.as_ref().map(|v| v.veh)).unwrap_or(e);
                let Some(b) = w.body(e) else { continue };
                mk.place_cone(id, b.phys.matrix.pos + Vec3::Z * 2.7, 1.2, rgb, true);
            }
            BlipType::Object | BlipType::Pickup => {
                let (pos, h) = if t.ty == BlipType::Object {
                    let Some(b) = t.entity.and_then(|e| w.body(e)) else { continue };
                    (b.phys.matrix.pos, b.col.bbox_max.z)
                } else {
                    (t.pos, 2.0)
                };
                let k = if w.curr_area == 0 { 1.8 } else { 1.6 };
                mk.place_cone(id, pos + Vec3::Z * (h + k), 0.8, rgb, true);
            }
            BlipType::Contact => {
                if !mk.on_mission && w.curr_area == 0 {
                    mk.place_set(id, ty::CYLINDER, t.pos, 2.0, [255, 0, 0, 228]);
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process(
    mut commands: Commands,
    mut mk: ResMut<Markers>,
    mut sa: ResMut<SaPhys>,
    radar: Res<Radar>,
    overlay: Res<crate::hud::Overlay>,
    col: Option<Res<ColStore>>,
    world: Res<WorldRes>,
    loader: Res<Loader>,
    mut cache: ResMut<Cache>,
    meshes: Res<Assets<Mesh>>,
    mut mats: ResMut<Assets<WorldMaterial>>,
    mut tfs: Query<&mut Transform>,
    mut vis: Query<&mut Visibility>,
) {
    let mk = &mut *mk;
    // CHud::Draw → Draw3dMarkers (not in widescreen). In the game these land after the 3D
    // pass and so show one frame later; here they are drawn in the same frame.
    if !overlay.widescreen {
        draw_3d_markers(mk, &radar, &sa);
    }
    // The models (keep-in-memory requests) and the diamond's bounding radius.
    let mut models = HashMap::new();
    for id in [DIAMOND, CYLINDER_MODEL, HOOP] {
        match cache.models.get(&id) {
            Some(ModelState::Ready(m)) => {
                if !mk.radius.contains_key(&id) {
                    let r = m
                        .parts
                        .iter()
                        .filter_map(|p| meshes.get(&p.mesh))
                        .filter_map(|m| m.attribute(Mesh::ATTRIBUTE_POSITION).and_then(|a| a.as_float3()).map(|v| v.to_vec()))
                        .flatten()
                        .map(|v| Vec3::from(v).length())
                        .fold(0.0f32, f32::max);
                    mk.radius.insert(id, r);
                }
                models.insert(id, m.clone());
            }
            None => {
                cache.models.insert(id, ModelState::Loading);
                request_model(&world, &loader, id);
            }
            _ => {}
        }
    }
    let pid = sa.world.player_id();
    let veh = pid.and_then(|p| sa.logic::<PedLogic>(p)).and_then(|l| l.vehicle.as_ref().map(|v| v.veh));
    let centre = veh.or(pid).and_then(|e| sa.world.body(e)).map_or(Vec3::ZERO, |b| b.phys.matrix.pos);
    let cam = sa.world.camera_pos;
    let roof_radius = mk.radius.get(&DIAMOND).copied().unwrap_or(1.0);
    let reqs = std::mem::take(&mut mk.reqs);
    {
        let mut cx = Ctx { sa: &mut sa, col: col.as_deref(), centre, cam };
        for mut r in reqs {
            match r.kind {
                Kind::Plain | Kind::Set => {}
                Kind::Cone => {
                    if r.pos.distance(cx.cam) < 1.6 {
                        continue;
                    }
                }
            }
            if matches!(r.kind, Kind::Set) {
                r.normal = Vec3::ZERO;
            }
            place_marker(mk, &mut cx, r, roof_radius);
        }
    }
    // Update (0x7227B0).
    mk.angle += 5.0 * sa.world.ts();
    for m in mk.slots.iter_mut().filter(|m| m.used) {
        m.must_render = true;
    }
    if std::env::var("SA_MARKERLOG").is_ok() && sa.world.frame % 60 == 0 {
        for m in mk.slots.iter().filter(|m| m.must_render) {
            info!("marker id {:x} ty {} pos {:?} size {} colour {:?} range {}", m.id, m.ty, m.pos, m.size, m.colour, m.camera_range);
        }
    }
    // Render (0x725040).
    for i in 0..mk.slots.len() {
        if !mk.slots[i].must_render {
            if mk.slots[i].ty != ty::NA {
                mk.delete(i);
            }
            if let Some((e, ..)) = mk.ents[i].take() {
                commands.entity(e).despawn();
            }
            continue;
        }
        let m = mk.slots[i].clone();
        let model_id = match m.ty {
            ty::CYLINDER | ty::TUBE => CYLINDER_MODEL,
            ty::TORUS => HOOP,
            _ => DIAMOND,
        };
        let shown = m.camera_range < 150.0 || m.ty == ty::TORUS;
        let shown = shown && sa.world.sphere_visible(m.pos, 2.0);
        mk.slots[i].used = false;
        mk.slots[i].must_render = false;
        // The entity: rebuilt when the model changes.
        if mk.ents[i].as_ref().is_some_and(|(_, id, _)| *id != model_id) {
            if let Some((e, ..)) = mk.ents[i].take() {
                commands.entity(e).despawn();
            }
        }
        if mk.ents[i].is_none() {
            let Some(model) = models.get(&model_id) else { continue };
            let mut hs = Vec::new();
            let e = commands
                .spawn((Transform::default(), Visibility::Hidden))
                .with_children(|c| {
                    for part in model.parts.iter().filter(|p| !p.damaged) {
                        let Some(base) = mats.get(&part.material) else { continue };
                        let mut mat = base.clone();
                        mat.alpha_mode = AlphaMode::Blend;
                        let h = mats.add(mat);
                        hs.push(h.clone());
                        c.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(h), bevy::light::NotShadowCaster));
                    }
                })
                .id();
            mk.ents[i] = Some((e, model_id, hs));
            continue;
        }
        let Some((e, _, hs)) = mk.ents[i].clone() else { continue };
        if let Ok(mut v) = vis.get_mut(e) {
            *v = if shown { Visibility::Inherited } else { Visibility::Hidden };
        }
        if !shown {
            continue;
        }
        // C3dMarker::Render: orientation (TORUS / ARROW2 local Z → normal), uniform scale, TUBE z × 20.
        let mut rot = Quat::from_rotation_z(m.yaw);
        if matches!(m.ty, ty::TORUS | ty::ARROW2) && m.normal != Vec3::Z && m.normal != Vec3::ZERO {
            rot = Quat::from_rotation_arc(Vec3::Z, m.normal.normalize()) * rot;
        }
        let gscale = if m.ty == ty::TUBE { Vec3::new(m.size, m.size, m.size * 20.0) } else { Vec3::splat(m.size) };
        // GTA → Bevy: (x, y, z) → (x, z, -y).
        let to_b = |v: Vec3| g2b(v.to_array());
        let (gx, gy, gz) = (rot * Vec3::X * gscale.x, rot * Vec3::Y * gscale.y, rot * Vec3::Z * gscale.z);
        let mat = Mat3::from_cols(to_b(gx), to_b(gz), -to_b(gy));
        let (scale, q, _) = Mat4::from_mat3(mat).to_scale_rotation_translation();
        if let Ok(mut t) = tfs.get_mut(e) {
            *t = Transform { translation: to_b(m.pos), rotation: q, scale };
        }
        // Colour: cones and arrows always opaque; never two equal alphas in a row.
        let mut c = m.colour;
        if matches!(m.ty, ty::ARROW | ty::CONE | ty::CONE_NO_COLLISION) {
            c[3] = 255;
        }
        if c[3] == mk.last_alpha {
            c[3] = if c[3] > 127 { c[3] - 1 } else { c[3] + 1 };
        }
        mk.last_alpha = c[3];
        let col = Vec4::new(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32) / 255.0;
        for h in &hs {
            if let Some(mut mat) = mats.get_mut(h) {
                if mat.uniform.color != col {
                    mat.uniform.color = col;
                    // SetBrightMarkerColours: lit, not by the scene's ambient.
                    mat.uniform.params.x = 0.0;
                }
            }
        }
    }
}
