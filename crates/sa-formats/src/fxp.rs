//! `models/effects.fxp`: the particle FX project (FxTools text format, version 109).
//!
//! The file is a flat list of `KEY: value` lines. Systems hold emitter prims, prims hold
//! "infos" (behaviours), and every info field is a keyframed curve (`FX_INTERP_DATA`).
//! This parser keeps the data as written; meaning and units belong to the FX runtime.

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct Project {
    pub systems: Vec<SystemBp>,
}

impl Project {
    pub fn system(&self, name: &str) -> Option<&SystemBp> {
        self.systems.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone)]
pub struct SystemBp {
    pub version: u32,
    pub name: String,
    pub length: f32,
    pub loop_interval_min: f32,
    /// The second `LENGTH:` line of the header.
    pub loop_length: f32,
    pub play_mode: u32,
    pub cull_dist: f32,
    pub bounding_sphere: [f32; 4],
    pub prims: Vec<PrimBp>,
    pub omit_textures: bool,
    /// `NOTXDSET` means the shared effects TXD.
    pub txd_name: String,
}

#[derive(Debug, Clone)]
pub struct PrimBp {
    pub name: String,
    /// Rotation rows (right, forward, up) then position, as in the file.
    pub matrix: [f32; 12],
    /// Up to four texture names; `NULL` entries are `None`.
    pub textures: [Option<String>; 4],
    pub alpha_on: bool,
    pub src_blend: u32,
    pub dst_blend: u32,
    pub infos: Vec<Info>,
    /// Emitter LOD distances (`LODSTART`/`LODEND`, after the infos).
    pub lod_start: f32,
    pub lod_end: f32,
}

impl PrimBp {
    pub fn info(&self, kind: &str) -> Option<&Info> {
        self.infos.iter().find(|i| i.kind == kind)
    }
}

#[derive(Debug, Clone)]
pub struct Info {
    /// Type tag between `FX_INFO_` and `_DATA`, e.g. `EMRATE`, `COLOUR`.
    pub kind: String,
    /// `TIMEMODEPRT`: curves run on particle life rather than system time.
    pub time_mode_prt: Option<bool>,
    pub fields: Vec<(String, Interp)>,
}

impl Info {
    pub fn field(&self, name: &str) -> Option<&Interp> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, i)| i)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Interp {
    pub looped: bool,
    /// (time, value) keys in file order.
    pub keys: Vec<(f32, f32)>,
}

struct Lines<'a> {
    lines: Vec<(usize, &'a str)>,
    pos: usize,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let lines = text
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        Self { lines, pos: 0 }
    }

    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.pos).map(|l| l.1)
    }

    fn peek_key(&self) -> Option<&'a str> {
        self.peek().map(|l| l.split_once(':').map_or(l, |(k, _)| k))
    }

    fn line_no(&self) -> usize {
        self.lines.get(self.pos).map_or(0, |l| l.0)
    }

    fn next(&mut self) -> Result<&'a str> {
        let l = self.peek().context("unexpected end of file")?;
        self.pos += 1;
        Ok(l)
    }

    /// Next line must be `key:`; returns its trimmed value.
    fn kv(&mut self, key: &str) -> Result<&'a str> {
        let at = self.line_no();
        let l = self.next()?;
        match l.split_once(':') {
            Some((k, v)) if k == key => Ok(v.trim()),
            _ => bail!("line {at}: expected {key}:, got {l:?}"),
        }
    }

    fn f32(&mut self, key: &str) -> Result<f32> {
        let at = self.line_no();
        let v = self.kv(key)?;
        v.parse().with_context(|| format!("line {at}: bad number {v:?}"))
    }

    fn u32(&mut self, key: &str) -> Result<u32> {
        Ok(self.f32(key)? as u32)
    }

    fn floats<const N: usize>(&mut self, key: &str) -> Result<[f32; N]> {
        let at = self.line_no();
        let v = self.kv(key)?;
        let mut out = [0.0; N];
        let mut it = v.split_whitespace();
        for o in &mut out {
            *o = it
                .next()
                .and_then(|s| s.parse().ok())
                .with_context(|| format!("line {at}: expected {N} numbers in {v:?}"))?;
        }
        Ok(out)
    }
}

pub fn parse(text: &str) -> Result<Project> {
    let mut l = Lines::new(text);
    l.kv("FX_PROJECT_DATA")?;
    let mut systems = Vec::new();
    loop {
        match l.peek_key() {
            Some("FX_SYSTEM_DATA") => systems.push(system(&mut l)?),
            Some("FX_PROJECT_DATA_END") | None => break,
            Some(_) => bail!("line {}: unexpected {:?}", l.line_no(), l.peek()),
        }
    }
    Ok(Project { systems })
}

fn system(l: &mut Lines) -> Result<SystemBp> {
    l.kv("FX_SYSTEM_DATA")?;
    let version: u32 = l.next()?.parse().context("system version")?;
    l.kv("FILENAME")?;
    let name = l.kv("NAME")?.to_string();
    let length = l.f32("LENGTH")?;
    let loop_interval_min = l.f32("LOOPINTERVALMIN")?;
    let loop_length = l.f32("LENGTH")?;
    let play_mode = l.u32("PLAYMODE")?;
    let cull_dist = l.f32("CULLDIST")?;
    let bounding_sphere = l.floats::<4>("BOUNDINGSPHERE")?;
    let n = l.u32("NUM_PRIMS")?;
    let prims = (0..n).map(|_| prim(l)).collect::<Result<Vec<_>>>().with_context(|| format!("system {name}"))?;
    Ok(SystemBp {
        version,
        name,
        length,
        loop_interval_min,
        loop_length,
        play_mode,
        cull_dist,
        bounding_sphere,
        prims,
        omit_textures: l.u32("OMITTEXTURES")? != 0,
        txd_name: l.kv("TXDNAME")?.to_string(),
    })
}

