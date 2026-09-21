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

    metrics
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
