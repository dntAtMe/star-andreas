//! `data/gta.dat` / `default.dat`: list of IDE/IPL/IMG files to load.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Img(String),
    Ide(String),
    Ipl(String),
}

/// Paths are returned as written (relative to the game root, `\` separators).
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|l| {
            let l = l.split('#').next()?.trim();
            let (kw, path) = l.split_once(char::is_whitespace)?;
            let path = path.trim().to_string();
            match kw.to_ascii_uppercase().as_str() {
                "IMG" => Some(Entry::Img(path)),
                "IDE" => Some(Entry::Ide(path)),
                "IPL" => Some(Entry::Ipl(path)),
                _ => None,
            }
        })
        .collect()
}
