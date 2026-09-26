//! Heuristics for detecting garbled extracted text (e.g. broken CID font
//! mappings) shared by the plain-text and structured extractors. Both paths
//! must judge the same input shape — `sanitize_extracted_text` of the page's
//! full text — so the thresholds below apply to one distribution.

/// Puts raw pdfium page text into the shape every consumer of it expects:
/// control characters (except line breaks and tabs) and trailing whitespace
/// dropped, and Armenian written on Latin-1 code points read back.
///
/// THE TRANSLITERATION BELONGS HERE AND NOT AT A CALL SITE. It is what makes
/// `¶ÈÊ²ìàð Ð²î²Î²¶ÆÌ` readable, and [`is_probably_unreadable_text`] must see
/// the readable form or it drops a perfectly good page as high-entropy
/// nonsense. Doing it in one extractor and not the others left `text` and
/// `structured` emitting the mojibake that `dossier` had already learned to
/// read. See [`pdf_armscii`].
pub fn sanitize_extracted_text(text: &str) -> String {
    let filtered = text
        .chars()
        .filter(|ch| is_allowed_text_char(*ch))
        .collect::<String>();
    let trimmed = filtered.trim_end();

    pdf_armscii::repair(trimmed).unwrap_or_else(|| trimmed.to_string())
}

fn is_allowed_text_char(ch: char) -> bool {
    !ch.is_control() || matches!(ch, '\n' | '\r' | '\t')
}

/// Words of letters alone a page needs before their vowels can speak for it.
const MIN_PLAIN_WORDS: usize = 10;

/// Share of those words with a vowel above which the page is readable.
const READABLE_PLAIN_VOWEL_RATIO: f32 = 0.9;

/// Returns true when extracted text is statistically likely to be garbage:
/// enough material to judge, high symbol/digit noise, almost no lowercase,
/// and almost no vowel-bearing words.
pub fn is_probably_unreadable_text(text: &str) -> bool {
    let metrics = analyze_text_quality(text);

    if metrics.visible_char_count() < 80 || metrics.alpha_word_count < 20 {
        return false;
    }

    // THE TEST IS ABOUT LATIN SCRIPT AND ONLY ABOUT LATIN SCRIPT. Its two
    // deciding signals are the share of lowercase and the share of words
    // carrying a Latin vowel, and a page set in Armenian, Greek or Cyrillic
    // capitals scores zero on both by definition — the drawing sheets in this
    // project's corpus are titled entirely in Armenian capitals. Judging them
    // by it threw away perfectly readable text, so text that is not mostly
    // Latin is not judged here at all: an unreadable page costs a vision
    // pass, a page wrongly called unreadable is silently lost.
    if metrics.latin_letter_count * 2 < metrics.letter_count {
        return false;
    }

    // Real words are the one thing garbage does not produce. A page of codes,
    // times and capitals ("CD4:3 8 CD23:5", a bus timetable) is all digits and
    // punctuation, yet the words it does spell out nearly all carry a vowel;
    // letters from a broken font map scatter them ("LJQDOL]HG"), and even
    // random letters leave a vowel out of one word in five.
    if metrics.plain_word_count >= MIN_PLAIN_WORDS
        && metrics.plain_vowel_ratio() >= READABLE_PLAIN_VOWEL_RATIO
    {
        return false;
    }

    metrics.noise_ratio() > 0.35
        && metrics.lowercase_ratio() < 0.30
        && metrics.vowel_word_ratio() < 0.45
}

fn analyze_text_quality(text: &str) -> TextQualityMetrics {
    let mut metrics = TextQualityMetrics::default();

    for ch in text.chars() {
        if ch.is_alphabetic() {
            metrics.letter_count += 1;

            if is_latin_letter(ch) {
                metrics.latin_letter_count += 1;
            }
            if ch.is_lowercase() {
                metrics.lowercase_count += 1;
            }
        } else if ch.is_numeric() {
            metrics.digit_count += 1;
        } else if ch.is_ascii_punctuation() {
            metrics.punctuation_count += 1;
        }
    }

    metrics.alpha_word_count = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .count();
    metrics.vowel_word_count = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .filter(|word| word.chars().any(is_latin_vowel))
        .count();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| c.is_ascii_punctuation());
        if word.chars().count() >= 2 && word.chars().all(is_latin_letter) {
            metrics.plain_word_count += 1;
            metrics.plain_vowel_word_count += usize::from(is_vowelled_word(word));
        }
    }

    metrics
}

