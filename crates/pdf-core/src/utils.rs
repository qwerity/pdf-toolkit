use std::ffi::OsStr;

pub(crate) fn sanitize_filename_component(stem: Option<&OsStr>) -> String {
    let raw_stem = stem.and_then(OsStr::to_str).unwrap_or_default();
    let mut sanitized = String::with_capacity(raw_stem.len());
    let mut last_was_replacement = false;

    for ch in raw_stem.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' => {
                sanitized.push(ch);
                last_was_replacement = false;
            }
            '.' | '_' | '-' if !sanitized.is_empty() => {
                sanitized.push(ch);
                last_was_replacement = false;
            }
            _ if !last_was_replacement => {
                sanitized.push('_');
                last_was_replacement = true;
            }
            _ => {}
        }
    }

    let trimmed = sanitized.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));

    if trimmed.is_empty() {
        "document".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitize(input: &str) -> String {
        sanitize_filename_component(Some(OsStr::new(input)))
    }

    #[test]
    fn keeps_alphanumeric_names() {
        assert_eq!(sanitize("Report2024"), "Report2024");
    }

    #[test]
    fn keeps_inner_dots_dashes_underscores() {
        assert_eq!(sanitize("a.b-c_d"), "a.b-c_d");
    }

    #[test]
    fn collapses_runs_of_special_characters() {
        assert_eq!(sanitize("my doc (1)"), "my_doc_1");
        assert_eq!(sanitize("a   b"), "a_b");
    }

    #[test]
    fn trims_leading_and_trailing_punctuation() {
        assert_eq!(sanitize("..name.."), "name");
        assert_eq!(sanitize("_x_"), "x");
        assert_eq!(sanitize(".hidden"), "hidden");
    }

    #[test]
    fn replaces_non_ascii_characters() {
        assert_eq!(sanitize("résumé"), "r_sum");
    }

    #[test]
    fn falls_back_to_document_when_nothing_remains() {
        assert_eq!(sanitize(""), "document");
        assert_eq!(sanitize("???"), "document");
        assert_eq!(sanitize("..."), "document");
        assert_eq!(sanitize_filename_component(None), "document");
    }
}
