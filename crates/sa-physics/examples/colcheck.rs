//! Build every COL model from gta3.img and probe it with a sphere and a vertical line
//! through its bounding sphere: checks for panics / NaN contacts on real data.
use sa_formats::{col, img::Img};
use sa_physics::{
    Vec3,
    collision::{ColLine, ColModel, ColSphere, MAX_COLPOINTS, Surf, process_col_models},
    colpoint::ColPoint,
    physical::Matrix,
};

fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas");
    let img = Img::open(&root.join("models/gta3.img"))?;
    let (mut models, mut tris, mut contacts, mut line_hits, mut bad) = (0, 0, 0, 0, 0);
    for e in img.entries().iter().filter(|e| e.name.to_ascii_lowercase().ends_with(".col")) {
        let data = img.data(e);
        for c in col::index(data)? {
            let m = ColModel::from_col(&col::parse_model(&data[c.offset..c.offset + c.size])?);
            models += 1;
            tris += m.tris.len();
            // Probe: a 0.5 m ball dropped onto the top of the model, with a wheel line under it.
            let top = Vec3::new(m.bound_center.x, m.bound_center.y, m.bbox_max.z);
            let mut probe = ColModel {
                bbox_min: Vec3::new(-0.5, -0.5, -2.5),
                bbox_max: Vec3::splat(0.5),
                bound_radius: 2.5,
                spheres: vec![ColSphere { center: Vec3::ZERO, radius: 0.5, surf: Surf::default() }],
                ..Default::default()
            };
            probe.lines.push(ColLine { start: Vec3::ZERO, end: Vec3::new(0.0, 0.0, -2.5) });
            let mat_a = Matrix { pos: top + Vec3::new(0.0, 0.0, 0.3), ..Matrix::IDENTITY };
            let mut pts = [ColPoint::default(); MAX_COLPOINTS];
            let mut lp = [ColPoint::default(); 1];
            let mut lv = [1.0f32];
            let n = process_col_models(&mat_a, &probe, &Matrix::IDENTITY, &m, &mut pts, &mut lp, &mut lv, false);
            contacts += n;
            if lv[0] < 1.0 {
                line_hits += 1;
            }
            if pts[..n].iter().any(|p| !p.point.is_finite() || !p.normal.is_finite()) || !lv[0].is_finite() {
                bad += 1;
            }
        }
    }
    println!("{models} col models, {tris} triangles: {contacts} contacts, {line_hits} line hits, {bad} with NaN/inf");
    Ok(())
}