fn prim(l: &mut Lines) -> Result<PrimBp> {
    l.kv("FX_PRIM_EMITTER_DATA")?;
    l.kv("FX_PRIM_BASE_DATA")?;
    let name = l.kv("NAME")?.to_string();
    let matrix = l.floats::<12>("MATRIX")?;
    let mut textures: [Option<String>; 4] = Default::default();
    for (i, key) in ["TEXTURE", "TEXTURE2", "TEXTURE3", "TEXTURE4"].into_iter().enumerate() {
        let t = l.kv(key)?;
        textures[i] = (t != "NULL").then(|| t.to_string());
    }
    let alpha_on = l.u32("ALPHAON")? != 0;
    let src_blend = l.u32("SRCBLENDID")?;
    let dst_blend = l.u32("DSTBLENDID")?;
    let n = l.u32("NUM_INFOS")?;
    let infos = (0..n).map(|_| info(l)).collect::<Result<Vec<_>>>().with_context(|| format!("prim {name}"))?;
    Ok(PrimBp {
        name,
        matrix,
        textures,
        alpha_on,
        src_blend,
        dst_blend,
        infos,
        lod_start: l.f32("LODSTART")?,
        lod_end: l.f32("LODEND")?,
    })
}

fn info(l: &mut Lines) -> Result<Info> {
    let at = l.line_no();
    let head = l.next()?;
    let Some(kind) = head.strip_prefix("FX_INFO_").and_then(|s| s.strip_suffix("_DATA:")) else {
        bail!("line {at}: expected FX_INFO_*_DATA:, got {head:?}");
    };
    let mut out = Info { kind: kind.to_string(), time_mode_prt: None, fields: Vec::new() };
    if l.peek_key() == Some("TIMEMODEPRT") {
        out.time_mode_prt = Some(l.u32("TIMEMODEPRT")? != 0);
    }
    // Curve fields: a bare `NAME:` line followed by FX_INTERP_DATA.
    while let Some(line) = l.peek() {
        let Some(field) = line.strip_suffix(':') else { break };
        if field.starts_with("FX_") {
            break;
        }
        l.pos += 1;
        out.fields.push((field.to_string(), interp(l).with_context(|| format!("{kind}.{field}"))?));
    }
    Ok(out)
}

fn interp(l: &mut Lines) -> Result<Interp> {
    l.kv("FX_INTERP_DATA")?;
    let looped = l.u32("LOOPED")? != 0;
    let n = l.u32("NUM_KEYS")?;
    let mut keys = Vec::with_capacity(n as usize);
    for _ in 0..n {
        l.kv("FX_KEYFLOAT_DATA")?;
        keys.push((l.f32("TIME")?, l.f32("VAL")?));
    }
    Ok(Interp { looped, keys })
}

#[cfg(test)]
mod tests {
    const SAMPLE: &str = "FX_PROJECT_DATA:\n\nFX_SYSTEM_DATA:\n109\n\nFILENAME: x.fxs\nNAME: test\nLENGTH: 1.000\n\
        LOOPINTERVALMIN: 0.000\nLENGTH: 0.000\nPLAYMODE: 2\nCULLDIST: 20.000\nBOUNDINGSPHERE: 0.0 0.0 0.5 0.7\nNUM_PRIMS: 1\n\
        FX_PRIM_EMITTER_DATA:\n\nFX_PRIM_BASE_DATA:\nNAME: Smoke\n\
        MATRIX: 1 0 0 0 1 0 0 0 1 0 0 0 \nTEXTURE: bullethitsmoke\nTEXTURE2: NULL\nTEXTURE3: NULL\nTEXTURE4: NULL\n\
        ALPHAON: 1\nSRCBLENDID: 4\nDSTBLENDID: 5\n\nNUM_INFOS: 2\n\
        FX_INFO_EMRATE_DATA:\nRATE:\nFX_INTERP_DATA:\nLOOPED: 0\nNUM_KEYS: 2\nFX_KEYFLOAT_DATA:\nTIME: 0.000\nVAL: 0.000\n\
        FX_KEYFLOAT_DATA:\nTIME: 0.500\nVAL: 60.000\n\n\
        FX_INFO_SELFLIT_DATA:\nTIMEMODEPRT: 1\n\n\
        LODSTART: 8.000\nLODEND: 20.000\nOMITTEXTURES: 0\nTXDNAME: NOTXDSET\n\nFX_PROJECT_DATA_END:\n";

    #[test]
    fn parses_sample() {
        let p = super::parse(SAMPLE).unwrap();
        let s = p.system("test").unwrap();
        assert_eq!(s.version, 109);
        assert_eq!(s.prims[0].lod_end, 20.0);
        let prim = &s.prims[0];
        assert_eq!(prim.textures[0].as_deref(), Some("bullethitsmoke"));
        assert!(prim.textures[1].is_none());
        let rate = prim.info("EMRATE").unwrap().field("RATE").unwrap();
        assert_eq!(rate.keys, vec![(0.0, 0.0), (0.5, 60.0)]);
        assert_eq!(prim.info("SELFLIT").unwrap().time_mode_prt, Some(true));
    }
}
