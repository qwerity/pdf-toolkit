//! Heuristics for detecting garbled extracted text (e.g. broken CID font
//! mappings) shared by the plain-text and structured extractors.

/// Returns true when extracted text is statistically likely to be garbage:
/// enough material to judge, high symbol/digit noise, almost no lowercase,
/// and almost no vowel-bearing words.
pub(crate) fn is_probably_unreadable_text(text: &str) -> bool {
    let metrics = analyze_text_quality(text);

    if metrics.visible_char_count() < 80 || metrics.alpha_word_count < 20 {
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

fn is_latin_vowel(ch: char) -> bool {
    matches!(
        ch,
        'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'
    )
}

#[derive(Default)]
struct TextQualityMetrics {
    letter_count: usize,
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
