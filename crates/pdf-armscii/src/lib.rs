//! Armenian written on Latin-1 code points, and how to read it back.
//!
//! Older Armenian CAD fonts — `ArialArmenian`, `ArialLatArm`, the
//! `ArTarumian` family — carry the alphabet on the ArmSCII-8 bytes and leave
//! every Unicode-facing part of the PDF describing the Latin-1 character that
//! byte happens to be. `ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ` reaches a reader as
//! `¶ÈÊ²ìàð Ð²î²Î²¶ÆÌ`, or as glyph names `paragraph`, `Egrave`,
//! `Ecircumflex`, `twosuperior`.
//!
//! **Nothing in the file says the letter.** On this project's corpus the
//! embedded program's `post` table is version 3.0 with the glyph names
//! stripped and its `cmap` is a `(3, 0)` symbol table, so the font identifies
//! its glyphs no better than the text does. The code page is the only thing
//! left that can supply the letter, and it supplies it exactly.

/// ArmSCII-8 lays the alphabet over `0xB2..=0xFD`, the two cases interleaved:
/// `0xB2` is `Ա` and `0xB3` `ա`, `0xB4` `Բ` and `0xB5` `բ`, up to `0xFC` `Ֆ`
/// and `0xFD` `ֆ`.
const FIRST: u32 = 0xB2;
const LAST: u32 = 0xFD;

/// Fewest DISTINCT band characters before a text is judged Armenian rather
/// than Latin.
///
/// Distinct, not total, because the band's most common visitor by far is a
/// single character used correctly: `5,98 մ²` is square metres, and a page of
/// areas carries dozens of `²` and nothing else from the band. Armenian
/// written on these bytes is words, and words need letters -- the shortest
/// sheet name in this corpus, `¶ÈÊ²ìàð Ð²î²Î²¶ÆÌ`, already spends eleven.
const MIN_DISTINCT_BAND: usize = 8;

/// The Armenian letter a Latin-1 character stands for, when it is really an
/// ArmSCII-8 byte.
pub fn letter(c: char) -> Option<char> {
    let byte = u32::from(c);
    if !(FIRST..=LAST).contains(&byte) {
        return None;
    }
    let offset = byte - FIRST;
    let base = if offset.is_multiple_of(2) {
        0x531
    } else {
        0x561
    };
    char::from_u32(base + offset / 2)
}

/// What the test below counts.
#[derive(Default)]
pub struct Tally {
    /// Characters that would become Armenian letters.
    band: usize,
    /// Which of them, as a bit per offset into the band.
    seen: u128,
    /// Armenian letters already written as such.
    armenian: usize,
    /// ASCII letters, which any Latin alphabet uses and Armenian does not.
    latin: usize,
}

impl Tally {
    pub fn of(chars: impl Iterator<Item = char>) -> Self {
        let mut t = Self::default();
        for c in chars {
            if letter(c).is_some() {
                t.band += 1;
                t.seen |= 1u128 << (u32::from(c) - FIRST);
            } else if matches!(u32::from(c), 0x531..=0x58f) {
                t.armenian += 1;
            } else if c.is_ascii_alphabetic() {
                t.latin += 1;
            }
        }
        t
    }

    /// Whether this text carries Armenian spelled in Latin-1 anywhere in it.
    ///
    /// THE TEST HAS TO EXCLUDE TWO KINDS OF INNOCENT TEXT, both of which live
    /// in the very same band. Ordinary Latin, whose accented letters are here
    /// (`é` is `0xE9`, `ü` is `0xFC`) — excluded by `band > latin`, since a
    /// French or German page is overwhelmingly ASCII with accents sprinkled
    /// through it while an Armenian one drawn this way has ASCII only in its
    /// digits and the occasional Latin logo. And the unit symbols, `²` in
    /// `5,98 մ²` above all — excluded by counting DISTINCT band characters,
    /// because a page of areas repeats one of them and Armenian words do not.
    ///
    /// IT DOES NOT ASK WHETHER THE TEXT IS ALREADY ARMENIAN, and that was a
    /// bug rather than a simplification. One document in this corpus prints a
    /// legacy font beside a properly encoded one, so a single correctly
    /// spelled word vetoed the repair of the whole page around it. Nothing
    /// was gained by the veto either: [`letter`] is defined only on the
    /// Latin-1 band, so text that is already Armenian passes through
    /// [`transliterate`] unchanged.
    ///
    /// THIS IS A QUESTION ABOUT TEXT AND NOT ABOUT A FONT. Asked of a font's
    /// character map it answers yes for any ordinary Latin font whose
    /// `/Encoding /Differences` names its accents and leaves ASCII to the base
    /// encoding: the map then holds the whole accent band and no ASCII at all,
    /// so `band > latin` is satisfied by a table that draws French.
    pub fn has_legacy_text(&self) -> bool {
        self.seen.count_ones() as usize >= MIN_DISTINCT_BAND && self.band > self.latin
    }
}

