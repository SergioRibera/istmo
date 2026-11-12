//! XML text helpers shared by every generator that writes manifests,
//! plists or intent filters.

/// Escape the five XML special characters so `raw` is safe both as
/// element text and inside a single- or double-quoted attribute.
#[must_use]
pub(crate) fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_every_special_character() {
        assert_eq!(escape(r#"a&b<c>d"e'f"#), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(
            escape("android.permission.INTERNET"),
            "android.permission.INTERNET"
        );
    }
}
