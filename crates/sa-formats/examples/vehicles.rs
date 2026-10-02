//! Validate every car in vehicles.ide: DFF, embedded COL, handling entry, colours.
use sa_formats::{col, dff, img::Img, vehicle};

fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let img = Img::open(&root.join("models/gta3.img"))?;
    let read = |p: &str| String::from_utf8_lossy(&std::fs::read(root.join(p)).unwrap()).into_owned();
    let defs = vehicle::parse_vehicles_ide(&read("data/vehicles.ide"));
    let handling = vehicle::parse_handling(&read("data/handling.cfg"));
    let colors = vehicle::parse_carcols(&read("data/carcols.dat"));
    println!(
        "{} vehicles, {} handling lines, {} palette colours, {} car colour sets",
        defs.len(),
        handling.len(),
        colors.palette.len(),
        colors.cars.len()
    );
    let (mut ok, mut fail) = (0, 0);
    for d in defs.iter().filter(|d| d.kind == "car") {
        let res = (|| -> anyhow::Result<String> {
            let data = img.get(&format!("{}.dff", d.model)).ok_or_else(|| anyhow::anyhow!("no dff"))?;
            let c = dff::parse(data)?;
            let raw = c.collision.as_deref().ok_or_else(|| anyhow::anyhow!("no embedded col"))?;
            let col = col::parse_model(raw)?;
            let h = handling.get(&d.handling).ok_or_else(|| anyhow::anyhow!("no handling {}", d.handling))?;
            let wheels = c.frames.iter().filter(|f| f.name.starts_with("wheel_") && f.name.ends_with("_dummy")).count();
            let has_wheel = c.frames.iter().any(|f| f.name == "wheel");
            Ok(format!(
                "mass {} accel {} vmax {} drive {} | dummies {wheels} wheel={has_wheel} | col s{} b{} f{}",
                h.mass,
                h.engine_accel,
                h.max_velocity,
                h.drive_type,
                col.spheres.len(),
                col.boxes.len(),
                col.faces.len()
            ))
        })();
        match res {
            Ok(s) => {
                ok += 1;
                if ["greenwoo", "infernus", "sabre", "bobcat"].contains(&d.model.as_str()) {
                    println!("  {} ({}): {s}", d.model, d.game_name);
                }
            }
            Err(e) => {
                fail += 1;
                println!("  FAIL {}: {e:#}", d.model);
            }
        }
    }
    println!("cars ok {ok}, failed {fail}");
    Ok(())
}