/// The same text with every band character read as the letter it stands for.
///
/// Everything else passes through untouched — ASCII digits, punctuation, a
/// Latin logo, and Armenian that was already written properly, since none of
/// them lies in the band.
pub fn transliterate(text: &str) -> String {
    text.chars().map(|c| letter(c).unwrap_or(c)).collect()
}

/// Reads back a text that spells Armenian with Latin-1 code points, or
/// returns `None` when it is not that kind of text.
///
/// The decision needs the WHOLE text: a line, let alone a word, is far too
/// short to tell `Ø` in `Ø 1:25` from a Latin `Ø`. Applying it to the whole
/// text costs nothing, because [`transliterate`] touches only the band.
pub fn repair(text: &str) -> Option<String> {
    Tally::of(text.chars())
        .has_legacy_text()
        .then(|| transliterate(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_sheet_name_off_its_bytes() {
        assert_eq!(
            repair("¶ÈÊ²ìàð Ð²î²Î²¶ÆÌ Ø 1:100").as_deref(),
            Some("ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ Մ 1:100")
        );
        assert_eq!(
            repair("ÎÆê²ÜÎàôÔ²ÚÆÜ Ð²ðÎÆ Ð²î²Î²¶ÆÌ Àêî -2.67 ÜÆÞÆ").as_deref(),
            Some("ԿԻՍԱՆԿՈՒՂԱՅԻՆ ՀԱՐԿԻ ՀԱՏԱԿԱԳԻԾ ԸՍՏ -2.67 ՆԻՇԻ")
        );
    }

    #[test]
    fn leaves_ordinary_latin_alone() {
        // Accented Latin lives in the same byte range and must survive.
        assert_eq!(repair("Café de la Gare, Ürün çeşitleri, Grüße"), None);
        assert_eq!(repair("ARCHFORM PROJECT"), None);
        // Text already in Armenian carries nothing in the band to repair.
        assert_eq!(repair("ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ"), None);
        // Too little to tell.
        assert_eq!(repair("²ð"), None);
        // Square metres: one band character, however often a page repeats it.
        assert_eq!(repair("5,98 մ² 12,40 մ² 3,10 մ² 77,00 մ² 8,25 մ²"), None);
        // ...and degrees, the other unit that lands in the band.
        assert_eq!(repair("15° 30° 45° 60° 90° 120° 180° 270°"), None);
    }

    /// One dossier in this corpus prints a legacy font beside a properly
    /// encoded one, and a line can mix the two inside itself. Requiring the
    /// text to hold no proper Armenian left every one of those pages as it
    /// was; the properly spelled words need no protection, because they are
    /// not in the band.
    #[test]
    fn a_page_that_mixes_both_encodings_is_repaired() {
        assert_eq!(
            repair("¶ÈÊ²ìàð Ð²î²Î²¶ÆÌ Ø 1:200\nՀՀ, ԵՐԵՎԱՆ\n50մմ ´²¼²Èîº ¶ÈÊ²ø²ð").as_deref(),
            Some("ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ Մ 1:200\nՀՀ, ԵՐԵՎԱՆ\n50մմ ԲԱԶԱԼՏԵ ԳԼԽԱՔԱՐ")
        );
    }

    #[test]
    fn a_latin_logo_does_not_stop_a_repair() {
        let repaired = repair("ARCHFORM ä²îìÆð²îàôª ²ðÂàôð Ø²ð¶²ðÚ²Ü").expect("repaired");
        assert!(repaired.starts_with("ARCHFORM ՊԱՏՎԻՐԱՏՈՒ"), "{repaired}");
    }
}
