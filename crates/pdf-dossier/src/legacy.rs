//! Decoding the legacy 8-bit Armenian fonts (the ArmSCII-8 family behind
//! Latin-1 code points) that Cadastre Committee fee tables, some 1:500
//! extracts and many CAD title blocks are set in. The text layer of such a
//! page reads `¸ÇÙáÕ` where the page shows Դիմող; the mapping is fixed, so
//! the decoding is exact — nothing is guessed.
//!
//! Used for reading facts off a page only. The annotation's `extraction.text`
//! stays the layer as extracted, with `encoding_issue` saying it is
//! mis-encoded, so a reader knows what the binary saw.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::text_signals::is_legacy_encoded;

/// Latin-1 code points of the legacy font, lower case then upper case, in
/// Armenian alphabet order (ա…ֆ, with և after օ).
const LEGACY_LOWER: &str = "³µ·¹»½¿ÁÃÅÇÉËÍÏÑÓÕ×ÙÛÝßáãåçéëíïñóõ÷ùû¨ý";
const LEGACY_UPPER: &str = "²´¶¸º¼¾ÀÂÄÆÈÊÌÎÐÒÔÖØÚÜÞàâäæèêìîðòôöøúü";
const ARMENIAN_LOWER: &str = "աբգդեզէըթժիլխծկհձղճմյնշոչպջռսվտրցւփքօևֆ";
const ARMENIAN_UPPER: &str = "ԱԲԳԴԵԶԷԸԹԺԻԼԽԾԿՀՁՂՃՄՅՆՇՈՉՊՋՌՍՎՏՐՑՒՓՔՕՖ";

static MAP: LazyLock<HashMap<char, char>> = LazyLock::new(|| {
    let mut map: HashMap<char, char> = LEGACY_LOWER
        .chars()
        .zip(ARMENIAN_LOWER.chars())
        .chain(LEGACY_UPPER.chars().zip(ARMENIAN_UPPER.chars()))
        .collect();
    map.insert('`', '՝');
    map.insert('ª', 'ը');
    map
});

fn is_legacy_char(c: char) -> bool {
    MAP.contains_key(&c) && c != '`'
}

/// The text with legacy-font runs turned into Armenian. A page the detector
/// calls mis-encoded is mapped whole; otherwise only runs of two or more
/// legacy characters are, so a stray title-block word on a clean page is
/// read too and the clean text is untouched.
pub fn decode_legacy(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_legacy_char) {
        return Cow::Borrowed(text);
    }
    if is_legacy_encoded(text) {
        return Cow::Owned(
            text.chars()
                .map(|c| MAP.get(&c).copied().unwrap_or(c))
                .collect(),
        );
    }
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let run = chars[i..]
            .iter()
            .take_while(|c| is_legacy_char(**c))
            .count();
        if run >= 2 {
            out.extend(chars[i..i + run].iter().map(|c| MAP[c]));
            i += run;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fee_table_line_decodes_to_armenian() {
        let decoded =
            decode_legacy("¸ÇÙáÕ` ²ñÙ³Ý ÐáíÑ³ÝÝÇëÛ³Ý îñ³Ù³¹ñí³Í ï»Õ»Ï³ïíáõÃÛáõÝÁ 19.05.2026Ã.");
        assert_eq!(
            decoded,
            "Դիմող՝ Արման Հովհաննիսյան Տրամադրված տեղեկատվությունը 19.05.2026թ."
        );
    }

    #[test]
    fn clean_text_is_returned_as_is() {
        assert!(matches!(decode_legacy("Դիմող Արման 123"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_stray_legacy_word_on_a_clean_page_is_decoded_alone() {
        let decoded = decode_legacy("X Y 8470125.3968 4418650.9555 Îááñ¹ÇÝ³ïÝ»ñ");
        assert_eq!(decoded, "X Y 8470125.3968 4418650.9555 Կոորդինատներ");
    }
}
