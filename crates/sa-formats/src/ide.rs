//! IDE item definition files (text). Only the sections needed for the map
//! are parsed: `objs`, `tobj`, `anim`, `weap` and `txdp`.

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct ObjectDef {
    pub id: u32,
    pub model: String,
    pub txd: String,
    pub draw_distance: f32,
    pub flags: u32,
    /// `tobj` visibility window in game hours, if timed.
    pub time: Option<(u8, u8)>,
}

#[derive(Debug, Clone, Default)]
pub struct Ide {
    pub objects: Vec<ObjectDef>,
    /// `weap` (default.ide): weapon models.
    pub weapons: Vec<ObjectDef>,
    /// (txd, parent txd)
    pub txd_parents: Vec<(String, String)>,
}

/// Split an IDE/IPL line into trimmed fields (comma and/or whitespace separated).
pub fn fields(line: &str) -> Vec<&str> {
    let line = line.split('#').next().unwrap_or("");
    line.split([',', ' ', '\t']).map(str::trim).filter(|s| !s.is_empty()).collect()
}

pub fn parse(text: &str) -> Result<Ide> {
    let mut ide = Ide::default();
    let mut section: Option<String> = None;
    for raw in text.lines() {
        let f = fields(raw);
        if f.is_empty() {
            continue;
        }
        if f.len() == 1 {
            let word = f[0].to_ascii_lowercase();
            section = if word == "end" { None } else { Some(word) };
            continue;
        }
        let Some(sec) = section.as_deref() else { continue };
        match sec {
            "objs" | "tobj" | "anim" => {
                if let Some(def) = parse_object(sec, &f) {
                    ide.objects.push(def);
                }
            }
            "weap" => {
                if let Some(def) = parse_object(sec, &f) {
                    ide.weapons.push(def);
                }
            }
            "txdp" if f.len() >= 2 => {
                ide.txd_parents.push((f[0].to_ascii_lowercase(), f[1].to_ascii_lowercase()));
            }
            _ => {}
        }
    }
    Ok(ide)
}

fn parse_object(sec: &str, f: &[&str]) -> Option<ObjectDef> {
    let id = f.first()?.parse().ok()?;
    let model = f.get(1)?.to_ascii_lowercase();
    let txd = f.get(2)?.to_ascii_lowercase();
    // Strip trailing time window for tobj; anim has an extra anim name at [3].
    let (body, time) = if sec == "tobj" && f.len() >= 7 {
        let n = f.len();
        (&f[3..n - 2], Some((f[n - 2].parse().ok()?, f[n - 1].parse().ok()?)))
    } else if sec == "anim" || sec == "weap" {
        (f.get(4..)?, None)
    } else {
        (&f[3..], None)
    };
    // body is either [draw, flags] or [meshCount, draw1..drawN, flags].
    let (draw_distance, flags) = match body {
        [d, fl] => (d.parse().ok()?, fl.parse().ok()?),
        [_count, rest @ ..] if rest.len() >= 2 => {
            (rest[0].parse().ok()?, rest[rest.len() - 1].parse().ok()?)
        }
        _ => return None,
    };
    Some(ObjectDef { id, model, txd, draw_distance, flags, time })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_variants() {
        let ide = parse(
            "objs\n\
             620, veg_palm04, gta_tree_palm, 299, 2097156\n\
             621, x, y, 1, 150, 0\n\
             end\n\
             tobj\n\
             700, night, lights, 100, 0, 20, 6\n\
             end\n\
             txdp\n\
             a, b\n\
             end\n",
        )
        .unwrap();
        assert_eq!(ide.objects.len(), 3);
        assert_eq!(ide.objects[0].draw_distance, 299.0);
        assert_eq!(ide.objects[1].draw_distance, 150.0);
        assert_eq!(ide.objects[2].time, Some((20, 6)));
        assert_eq!(ide.txd_parents, vec![("a".into(), "b".into())]);
    }
}
