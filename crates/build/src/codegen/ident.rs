//! Identifier and string-literal escaping for the generated sources.

/// Kotlin hard keywords: never valid as a bare identifier.
const KOTLIN_KEYWORDS: &[&str] = &[
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

/// Swift keywords plus the contextual ones that are rejected in some
/// declaration positions; backticks are harmless where they are not needed.
const SWIFT_KEYWORDS: &[&str] = &[
    "Any",
    "Protocol",
    "Self",
    "Type",
    "as",
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "fileprivate",
    "final",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "is",
    "lazy",
    "let",
    "mutating",
    "nil",
    "nonmutating",
    "open",
    "operator",
    "override",
    "postfix",
    "precedencegroup",
    "prefix",
    "private",
    "protocol",
    "public",
    "repeat",
    "rethrows",
    "return",
    "self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "typealias",
    "unowned",
    "var",
    "weak",
    "where",
    "while",
];

/// `name` as a Kotlin identifier, backticked when it is a hard keyword.
pub(crate) fn kt_ident(name: &str) -> String {
    if KOTLIN_KEYWORDS.contains(&name) {
        format!("`{name}`")
    } else {
        name.to_owned()
    }
}

/// `name` as a Swift identifier, backticked when it is a keyword.
pub(crate) fn swift_ident(name: &str) -> String {
    if SWIFT_KEYWORDS.contains(&name) {
        format!("`{name}`")
    } else {
        name.to_owned()
    }
}

/// Rust strict and reserved keywords that a raw identifier can spell.
const RUST_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield",
];

/// `name` as a Rust identifier, raw (`r#type`) when it is a keyword.
pub(crate) fn rust_ident(name: &str) -> String {
    if RUST_KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}

/// A local variable for a contract-supplied name inside a generated body
/// that already declares `reserved` locals. A clash gets a trailing `_`
/// (never a keyword), anything else goes through `escape`.
pub(crate) fn local(name: &str, reserved: &[&str], escape: fn(&str) -> String) -> String {
    if reserved.contains(&name) {
        format!("{name}_")
    } else {
        escape(name)
    }
}

/// `base`, suffixed with `_` until it differs from every name in `taken`:
/// a generated local that must not shadow a contract-named parameter.
pub(crate) fn fresh(base: &str, taken: &[&str]) -> String {
    let mut name = base.to_owned();
    while taken.contains(&name.as_str()) {
        name.push('_');
    }
    name
}

/// A quoted Kotlin string literal. `$` is escaped so contract text never
/// turns into a string template.
pub(crate) fn kt_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A quoted Swift string literal. Escaping `\` also rules out `\(…)`
/// interpolation.
pub(crate) fn swift_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// `value` with control characters blanked, safe inside a `//` comment.
pub(crate) fn comment_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_backticked() {
        assert_eq!(kt_ident("object"), "`object`");
        assert_eq!(kt_ident("value"), "value");
        assert_eq!(swift_ident("default"), "`default`");
        assert_eq!(swift_ident("value"), "value");
    }

    #[test]
    fn reserved_locals_get_a_suffix() {
        assert_eq!(local("payload", &["payload"], kt_ident), "payload_");
        assert_eq!(local("in", &["payload"], kt_ident), "`in`");
    }

    #[test]
    fn string_literals_escape_quotes_backslashes_and_templates() {
        assert_eq!(kt_string(r#"a"b\c$d"#), r#""a\"b\\c\$d""#);
        assert_eq!(swift_string(r#"a"b\(c)"#), r#""a\"b\\(c)""#);
    }
}
