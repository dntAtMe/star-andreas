//! Cutscene data from `anim/cuts.img` (cutscene.md §3): the `.cut` description (origin,
//! actors and their anims, subtitles), the `.dat` flyby camera splines, and the flyby
//! evaluation of `CCam::Process_FlyBy` (§4).

/// One `.cut` actor line: `count, model, anim[, anim...]`.
#[derive(Debug, Clone)]
pub struct CutModel {
    pub model: String,
    pub anims: Vec<String>,
}

/// One subtitle: start / duration (ms) and the GXT key (upper-case, 7 chars).
#[derive(Debug, Clone)]
pub struct CutText {
    pub start_ms: u32,
    pub duration_ms: u32,
    pub key: String,
}

/// A parsed `.cut` file.
#[derive(Debug, Clone, Default)]
pub struct Cut {
    pub offset: [f32; 3],
    pub models: Vec<CutModel>,
    pub texts: Vec<CutText>,
    pub uncompress: Vec<String>,
    /// `extracol`: the first non-zero value (timecycle extra colour = n - 1).
    pub extra_colour: Option<u32>,
    /// `attach`: (parent object, child object, bone id).
    pub attach: Vec<(usize, usize, i32)>,
}

/// `.cut` parser (0x5B05A0): sections `info`, `model`, `text`, `uncompress`, `attach`,
/// `extracol` (… `end`); unknown sections (`motion`) are skipped.
pub fn parse_cut(text: &str) -> Cut {
    let mut cut = Cut::default();
    let mut state = "";
    for raw in text.split(['\n', '\r']) {
        let line = raw.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if line.is_empty() {
            continue;
        }
        if line == "end" {
            state = "";
            continue;
        }
        if state.is_empty() {
            state = match line {
                "info" | "model" | "text" | "uncompress" | "attach" | "remove" | "peffect" | "extracol" => line,
                _ => "skip",
            };
            if state == "skip" {
                // `motion` and friends: ignored until their `end`.
            }
            continue;
        }
        match state {
            "info" => {
                if let Some(rest) = line.strip_prefix("offset") {
                    let v: Vec<f32> = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                    if v.len() >= 3 {
                        cut.offset = [v[0], v[1], v[2]];
                    }
                }
            }
            "model" => {
                let toks: Vec<&str> = line.split([' ', ',']).filter(|t| !t.is_empty()).collect();
                if toks.len() >= 3 {
                    cut.models.push(CutModel { model: toks[1].to_ascii_lowercase(), anims: toks[2..].iter().map(|a| a.to_ascii_lowercase()).collect() });
                }
            }
            "text" => {
                let mut it = line.splitn(3, ',');
                let (Some(a), Some(b), Some(k)) = (it.next(), it.next(), it.next()) else { continue };
                if let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) {
                    let key: String = k.trim().to_ascii_uppercase().chars().take(7).collect();
                    cut.texts.push(CutText { start_ms: a, duration_ms: b, key });
                }
            }
            "uncompress" => {
                if let Some(t) = line.split([' ', ',']).find(|t| !t.is_empty()) {
                    cut.uncompress.push(t.to_ascii_lowercase());
                }
            }
            "attach" => {
                let v: Vec<i32> = line.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                if v.len() >= 3 {
                    cut.attach.push((v[0] as usize, v[1] as usize, v[2]));
                }
            }
            "extracol" => {
                if let Ok(n) = line.trim().parse::<u32>() {
                    if n != 0 && cut.extra_colour.is_none() {
                        cut.extra_colour = Some(n);
                    }
                }
            }
            _ => {}
        }
    }
    cut
}

/// The `.dat` flyby splines (§3.4), each as the game stores them: `arr[0] = n`, then `n`
/// keys of `stride` floats (time s, value, in-handle, out-handle).
#[derive(Debug, Clone, Default)]
pub struct CamSplines {
    pub fov: Vec<f32>,
    pub roll: Vec<f32>,
    pub pos: Vec<f32>,
    pub target: Vec<f32>,
}

