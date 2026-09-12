//! What a page's embedded text says about itself: which scripts it uses, and
//! whether the layer is mis-encoded.
//!
//! The mis-encoding detector exists because a text layer can be present,
//! extractable, and wrong: CAD exporters that embed a legacy 8-bit Armenian
//! font (ArmSCII glyphs behind Latin-1 codepoints) or a custom phonetic-glyph
//! mapping produce confident mojibake — `´ÜÎºÈÆ` where the page shows
//! ԲՆԱԿԵԼԻ. Character statistics catch it: real content in this domain is
//! Armenian, Basic Latin or Cyrillic, so alphabetic characters landing in the
//! Latin-1 Supplement / Latin Extended / IPA blocks are evidence of a wrong
//! mapping, not of a language.
//!
//! Thresholds were calibrated on the 14-dossier ground-truth corpus
//! (`docs/annotation-findings.md`): flag when at least [`MIN_SUSPICIOUS_WORDS`]
//! words are mostly-suspicious and they exceed [`SUSPICIOUS_WORD_SHARE`] of
//! the page's words. That separates whole-album mojibake and mojibake title
//! blocks from clean pages with a stray legacy word in a coordinate table.

/// Fewer mostly-suspicious words than this is a stray, not a mis-encoding.
const MIN_SUSPICIOUS_WORDS: usize = 4;

/// Share of the page's alphabetic words that must be mostly-suspicious.
const SUSPICIOUS_WORD_SHARE: f64 = 0.08;

/// Letters landing in these blocks are no language this corpus contains.
fn is_suspicious_letter(c: char) -> bool {
    let cp = c as u32;
    c.is_alphabetic() && ((0xA1..=0x24F).contains(&cp) || (0x250..=0x2AF).contains(&cp))
}

/// True when the text layer is likely a legacy-encoded font's mojibake.
pub fn is_legacy_encoded(text: &str) -> bool {
    let mut words = 0usize;
    let mut suspicious_words = 0usize;
    for word in text.split_whitespace() {
        let letters = word.chars().filter(|c| c.is_alphabetic()).count();
        if letters == 0 {
            continue;
        }
        words += 1;
        let suspicious = word.chars().filter(|c| is_suspicious_letter(*c)).count();
        if suspicious * 2 > letters {
            suspicious_words += 1;
        }
    }
    suspicious_words >= MIN_SUSPICIOUS_WORDS
        && suspicious_words as f64 > words as f64 * SUSPICIOUS_WORD_SHARE
}

/// Minimum letters of a script before the page is said to carry the language.
const MIN_SCRIPT_LETTERS: usize = 10;

/// ISO 639-1 codes of the languages visible in the text, in a fixed order.
pub fn languages(text: &str) -> Vec<String> {
    let mut armenian = 0usize;
    let mut latin = 0usize;
    let mut cyrillic = 0usize;
    for c in text.chars() {
        let cp = c as u32;
        if (0x531..=0x58F).contains(&cp) || (0xFB13..=0xFB17).contains(&cp) {
            armenian += 1;
        } else if c.is_ascii_alphabetic() {
            latin += 1;
        } else if (0x400..=0x4FF).contains(&cp) {
            cyrillic += 1;
        }
    }
    let mut out = Vec::new();
    if armenian >= MIN_SCRIPT_LETTERS {
        out.push("hy".to_string());
    }
    if latin >= MIN_SCRIPT_LETTERS {
        out.push("en".to_string());
    }
    if cyrillic >= MIN_SCRIPT_LETTERS {
        out.push("ru".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armenian_english_and_russian_pages_are_not_flagged() {
        for text in [
            "ՎԿԱՅԱԿԱՆ N 20300101-01-0001, գաղտնաբառ՝ 0AAAA0AAA0AA Կադաստրային ծածկագիրը՝ \
             10-001-0456-0123 Մակերեսի չափը հա 0.0434 Նպատակային նշանակությունը բնակավայրերի",
            "REPUBLIC OF ARMENIA passport surname name date of birth authority signature of holder",
            "Технические характеристики экскаватора рабочая масса двигатель мощность привода",
        ] {
            assert!(!is_legacy_encoded(text), "wrongly flagged: {text}");
        }
    }

    #[test]
    fn armscii_mojibake_is_flagged() {
        let text = "´Ü²ÎºÈÆ î²Ü Ø²êÜ²ÎÆ ìºð²Î²èàôòØ²Ü ¾êøÆ¼²ÚÆÜ Ü²Ê²¶ÆÌ ø. ºðºì²Ü, ÞºÜ¶²ìÆÂ";
        assert!(is_legacy_encoded(text));
    }

    #[test]
    fn phonetic_glyph_fonts_are_flagged() {
        let text = "ɬʏʙʃ Թɼʗʀ Թɼʗʀɼʗ ɬʏʚ ʛɸʍ Թɼʗ ɬɸʔʖ ɦʖʏʗ Աʋ ԱՆ Տʍʜʗɼʍ Կɣɸʍʏʔʌɸʍ";
        assert!(is_legacy_encoded(text));
    }

    /// A single mojibake word in an otherwise-numeric coordinate table is a
    /// stray, not a mis-encoded page.
    #[test]
    fn a_stray_legacy_word_among_numbers_is_not_flagged() {
        let text = "X Y 1 8470125.3968 4418650.9555 2 8470129.9556 4418643.7350 Îááñ¹ÇÝ³ïÝ»ñ";
        assert!(!is_legacy_encoded(text));
    }

    #[test]
    fn languages_report_the_scripts_actually_present() {
        assert_eq!(
            languages("ՀԱՅԱՍՏԱՆԻ ՀԱՆՐԱՊԵՏՈՒԹՅՈՒՆ REPUBLIC OF ARMENIA"),
            vec!["hy", "en"]
        );
        assert_eq!(languages("Երևան քաղաք Շենգավիթ վարչական շրջան"), vec!["hy"]);
        assert!(languages("1 2 3 4.5").is_empty());
    }
}
