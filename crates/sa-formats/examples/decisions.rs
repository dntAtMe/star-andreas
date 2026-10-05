//! Print a decision maker's responses: decisions <file.ped> ["<game dir>"]
fn main() -> anyhow::Result<()> {
    let file = std::env::args().nth(1).unwrap_or_else(|| "R_Norm.ped".into());
    let root = std::path::PathBuf::from(std::env::args().nth(2).unwrap_or_else(|| {
        r"G:\Programy\Steam\steamapps\common\Grand Theft Auto San Andreas".into()
    }));
    let ev = sa_formats::decision::parse_ped_event_txt(&std::fs::read_to_string(root.join("data/decision/PedEvent.txt"))?);
    let dm = sa_formats::decision::parse_decision_maker(&std::fs::read_to_string(root.join("data/decision/allowed").join(&file))?, &ev);
    for (ty, &d) in ev.iter().enumerate() {
        if d == 0 && ty != 7 {
            continue;
        }
        let dec = &dm[d as usize];
        let rows: Vec<String> = (0..6)
            .filter(|&i| dec.task[i] != -1 && dec.prob[i] != [0; 4])
            .map(|i| format!("{}({:?} F{} C{})", dec.task[i], dec.prob[i], dec.flag[i][0] as u8, dec.flag[i][1] as u8))
            .collect();
        if !rows.is_empty() {
            println!("{ty:3} -> {d:2}: {}", rows.join(" "));
        }
    }
    Ok(())
}