/// `.dat` parser (CCamera::LoadPathSplines): FOV, ROLL, POSITION, TARGET blocks; each a
/// header line with the key count, the key lines, then `;`.
pub fn parse_dat(text: &str) -> CamSplines {
    let mut blocks: Vec<Vec<f32>> = Vec::new();
    let mut cur: Option<(usize, usize, Vec<f32>)> = None; // (keys left, stride, array)
    for raw in text.lines() {
        let line: String = raw.chars().map(|c| if (c as u32) < 0x20 || c == ',' { ' ' } else { c }).collect();
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match cur.as_mut() {
            None => {
                if blocks.len() >= 4 {
                    break;
                }
                let n: usize = line.split_whitespace().next().and_then(|t| t.parse().ok()).unwrap_or(0);
                let stride = if blocks.len() < 2 { 4 } else { 10 };
                cur = Some((n, stride, vec![n as f32]));
            }
            Some((left, _, arr)) => {
                if line.starts_with(';') {
                    blocks.push(std::mem::take(arr));
                    cur = None;
                    continue;
                }
                if *left > 0 {
                    for t in line.split_whitespace() {
                        let t = t.trim_end_matches('f');
                        if let Ok(v) = t.parse::<f32>() {
                            arr.push(v);
                        }
                    }
                    *left -= 1;
                }
            }
        }
    }
    let mut it = blocks.into_iter();
    CamSplines { fov: it.next().unwrap_or_default(), roll: it.next().unwrap_or_default(), pos: it.next().unwrap_or_default(), target: it.next().unwrap_or_default() }
}

/// `CCam::Process_FlyBy` state: its timer and the four spline markers.
#[derive(Debug, Clone, Default)]
pub struct Flyby {
    started: bool,
    timer_ms: f32,
    finish_ms: u32,
    pos_idx: usize,
    tgt_idx: usize,
    roll_idx: usize,
    fov_idx: usize,
    /// `m_fPositionAlongSpline`: 1.0 once finished.
    pub along: f32,
}

/// One camera sample: source and target (offset applied), roll (degrees), FOV.
#[derive(Debug, Clone, Copy)]
pub struct FlybyFrame {
    pub source: [f32; 3],
    pub target: [f32; 3],
    pub roll: f32,
    pub fov: f32,
}

fn find_vec(arr: &[f32], t: f32, idx: &mut usize, off: [f32; 3]) -> [f32; 3] {
    let n = arr.first().copied().unwrap_or(0.0) as usize;
    if n == 0 || arr.len() < 1 + n * 10 {
        return off;
    }
    let at = |i: usize| arr.get(i).copied().unwrap_or(0.0);
    let dur = (at(*idx) - at(idx.saturating_sub(10))) * 1000.0;
    let last = at(10 * n - 9) * 1000.0;
    if t < last {
        let clamped = n < (*idx - 1) / 10;
        if clamped {
            *idx = 10 * n - 9;
        }
        if dur <= 32.0 && !clamped {
            let old = *idx;
            *idx += 10;
            if n < (old + 9) / 10 {
                *idx = 10 * n - 9;
            }
        }
    }
    let i = *idx;
    // Quirk: divides by the OLD segment's duration.
    let mut u = (t - at(i.saturating_sub(10)) * 1000.0) / dur;
    u = if u <= 1.0 { if u < 0.0 || u.is_nan() { 0.0 } else { u } } else { 1.0 };
    if t > last {
        u = 1.0;
    }
    let v = |k: usize| [at(k), at(k + 1), at(k + 2)];
    let p0 = v(i - 9);
    let cout = v(i - 3);
    let p1 = v(i + 1);
    let cin = v(i + 4);
    let r = if cout == p0 {
        [0, 1, 2].map(|c| p0[c] + (p1[c] - p0[c]) * u)
    } else {
        let s = 1.0 - u;
        [0, 1, 2].map(|c| s * s * s * p0[c] + u * u * u * p1[c] + 3.0 * (u * s * s * cout[c] + s * u * u * cin[c]))
    };
    [r[0] + off[0], r[1] + off[1], r[2] + off[2]]
}

