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
