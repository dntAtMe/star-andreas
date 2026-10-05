//! `data/fonts.dat` (`CFont::LoadFontValues` 0x7187C0, font.md §1.2): per font texture a
//! 210-byte record: 208 proportional glyph widths, the replacement-space value at index 208
//! and the unproportional width at 209.

/// One fonts.dat record.
#[derive(Debug, Clone)]
pub struct FontValues {
    /// `[0..207]` proportional widths, `[208]` replacement space, `[209]` unproportional width.
    pub record: [u8; 210],
}

impl Default for FontValues {
    fn default() -> Self {
        Self { record: [0; 210] }
    }
}

impl FontValues {
    pub fn prop(&self, idx: usize) -> u8 {
        self.record.get(idx).copied().unwrap_or(0)
    }

    pub fn unprop(&self) -> u8 {
        self.record[209]
    }
}

/// Parse fonts.dat into the records by font id (texture id).
pub fn parse_fonts_dat(text: &str) -> Vec<FontValues> {
    let mut out: Vec<FontValues> = Vec::new();
    let mut id = 0usize;
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    let num = |l: Option<&str>| l.and_then(|l| l.split_whitespace().next()).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
    while let Some(l) = lines.next() {
        let key = l.split_whitespace().next().unwrap_or("");
        let rec = |out: &mut Vec<FontValues>, id: usize| {
            while out.len() <= id {
                out.push(FontValues::default());
            }
        };
        match key {
            "[TOTAL_FONTS]" => {
                lines.next();
            }
            "[FONT_ID]" => id = num(lines.next()) as usize,
            "[REPLACEMENT_SPACE_CHAR]" => {
                rec(&mut out, id);
                out[id].record[208] = num(lines.next()) as u8;
            }
            "[PROP]" => {
                rec(&mut out, id);
                for row in 0..26 {
                    let Some(l) = lines.next() else { break };
                    for (k, v) in l.split_whitespace().take(8).enumerate() {
                        if let Ok(v) = v.parse::<i64>() {
                            out[id].record[row * 8 + k] = v as u8;
                        }
                    }
                }
            }
            "[UNPROP]" => {
                rec(&mut out, id);
                out[id].record[209] = num(lines.next()) as u8;
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn records() {
        let t = "[TOTAL_FONTS]\n2\n[FONT_ID]\n0\n[PROP]\n12 13 13 28 28 28 28 8 # 0\n".to_string()
            + &"1 1 1 1 1 1 1 1\n".repeat(25)
            + "[UNPROP]\n27\n[REPLACEMENT_SPACE_CHAR]\n10\n";
        let f = super::parse_fonts_dat(&t);
        assert_eq!(f[0].prop(0), 12);
        assert_eq!(f[0].prop(7), 8);
        assert_eq!(f[0].prop(207), 1);
        assert_eq!(f[0].prop(208), 10);
        assert_eq!(f[0].unprop(), 27);
    }
}