fn find_float(arr: &[f32], t: f32, idx: &mut usize) -> f32 {
    let n = arr.first().copied().unwrap_or(0.0) as usize;
    if n == 0 || arr.len() < 1 + n * 4 {
        return 0.0;
    }
    let at = |i: usize| arr.get(i).copied().unwrap_or(0.0);
    let dur0 = (at(*idx) - at(idx.saturating_sub(4))) * 1000.0;
    let last = at(4 * n - 3) * 1000.0;
    if t < last {
        let clamped = n < (*idx - 1) / 4;
        if clamped {
            *idx = 4 * n - 3;
        }
        if dur0 <= 32.0 && !clamped {
            let old = *idx;
            *idx += 4;
            if n < (old + 3) / 4 {
                *idx = 4 * n - 3;
            }
        }
    }
    let i = *idx;
    let mut u = (t - at(i - 4) * 1000.0) / ((at(i) - at(i - 4)) * 1000.0);
    u = if u <= 1.0 { if u < 0.0 || u.is_nan() { 0.0 } else { u } } else { 1.0 };
    if t > last {
        u = 1.0;
    }
    if at(i - 1) == at(i - 3) {
        at(i - 3) + (at(i + 1) - at(i - 3)) * u
    } else {
        let s = 1.0 - u;
        s * s * s * at(i - 3) + 3.0 * s * s * u * at(i - 1) + 3.0 * s * u * u * at(i + 2) + u * u * u * at(i + 1)
    }
}

impl Flyby {
    /// The finish time (`GetCutSceneFinishTime`), ms: the last POSITION key.
    pub fn finish_ms(sp: &CamSplines) -> u32 {
        let n = sp.pos.first().copied().unwrap_or(0.0) as usize;
        if n == 0 {
            return 0;
        }
        (sp.pos.get(1 + 10 * (n - 1)).copied().unwrap_or(0.0) * 1000.0) as u32
    }

    /// One `Process_FlyBy` frame; `dt_ms` = ts·0.02·1000 (0 on the first frame).
    pub fn step(&mut self, sp: &CamSplines, off: [f32; 3], dt_ms: f32) -> FlybyFrame {
        if !self.started {
            self.started = true;
            self.timer_ms = 0.0;
            self.finish_ms = Self::finish_ms(sp);
            self.pos_idx = 11;
            self.tgt_idx = 11;
            self.roll_idx = 5;
            self.fov_idx = 5;
        } else {
            self.timer_ms += dt_ms;
        }
        let t = self.timer_ms as u32 as f32;
        let n = |a: &Vec<f32>| a.first().copied().unwrap_or(0.0) as usize;
        if t >= self.finish_ms as f32 {
            self.pos_idx = (10 * n(&sp.pos)).saturating_sub(9).max(11);
            self.tgt_idx = (10 * n(&sp.target)).saturating_sub(9).max(11);
            self.roll_idx = (4 * n(&sp.roll)).saturating_sub(3).max(5);
            self.fov_idx = (4 * n(&sp.fov)).saturating_sub(3).max(5);
            self.along = 1.0;
        } else {
            self.along = if self.finish_ms > 0 { t / self.finish_ms as f32 } else { 1.0 };
            let adv = |arr: &Vec<f32>, idx: &mut usize, stride: usize| {
                while *idx < arr.len() && (arr[*idx] - arr[1]) * 1000.0 <= t {
                    *idx += stride;
                }
                // Keep the index on a key (the game reads past the end harmlessly).
                let last = 1 + stride * (n(arr).max(1) - 1);
                if *idx > last {
                    *idx = last;
                }
            };
            adv(&sp.pos, &mut self.pos_idx, 10);
            adv(&sp.target, &mut self.tgt_idx, 10);
            adv(&sp.roll, &mut self.roll_idx, 4);
            adv(&sp.fov, &mut self.fov_idx, 4);
        }
        let source = find_vec(&sp.pos, t, &mut self.pos_idx, off);
        let target = find_vec(&sp.target, t, &mut self.tgt_idx, off);
        let roll = find_float(&sp.roll, t, &mut self.roll_idx);
        let fov = find_float(&sp.fov, t, &mut self.fov_idx);
        FlybyFrame { source, target, roll, fov }
    }

    /// `CCamera::FinishCutscene` → `SetPercentAlongCutScene(100)`: the timer jumps to the end.
    pub fn finish(&mut self, sp: &CamSplines) {
        self.timer_ms = Self::finish_ms(sp) as f32;
    }

    pub fn timer_ms(&self) -> f32 {
        self.timer_ms
    }
}
