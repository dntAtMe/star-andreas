//! Ped decision makers (ped_events.md §2): `data/PedEvent.txt` (event type → decision index)
//! and the `data/decision/allowed/*.ped` response tables.

/// `CDecision` (0x3C): six responses with a probability per event source type and the
/// on-foot / in-car flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub task: [i32; 6],
    /// `prob[i][srcType]`, memory order: [0] other, [1] player, [2] friend, [3] enemy.
    pub prob: [[u8; 4]; 6],
    /// `flag[i][0]` on foot, `flag[i][1]` in car.
    pub flag: [[bool; 2]; 6],
}

impl Default for Decision {
    /// `CDecision::SetDefault` (0x600530).
    fn default() -> Self {
        Self { task: [-1; 6], prob: [[0; 4]; 6], flag: [[false; 2]; 6] }
    }
}

/// Number of decisions per decision maker.
pub const NUM_DECISIONS: usize = 41;

/// PedEvent.txt (0x5BB9F0): `eventToDecision[96]`, every non-empty `name type` line gives
/// `eventToDecision[type] = lineIndex`; unlisted types map to decision 0.
pub fn parse_ped_event_txt(text: &str) -> [u8; 96] {
    let mut out = [0u8; 96];
    let mut idx = 0u8;
    for raw in text.lines() {
        let mut it = raw.split_whitespace();
        let (Some(_name), Some(ty)) = (it.next(), it.next()) else { continue };
        let Ok(ty) = ty.parse::<usize>() else { continue };
        if let Some(slot) = out.get_mut(ty) {
            *slot = idx;
        }
        idx += 1;
    }
    out
}

/// `LoadDecisionMaker` (0x6076B0) + `CDecision::Set` (0x600570): the first line is skipped;
/// each line is `eventType, unused` then 6 × `task, p0, p1, p2, p3, f0, f1, x0..x5`.
/// Probabilities are truncated to u8 (500 → 244) and stored shuffled (`p3 p2 p0 p1`).
pub fn parse_decision_maker(text: &str, event_to_decision: &[u8; 96]) -> Vec<Decision> {
    let mut out = vec![Decision::default(); NUM_DECISIONS];
    for raw in text.lines().skip(1) {
        let v: Vec<f64> = raw.split(',').filter_map(|x| x.trim().parse::<f64>().ok()).collect();
        if v.len() < 2 + 6 * 13 {
            continue;
        }
        let ev = v[0] as usize;
        let mut d = Decision::default();
        for i in 0..6 {
            let r = &v[2 + i * 13..2 + (i + 1) * 13];
            let p = |k: usize| (r[1 + k] as i64) as u8;
            d.task[i] = r[0] as i32;
            d.prob[i] = [p(3), p(2), p(0), p(1)];
            d.flag[i] = [r[6] != 0.0, r[5] != 0.0];
        }
        let idx = event_to_decision.get(ev).copied().unwrap_or(0) as usize;
        if let Some(slot) = out.get_mut(idx) {
            *slot = d;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_line_shuffles_and_truncates() {
        let ev = parse_ped_event_txt("EVENT_SHOT_FIRED 15\nEVENT_X 20\n");
        assert_eq!(ev[15], 0);
        assert_eq!(ev[20], 1);
        let line = "15,4,911,5.0,70.0,50.0,500.0,0,1,0,0,0,0,0,0,415,0.0,0.0,0.0,50.0,0,1,0,0,0,0,0,0,\
                    -1,0,0,0,0,0,0,0,0,0,0,0,0,-1,0,0,0,0,0,0,0,0,0,0,0,0,-1,0,0,0,0,0,0,0,0,0,0,0,0,-1,0,0,0,0,0,0,0,0,0,0,0,0";
        let dm = parse_decision_maker(&format!("data values2:\n{line}\n"), &ev);
        let d = dm[0];
        assert_eq!(d.task[0], 911);
        // [other, player, friend, enemy] = [p3 (500 → 244), p2, p0, p1].
        assert_eq!(d.prob[0], [244, 50, 5, 70]);
        assert_eq!(d.flag[0], [true, false]);
        assert_eq!(d.task[2], -1);
    }
}