/// Whether a word of letters reads as a word rather than letters a broken
/// font map or a misread line scattered: a word in another script, or a
/// Latin one that carries a vowel (`y` counted).
pub fn is_vowelled_word(word: &str) -> bool {
    !word.chars().all(is_latin_letter)
        || word
            .chars()
            .any(|c| is_latin_vowel(c) || matches!(c, 'y' | 'Y'))
}

/// Whether a letter belongs to the Latin script: ASCII, the Latin-1
/// supplement's letters, and Latin Extended-A and -B.
fn is_latin_letter(ch: char) -> bool {
    ch.is_ascii_alphabetic() || matches!(u32::from(ch), 0xc0..=0x24f)
}

fn is_latin_vowel(ch: char) -> bool {
    matches!(
        ch,
        'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'
    )
}

#[derive(Default)]
struct TextQualityMetrics {
    letter_count: usize,
    /// Of those, the ones written in the Latin script.
    latin_letter_count: usize,
    lowercase_count: usize,
    digit_count: usize,
    punctuation_count: usize,
    alpha_word_count: usize,
    vowel_word_count: usize,
    /// Words of two or more Latin letters and nothing else, once their
    /// punctuation is trimmed, and of those the ones with a vowel.
    plain_word_count: usize,
    plain_vowel_word_count: usize,
}

impl TextQualityMetrics {
    fn visible_char_count(&self) -> usize {
        self.letter_count + self.digit_count + self.punctuation_count
    }

    fn noise_ratio(&self) -> f32 {
        let visible_char_count = self.visible_char_count();

        if visible_char_count == 0 {
            0.0
        } else {
            (self.digit_count + self.punctuation_count) as f32 / visible_char_count as f32
        }
    }

    fn lowercase_ratio(&self) -> f32 {
        if self.letter_count == 0 {
            0.0
        } else {
            self.lowercase_count as f32 / self.letter_count as f32
        }
    }

    fn plain_vowel_ratio(&self) -> f32 {
        if self.plain_word_count == 0 {
            0.0
        } else {
            self.plain_vowel_word_count as f32 / self.plain_word_count as f32
        }
    }

