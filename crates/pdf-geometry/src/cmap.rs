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
    /// Whether a `ToUnicode` STREAM produced this map.
    ///
    /// It is the question [`ToUnicode::decode`]'s fallback really asks, and it
    /// used to be answered by `single.is_empty()` -- which was the same answer
    /// until [`ToUnicode::add_differences`] existed. A font whose only
    /// statement is an `/Encoding /Differences` array naming a handful of
    /// accents has a non-empty map and still no stream, and reading the one
    /// for the other silently dropped every code the array did not name: a
    /// Word or Distiller PDF's whole text layer, since those name the accents
    /// and leave ASCII to the base encoding.
    from_stream: bool,
    /// Byte width of a character code; 2 for `Identity-H`, 1 for simple fonts.
    pub code_bytes: usize,
}

impl ToUnicode {
    pub fn identity(code_bytes: usize) -> Self {
        Self {
            single: HashMap::new(),
            from_stream: false,
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
            // The fallback is for a font that never said what its codes mean.
            // An empty parsed stream counts as not having said, which is how
            // this read before `from_stream` existed.
            let unstated = !self.from_stream || self.single.is_empty();
            if let Some(s) = self.single.get(&code) {
                out.push_str(s);
            } else if unstated && step == 1 && (0x20..0x7f).contains(&code) {
                out.push(code as u8 as char);
            } else if unstated
                && step == 2
                && let Some(ch) = unicode_identity_char(code)
            {
                out.push(ch);
            }
        }
        out
    }

    /// Adds what an `/Encoding /Differences` array names, for codes the
    /// `ToUnicode` stream did not already cover.
    ///
    /// A simple font may carry no `ToUnicode` at all and say what its codes
    /// mean only here. `ToUnicode` stays authoritative where both speak, as
    /// the specification requires; this fills the gaps.
    pub(crate) fn add_differences(&mut self, named: impl IntoIterator<Item = (u32, String)>) {
        for (code, name) in named {
            if self.single.contains_key(&code) {
                continue;
            }
            if let Some(c) = glyph_name_char(&name) {
                self.single.insert(code, c.to_string());
            }
        }
    }

