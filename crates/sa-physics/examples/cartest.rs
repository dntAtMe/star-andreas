//! Drive a real car (from the game files) on flat ground with the ported physics.
//! cargo run -p sa-physics --example cartest --release -- infernus
use std::sync::Arc;

use sa_formats::{col, dff, img::Img, vehicle};
use sa_physics::{
    Vec3,
    automobile::{Automobile, CarInput, VehicleHandling},
    collision::{ColModel, ColTriangle, TrianglePlane},
    physical::{EntityType, Matrix, Physical, Status, VehicleClass, VehicleInfo},
    surface::SurfaceInfos,
    world::World,
};

fn ground() -> Arc<ColModel> {
    let s = 2000.0;
    let verts = vec![Vec3::new(-s, -s, 0.0), Vec3::new(s, -s, 0.0), Vec3::new(s, s, 0.0), Vec3::new(-s, s, 0.0)];
    let tris = vec![ColTriangle { v: [0, 2, 1], material: 1, light: 0 }, ColTriangle { v: [0, 3, 2], material: 1, light: 0 }];
    let planes = tris.iter().map(|t| TrianglePlane::new(verts[t.v[0] as usize], verts[t.v[1] as usize], verts[t.v[2] as usize])).collect();
    Arc::new(ColModel {
        bbox_min: Vec3::new(-s, -s, -0.1),
        bbox_max: Vec3::new(s, s, 0.1),
        bound_radius: s * 1.5,
        verts,
        tris,
        planes,
        ..Default::default()
    })
}

fn main() -> anyhow::Result<()> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "infernus".into());
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let read = |p: &str| String::from_utf8_lossy(&std::fs::read(root.join(p)).unwrap()).into_owned();
    let img = Img::open(&root.join("models/gta3.img"))?;
    let def = vehicle::parse_vehicles_ide(&read("data/vehicles.ide")).into_iter().find(|d| d.model == name).unwrap();
    let raw = vehicle::parse_handling(&read("data/handling.cfg")).remove(&def.handling).unwrap();
    let surfaces = SurfaceInfos::parse(&read("data/surface.dat"), &read("data/surfinfo.dat"));
    let clump = dff::parse(img.get(&format!("{name}.dff")).unwrap())?;
    let mut colm = ColModel::from_col(&col::parse_model(clump.collision.as_deref().unwrap())?);
    let dummy = |n: &str| {
        let i = clump.frames.iter().position(|f| f.name.eq_ignore_ascii_case(n)).unwrap();
        Vec3::from(clump.frame_world(i).1)
    };
    let dummies = [dummy("wheel_lf_dummy"), dummy("wheel_lb_dummy"), dummy("wheel_rf_dummy"), dummy("wheel_rb_dummy")];
    let h = VehicleHandling::from_raw(&raw);
    println!(
        "{name}: mass {} accel/wheel {:.6} top {:.3} u/f ({:.0} km/h) gears {:?}",
        h.mass,
        h.trans.engine_accel,
        h.trans.max_forward,
        h.trans.max_forward * 50.0 * 3.6,
        h.trans.gears.iter().map(|g| (g.max_vel * 180.0) as i32).collect::<Vec<_>>()
    );
    let car = Automobile::new(h, def.id as u16, def.wheel_scale_front, def.wheel_scale_rear, dummies, &mut colm, surfaces.clone());
    let mut phys = Physical::new(EntityType::Vehicle, Matrix { pos: Vec3::new(0.0, 0.0, 1.5), ..Matrix::IDENTITY });
    phys.vehicle = Some(VehicleInfo { class: VehicleClass::Automobile, model: def.id as u16, towed_mass: None });
    phys.status = Status::Player;
    car.setup_physical(&mut phys);
    let mut w = World::new(surfaces);
    w.add_building(Matrix::IDENTITY, ground());
    let id = w.add_body(phys, colm, Box::new(car));

    let ts = 30.0f32.recip() * 50.0; // 30 fps, as the game is usually played
    let fps = 30;
    let log = |w: &World, t: f32, label: &str| {
        let b = w.body(id).unwrap();
        let car = b.logic.as_any().downcast_ref::<Automobile>().unwrap();
        let v = b.phys.move_speed;
        println!(
            "{label} t={t:5.1}s pos {:6.1} {:6.1} z {:.3} speed {:6.1} km/h gear {} comp {:.2?} contacts {}",
            b.phys.matrix.pos.x,
            b.phys.matrix.pos.y,
            b.phys.matrix.pos.z,
            v.length() * 50.0 * 3.6,
            car.gear,
            car.comp_prev,
            car.num_contact_wheels
        );
    };
    let set = |w: &mut World, inp: CarInput| {
        w.body_mut(id).unwrap().logic.as_any_mut().downcast_mut::<Automobile>().unwrap().input = inp;
    };
    for f in 0..(3 * fps) {
        w.process(ts);
        if f % fps == fps - 1 {
            log(&w, (f + 1) as f32 / fps as f32, "settle");
        }
    }
    set(&mut w, CarInput { accelerate: 1.0, ..Default::default() });
    for f in 0..(20 * fps) {
        w.process(ts);
        if f % fps == fps - 1 {
            log(&w, (f + 1) as f32 / fps as f32, "full ");
        }
    }
    set(&mut w, CarInput { accelerate: 1.0, steer: 1.0, ..Default::default() });
    for f in 0..(4 * fps) {
        w.process(ts);
        if f % fps == fps - 1 {
            log(&w, (f + 1) as f32 / fps as f32, "left ");
        }
    }
    set(&mut w, CarInput { brake: 1.0, ..Default::default() });
    for f in 0..(5 * fps) {
        w.process(ts);
        if f % fps == fps - 1 {
            log(&w, (f + 1) as f32 / fps as f32, "brake");
        }
    }
    Ok(())
}