    fn vowel_word_ratio(&self) -> f32 {
        if self.alpha_word_count == 0 {
            0.0
        } else {
            self.vowel_word_count as f32 / self.alpha_word_count as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_disallowed_control_characters() {
        assert_eq!(sanitize_extracted_text("a\u{0}b\u{7}c"), "abc");
    }

    #[test]
    fn sanitize_keeps_newlines_tabs_and_carriage_returns() {
        assert_eq!(sanitize_extracted_text("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn sanitize_trims_trailing_whitespace_only() {
        assert_eq!(sanitize_extracted_text("  a b \n\t "), "  a b");
    }

    #[test]
    fn normal_prose_is_not_flagged_unreadable() {
        let text =
            "The quick brown fox jumps over the lazy dog near the quiet river bank. ".repeat(3);
        assert!(!is_probably_unreadable_text(&text));
    }

    #[test]
    fn numeric_heavy_but_readable_text_is_not_flagged() {
        let text = "Total revenue for quarter one was 1234 and quarter two reached 5678, \
                    while expenses stayed near 910 leaving margin around 34 percent overall \
                    according to the annual report published today by the finance team.";
        assert!(!is_probably_unreadable_text(text));
    }

    #[test]
    fn garbled_vowelless_noise_is_flagged_unreadable() {
        let text = "X9#Z 7Q$W 3K%T ".repeat(10);
        assert!(is_probably_unreadable_text(&text));
    }

    #[test]
    fn short_text_is_never_flagged() {
        assert!(!is_probably_unreadable_text("9#@!X"));
        assert!(!is_probably_unreadable_text(""));
    }

    /// A drawing sheet titled in Armenian capitals has no lowercase and no
    /// Latin vowel, which is what the Latin test calls garbage.
    #[test]
    fn capitals_in_another_script_are_not_garbage() {
        let text = "ԱՌԱՋԻՆ ՀԱՐԿԻ ՀԱՏԱԿԱԳԻԾ ՄԱՍՇՏԱԲ 1:100 ԲՆԱԿԵԼԻ ՏԱՆ ՄԱՍՆԱԿԻ \
                    ՎԵՐԱԿԱՌՈՒՑՄԱՆ ԷՍՔԻԶԱՅԻՆ ՆԱԽԱԳԻԾ ՔԱՂԱՔ ԵՐԵՎԱՆ, ՇԵՆԳԱՎԻԹ \
                    ՎԱՐՉԱԿԱՆ ՇՐՋԱՆ 5,215 3,734 1,892 100 2,719 1,838 100 2,126";
        assert!(!is_probably_unreadable_text(text));
        // The Latin judgement still applies where the text is Latin.
        assert!(is_probably_unreadable_text(&"X9#Z 7Q$W 3K%T ".repeat(10)));
    }

    /// A code's supplement history and a bus timetable: codes, numbers and
    /// capitals, every spelled-out word a word.
    #[test]
    fn codes_and_times_with_real_words_are_readable() {
        let history = "LAFAYETTE CODE Page No. Supp. No. CD1:1 3 CD11:1 OC CD1:3, CD1:4 OC \
                       CD20:1 OC CD1:5, CD1:6 OC CD20:3 OC CD1:7, CD1:8 3 CD21:1 5 CD1:9, \
                       CD1:10 3 CD23:1 5 CD2:1 8 CD23:3, CD23:4 5 CD4:1 8 CD23:5, CD23:6 5 \
                       CD4:3, CD4:4 8 CD23:7, CD23:8 8 CD4:5, CD4:6 8 CD23:9 8 CD4:7, CD4:8 \
                       Checklist of Up-to-Date Pages Supplement Index";
        assert!(!is_probably_unreadable_text(history));
        let timetable = "Route 1 Mendenhall Valley Downtown 6:15 6:22 6:30 6:41 6:45 6:52 \
                         7:00 7:11 7:15 7:22 7:30 7:41 Nugget Mall Riverside Federal Bldg \
                         Hospital Lemon Creek 7:45 7:52 8:00 8:11 8:15 8:22 8:30 8:41 DTC \
                         8:45 8:52 9:00 9:11 9:15 9:22 9:30 9:41 Sunday Outbound Inbound";
        assert!(!is_probably_unreadable_text(timetable));
    }

    /// A font mapped to the wrong glyphs spells letters, but not words: its
    /// runs of letters alone lack vowels, so they vouch for nothing.
    #[test]
    fn letter_runs_without_vowels_stay_unreadable() {
        let text = "LJQD 7#9$ QWHR 3%4& VHFW 8!2# ".repeat(8);
        assert!(is_probably_unreadable_text(&text));
    }

    #[test]
    fn quality_metrics_ratios_handle_empty_input() {
        let metrics = analyze_text_quality("");
        assert_eq!(metrics.visible_char_count(), 0);
        assert_eq!(metrics.noise_ratio(), 0.0);
        assert_eq!(metrics.lowercase_ratio(), 0.0);
        assert_eq!(metrics.vowel_word_ratio(), 0.0);
    }

    #[test]
    fn quality_metrics_count_categories() {
        let metrics = analyze_text_quality("Ab1! cd");
        assert_eq!(metrics.letter_count, 4);
        assert_eq!(metrics.lowercase_count, 3);
        assert_eq!(metrics.digit_count, 1);
        assert_eq!(metrics.punctuation_count, 1);
        assert_eq!(metrics.alpha_word_count, 2);
        assert_eq!(metrics.vowel_word_count, 1);
    }
}
