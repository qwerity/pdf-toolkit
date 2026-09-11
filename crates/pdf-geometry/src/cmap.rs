//! Minimal `ToUnicode` CMap support.
//!
//! CAD exports embed subset fonts with `Identity-H` encoding, so the raw bytes
//! in a `Tj` string are meaningless without the font's `ToUnicode` stream. This
//! module parses the `bfchar`/`bfrange` sections of such a stream, which is all
//! that is needed to recover the drawing's labels.

use std::collections::HashMap;

/// Maps character codes to their Unicode expansion for one font.
#[derive(Debug, Clone, Default)]
pub struct ToUnicode {
    single: HashMap<u32, String>,
    /// Byte width of a character code; 2 for `Identity-H`, 1 for simple fonts.
    pub code_bytes: usize,
}

impl ToUnicode {
    pub fn identity(code_bytes: usize) -> Self {
        Self {
            single: HashMap::new(),
            code_bytes,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.single.is_empty()
    }

    /// Splits a PDF string into character codes and maps each through the CMap.
    /// Unmapped codes are dropped rather than replaced, so that garbage from
    /// outline-converted text does not pollute keyword matching.
    pub fn decode(&self, bytes: &[u8]) -> String {
        let mut out = String::new();
        let step = self.code_bytes.max(1);
        for chunk in bytes.chunks(step) {
            let mut code = 0u32;
            for b in chunk {
                code = (code << 8) | u32::from(*b);
            }
            if let Some(s) = self.single.get(&code) {
                out.push_str(s);
            } else if self.single.is_empty() && step == 1 && (0x20..0x7f).contains(&code) {
                out.push(code as u8 as char);
            }
        }
        out
    }

    /// Parses the `beginbfchar`/`beginbfrange` sections of a `ToUnicode` stream.
    pub fn parse(data: &[u8]) -> Self {
        let text = String::from_utf8_lossy(data);
        let mut map = ToUnicode {
            single: HashMap::new(),
            code_bytes: 2,
        };
        map.parse_bfchar(&text);
        map.parse_bfrange(&text);
        map
    }

    fn parse_bfchar(&mut self, text: &str) {
        for section in Self::sections(text, "beginbfchar", "endbfchar") {
            let tokens = Self::hex_tokens(section);
            for pair in tokens.chunks(2) {
                if let [src, dst] = pair
                    && let Some(code) = Self::hex_to_code(src)
                {
                    self.single.insert(code, Self::hex_to_string(dst));
                }
            }
        }
    }

    fn parse_bfrange(&mut self, text: &str) {
        for section in Self::sections(text, "beginbfrange", "endbfrange") {
            // Array-form destinations (`<lo> <hi> [<a> <b> ...]`) are rare in CAD
            // exports; only the scalar form is handled, and malformed triples are
            // skipped rather than aborting the whole font.
            let tokens = Self::hex_tokens(section);
            for triple in tokens.chunks(3) {
                let [lo, hi, dst] = triple else { continue };
                let (Some(lo), Some(hi)) = (Self::hex_to_code(lo), Self::hex_to_code(hi)) else {
                    continue;
                };
                if hi < lo || hi - lo > 0xFFFF {
                    continue;
                }
                let base = Self::hex_to_string(dst);
                let Some(base_char) = base.chars().next_back() else {
                    continue;
                };
                let prefix: String = base.chars().take(base.chars().count() - 1).collect();
                for (i, code) in (lo..=hi).enumerate() {
                    let ch = char::from_u32(base_char as u32 + i as u32).unwrap_or(base_char);
                    self.single.insert(code, format!("{prefix}{ch}"));
                }
            }
        }
    }

    fn sections<'a>(text: &'a str, start: &str, end: &str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut cursor = 0usize;
        while let Some(s) = text[cursor..].find(start) {
            let body_start = cursor + s + start.len();
            let Some(e) = text[body_start..].find(end) else {
                break;
            };
            out.push(&text[body_start..body_start + e]);
            cursor = body_start + e + end.len();
        }
        out
    }

    /// Extracts `<...>` hex literals in order, ignoring array brackets.
    fn hex_tokens(section: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let bytes = section.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                if let Some(close) = section[i + 1..].find('>') {
                    out.push(&section[i + 1..i + 1 + close]);
                    i += close + 2;
                    continue;
                }
                break;
            }
            i += 1;
        }
        out
    }

    fn hex_to_code(hex: &str) -> Option<u32> {
        u32::from_str_radix(hex.trim(), 16).ok()
    }

    /// `ToUnicode` destinations are UTF-16BE, possibly several code units long.
    fn hex_to_string(hex: &str) -> String {
        let hex = hex.trim();
        let units: Vec<u16> = hex
            .as_bytes()
            .chunks(4)
            .filter(|c| c.len() == 4)
            .filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
            .collect();
        String::from_utf16_lossy(&units)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bfchar() {
        let src = b"beginbfchar\n<0003> <0020>\n<001a> <0031>\nendbfchar";
        let m = ToUnicode::parse(src);
        assert_eq!(m.decode(&[0x00, 0x03, 0x00, 0x1a]), " 1");
    }

    #[test]
    fn parses_bfrange() {
        let src = b"beginbfrange\n<0010> <0013> <0041>\nendbfrange";
        let m = ToUnicode::parse(src);
        assert_eq!(m.decode(&[0x00, 0x10, 0x00, 0x13]), "AD");
    }

    #[test]
    fn skips_unmapped_codes() {
        let m = ToUnicode::parse(b"beginbfchar\n<0003> <0020>\nendbfchar");
        assert_eq!(m.decode(&[0xff, 0xff]), "");
    }
}