    /// Parses the `beginbfchar`/`beginbfrange` sections of a `ToUnicode` stream.
    pub fn parse(data: &[u8]) -> Self {
        let text = String::from_utf8_lossy(data);
        let mut map = ToUnicode {
            single: HashMap::new(),
            from_stream: true,
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

/// The Latin-1 characters the Adobe standard glyph names stand for.
///
/// A simple font says what its codes mean through `/Encoding /Differences`,
/// an array of PostScript glyph names. Only the WinAnsi/Latin-1 repertoire is
/// listed, which is what a `/BaseEncoding /WinAnsiEncoding` font can name;
/// a single ASCII letter names itself and is handled without a table entry.
const LATIN1_GLYPH_NAMES: &[(&str, char)] = &[
    ("space", ' '),
    ("exclam", '!'),
    ("quotedbl", '"'),
    ("numbersign", '#'),
    ("dollar", '$'),
    ("percent", '%'),
    ("ampersand", '&'),
    ("quotesingle", '\''),
    ("parenleft", '('),
    ("parenright", ')'),
    ("asterisk", '*'),
    ("plus", '+'),
    ("comma", ','),
    ("hyphen", '-'),
    ("period", '.'),
    ("slash", '/'),
    ("zero", '0'),
    ("one", '1'),
    ("two", '2'),
    ("three", '3'),
    ("four", '4'),
    ("five", '5'),
    ("six", '6'),
    ("seven", '7'),
    ("eight", '8'),
    ("nine", '9'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less", '<'),
    ("equal", '='),
    ("greater", '>'),
    ("question", '?'),
    ("at", '@'),
    ("bracketleft", '['),
    ("backslash", '\\'),
    ("bracketright", ']'),
    ("asciicircum", '^'),
    ("underscore", '_'),
    ("grave", '`'),
    ("braceleft", '{'),
    ("bar", '|'),
    ("braceright", '}'),
    ("asciitilde", '~'),
    ("exclamdown", '\u{a1}'),
    ("cent", '\u{a2}'),
    ("sterling", '\u{a3}'),
    ("currency", '\u{a4}'),
    ("yen", '\u{a5}'),
    ("brokenbar", '\u{a6}'),
    ("section", '\u{a7}'),
    ("dieresis", '\u{a8}'),
    ("copyright", '\u{a9}'),
    ("ordfeminine", '\u{aa}'),
    ("guillemotleft", '\u{ab}'),
    ("logicalnot", '\u{ac}'),
    ("registered", '\u{ae}'),
    ("macron", '\u{af}'),
    ("degree", '\u{b0}'),
    ("plusminus", '\u{b1}'),
    ("twosuperior", '\u{b2}'),
    ("threesuperior", '\u{b3}'),
    ("acute", '\u{b4}'),
    ("mu", '\u{b5}'),
    ("paragraph", '\u{b6}'),
    ("periodcentered", '\u{b7}'),
    ("cedilla", '\u{b8}'),
    ("onesuperior", '\u{b9}'),
    ("ordmasculine", '\u{ba}'),
    ("guillemotright", '\u{bb}'),
    ("onequarter", '\u{bc}'),
    ("onehalf", '\u{bd}'),
    ("threequarters", '\u{be}'),
    ("questiondown", '\u{bf}'),
    ("Agrave", '\u{c0}'),
    ("Aacute", '\u{c1}'),
    ("Acircumflex", '\u{c2}'),
    ("Atilde", '\u{c3}'),
    ("Adieresis", '\u{c4}'),
    ("Aring", '\u{c5}'),
    ("AE", '\u{c6}'),
    ("Ccedilla", '\u{c7}'),
    ("Egrave", '\u{c8}'),
    ("Eacute", '\u{c9}'),
    ("Ecircumflex", '\u{ca}'),
    ("Edieresis", '\u{cb}'),
    ("Igrave", '\u{cc}'),
    ("Iacute", '\u{cd}'),
    ("Icircumflex", '\u{ce}'),
    ("Idieresis", '\u{cf}'),
    ("Eth", '\u{d0}'),
    ("Ntilde", '\u{d1}'),
    ("Ograve", '\u{d2}'),
    ("Oacute", '\u{d3}'),
    ("Ocircumflex", '\u{d4}'),
    ("Otilde", '\u{d5}'),
    ("Odieresis", '\u{d6}'),
    ("multiply", '\u{d7}'),
    ("Oslash", '\u{d8}'),
    ("Ugrave", '\u{d9}'),
    ("Uacute", '\u{da}'),
    ("Ucircumflex", '\u{db}'),
    ("Udieresis", '\u{dc}'),
    ("Yacute", '\u{dd}'),
    ("Thorn", '\u{de}'),
    ("germandbls", '\u{df}'),
    ("agrave", '\u{e0}'),
    ("aacute", '\u{e1}'),
    ("acircumflex", '\u{e2}'),
    ("atilde", '\u{e3}'),
    ("adieresis", '\u{e4}'),
    ("aring", '\u{e5}'),
    ("ae", '\u{e6}'),
    ("ccedilla", '\u{e7}'),
    ("egrave", '\u{e8}'),
    ("eacute", '\u{e9}'),
    ("ecircumflex", '\u{ea}'),
    ("edieresis", '\u{eb}'),
    ("igrave", '\u{ec}'),
    ("iacute", '\u{ed}'),
    ("icircumflex", '\u{ee}'),
    ("idieresis", '\u{ef}'),
    ("eth", '\u{f0}'),
    ("ntilde", '\u{f1}'),
    ("ograve", '\u{f2}'),
    ("oacute", '\u{f3}'),
    ("ocircumflex", '\u{f4}'),
    ("otilde", '\u{f5}'),
    ("odieresis", '\u{f6}'),
    ("divide", '\u{f7}'),
    ("oslash", '\u{f8}'),
    ("ugrave", '\u{f9}'),
    ("uacute", '\u{fa}'),
    ("ucircumflex", '\u{fb}'),
    ("udieresis", '\u{fc}'),
    ("yacute", '\u{fd}'),
    ("thorn", '\u{fe}'),
    ("ydieresis", '\u{ff}'),
    // The WinAnsi additions in `0x80..=0x9f` that carry meaning in prose.
    ("quoteleft", '\u{2018}'),
    ("quoteright", '\u{2019}'),
    ("quotedblleft", '\u{201c}'),
    ("quotedblright", '\u{201d}'),
    ("quotesinglbase", '\u{201a}'),
    ("quotedblbase", '\u{201e}'),
    ("bullet", '\u{2022}'),
    ("endash", '\u{2013}'),
    ("emdash", '\u{2014}'),
    ("ellipsis", '\u{2026}'),
    ("dagger", '\u{2020}'),
    ("daggerdbl", '\u{2021}'),
    ("perthousand", '\u{2030}'),
    ("trademark", '\u{2122}'),
    ("Euro", '\u{20ac}'),
    ("guilsinglleft", '\u{2039}'),
    ("guilsinglright", '\u{203a}'),
    ("OE", '\u{152}'),
    ("oe", '\u{153}'),
    ("Scaron", '\u{160}'),
    ("scaron", '\u{161}'),
    ("Zcaron", '\u{17d}'),
    ("zcaron", '\u{17e}'),
    ("Ydieresis", '\u{178}'),
    ("florin", '\u{192}'),
    ("circumflex", '\u{2c6}'),
    ("tilde", '\u{2dc}'),
];

/// The character a PostScript glyph name stands for.
///
/// `uniXXXX` and `uXXXX` say so outright; a single ASCII letter names itself;
/// otherwise the Latin-1 table decides. A name outside all three — a subset
/// font's `g37`, or a name from a script this does not list — yields nothing,
/// which drops the code rather than inventing a letter for it.
fn glyph_name_char(name: &str) -> Option<char> {
    let hex_char = |h: &str, exact: Option<usize>| -> Option<char> {
        if exact.is_some_and(|n| h.len() != n) || h.is_empty() || h.len() > 6 {
            return None;
        }
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        char::from_u32(u32::from_str_radix(h, 16).ok()?)
    };
    if let Some(h) = name.strip_prefix("uni")
        && let Some(c) = hex_char(h, Some(4))
    {
        return Some(c);
    }
    if let Some(h) = name.strip_prefix('u')
        && h.len() >= 4
        && let Some(c) = hex_char(h, None)
    {
        return Some(c);
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_alphabetic()
    {
        return Some(c);
    }
    LATIN1_GLYPH_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
}

/// The character a two-byte `Identity-H` code stands for when the exporter
/// used Unicode code points as CIDs and wrote no `ToUnicode` map — what
/// ArchiCAD's PDF export does. Only the scripts a drawing sheet can carry
/// (Latin, Cyrillic, Armenian, Greek, common punctuation and symbols) are
/// accepted, so a font whose CIDs are real glyph indices yields little
/// rather than a page of stray letters.
fn unicode_identity_char(code: u32) -> Option<char> {
    let ch = char::from_u32(code)?;
    let accepted = matches!(code,
        0x20..=0x7e          // ASCII
        | 0xa0..=0x24f       // Latin-1 supplement and extended
        | 0x370..=0x3ff      // Greek
        | 0x400..=0x4ff      // Cyrillic
        | 0x530..=0x58f      // Armenian
        | 0x2010..=0x2027    // dashes, quotes, bullets
        | 0x2030..=0x205e    // per mille, primes
        | 0x20a0..=0x20cf    // currency
        | 0x2100..=0x214f    // letterlike (°C, №)
        | 0x2190..=0x21ff    // arrows
        | 0x2200..=0x22ff); // mathematical operators
    accepted.then_some(ch)
}

#[cfg(test)]
mod tests {
    /// ArchiCAD exports `Identity-H` fonts with no `ToUnicode` map whose
    /// CIDs are Unicode code points; the fallback reads them as such, and
    /// only for scripts a drawing sheet carries.
    #[test]
    fn an_identity_font_without_a_map_decodes_unicode_cids() {
        let map = ToUnicode::identity(2);
        // "Փուլ :" as two-byte code points, then an ASCII colon.
        let bytes = [
            0x05, 0x53, 0x05, 0x78, 0x05, 0x82, 0x05, 0x6c, 0x00, 0x20, 0x00, 0x3a,
        ];
        assert_eq!(map.decode(&bytes), "Փուլ :");
        // A glyph-indexed font's small ids fall outside every accepted block
        // except ASCII; control codes and private-use codes yield nothing.
        assert_eq!(map.decode(&[0x00, 0x03, 0xe0, 0x00, 0x00, 0x41]), "A");
    }

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
    fn reads_glyph_names() {
        assert_eq!(glyph_name_char("twosuperior"), Some('\u{b2}'));
        assert_eq!(glyph_name_char("Oslash"), Some('\u{d8}'));
        assert_eq!(glyph_name_char("uni0533"), Some('Գ'));
        assert_eq!(glyph_name_char("u0561"), Some('ա'));
        assert_eq!(glyph_name_char("A"), Some('A'));
        assert_eq!(glyph_name_char("seven"), Some('7'));
        // A subset font's own glyph ids name no character.
        assert_eq!(glyph_name_char("g37"), None);
        assert_eq!(glyph_name_char("union"), None);
    }

    /// The legacy Armenian case: the font draws Armenian but every part of
    /// the PDF that faces Unicode describes the Latin-1 character whose byte
    /// the letter sits on. Decoding recovers exactly that; reading the
    /// letters back off it is the page's job, not the font's
    /// (`interpreter::repair_legacy_armenian`).
    #[test]
    fn a_font_that_spells_armenian_in_latin1_decodes_to_latin1() {
        let mut m = ToUnicode::identity(1);
        // `/Differences [1 /paragraph /Egrave /Ecircumflex /twosuperior
        //   /igrave /agrave /eth /space /Oslash /space /one /colon]`
        let names = [
            "paragraph",
            "Egrave",
            "Ecircumflex",
            "twosuperior",
            "igrave",
            "agrave",
            "eth",
            "space",
            "Oslash",
            "space",
            "one",
            "colon",
        ];
        m.add_differences(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (i as u32 + 1, (*n).to_string())),
        );
        let codes: Vec<u8> = (1..=12).collect();
        assert_eq!(m.decode(&codes), "¶ÈÊ²ìàð Ø 1:");
    }

    /// ...and an ordinary Latin font decodes as itself, though its accented
    /// letters sit in the very same band: `é` is `0xE9`, `ü` is `0xFC`.
    #[test]
    fn an_accented_latin_font_is_left_alone() {
        let mut m = ToUnicode::identity(1);
        let names = [
            "e",
            "acute",
            "eacute",
            "u",
            "udieresis",
            "agrave",
            "c",
            "ccedilla",
            "o",
            "n",
            "t",
            "egrave",
        ];
        m.add_differences(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (i as u32 + 1, (*n).to_string())),
        );
        assert_eq!(m.decode(&[1, 3, 5, 12]), "eéüè");
    }

    /// A PARTIAL `/Differences` ARRAY MUST NOT SWALLOW THE REST OF THE FONT.
    ///
    /// Word and Distiller name a font's accents and leave ASCII to the base
    /// encoding, so the map ends up holding `0x80..` and nothing else. The
    /// ASCII fallback used to be gated on the map being empty, which such a
    /// font's map is not -- and the whole text layer of every page set in it
    /// decoded to nothing.
    #[test]
    fn codes_a_differences_array_does_not_name_still_decode_as_ascii() {
        let mut m = ToUnicode::identity(1);
        m.add_differences([(0xE9, "eacute".to_string()), (0x80, "Euro".to_string())]);
        assert_eq!(m.decode(b"Cafe 123"), "Cafe 123");
        assert_eq!(m.decode(&[b'C', b'a', b'f', 0xE9]), "Caf\u{e9}");
    }

    /// `ToUnicode` stays authoritative where a font has both.
    #[test]
    fn differences_fill_gaps_rather_than_overriding() {
        let mut m = ToUnicode::parse(b"beginbfchar\n<0041> <0391>\nendbfchar");
        m.code_bytes = 1;
        m.add_differences([(0x41, "A".to_string()), (0x42, "B".to_string())]);
        assert_eq!(m.decode(&[0x41, 0x42]), "ΑB");
    }

    #[test]
    fn skips_unmapped_codes() {
        let m = ToUnicode::parse(b"beginbfchar\n<0003> <0020>\nendbfchar");
        assert_eq!(m.decode(&[0xff, 0xff]), "");
    }
}
