//! Small text codecs the engines share: JSON escaping and field
//! extraction for the flat bridge messages, base64 for byte payloads.

#![allow(dead_code, reason = "each engine uses a different subset")]

/// JSON string escaping safe inside `<script>` and JS string literals.
pub(crate) fn json_escape(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                write!(out, "\\u{:04x}", c as u32).ok();
            }
            c => out.push(c),
        }
    }
    out
}

/// Run `source` only on pages whose origin is listed.
pub(crate) fn origin_guard(origins: &[String], source: &str) -> String {
    let list = origins
        .iter()
        .map(|o| format!("\"{}\"", json_escape(o)))
        .collect::<Vec<_>>()
        .join(",");
    format!("if ([{list}].includes(location.origin)) {{\n{source}\n}}")
}

/// Standard base64 with padding (what `atob` reads).
pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode [`base64`] output (what `btoa` writes).
pub(crate) fn unbase64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    };
    let bytes: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0_u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        for i in 0..chunk.len().saturating_sub(1) {
            #[expect(clippy::cast_possible_truncation, reason = "one byte of the group")]
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// Raw JSON token (string, number, literal, object or array) after the
/// first `"key":` in `json`. Enough for the flat messages the engines
/// exchange; not a general parser.
pub(crate) fn json_value_of(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = json.find(&needle)? + needle.len();
    let rest = json[start..].trim_start();
    let bytes = rest.as_bytes();
    let end = match bytes.first()? {
        b'"' => {
            let mut escaped = false;
            let mut end = None;
            for (i, b) in bytes.iter().enumerate().skip(1) {
                match (escaped, b) {
                    (true, _) => escaped = false,
                    (false, b'\\') => escaped = true,
                    (false, b'"') => {
                        end = Some(i + 1);
                        break;
                    }
                    _ => {}
                }
            }
            end?
        }
        b'{' | b'[' => {
            let (mut depth, mut in_string, mut escaped) = (0_i32, false, false);
            let mut end = None;
            for (i, b) in bytes.iter().enumerate() {
                match (in_string, escaped, b) {
                    (true, true, _) => escaped = false,
                    (true, false, b'\\') => escaped = true,
                    (true, false, b'"') | (false, _, b'"') => in_string = !in_string,
                    (false, _, b'{' | b'[') => depth += 1,
                    (false, _, b'}' | b']') => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            end?
        }
        _ => rest.find([',', '}', ']']).unwrap_or(rest.len()),
    };
    Some(rest[..end].trim().to_owned())
}

/// Decoded string value of `"key"` in a flat JSON object.
pub(crate) fn json_field(json: &str, key: &str) -> Option<String> {
    let token = json_value_of(json, key)?;
    let inner = token.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// Integer value of `"key"` in a flat JSON object.
pub(crate) fn json_number(json: &str, key: &str) -> Option<u64> {
    json_value_of(json, key)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", &[0, 255, 7, 128]] {
            assert_eq!(unbase64(&base64(data)).unwrap(), data);
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn json_helpers() {
        let json = r#"{"result":{"type":"object","value":{"a":[1,"}"]}},"t":"i","id":42,"c":"r\"pc","b":"cGluZw=="}"#;
        assert_eq!(json_value_of(json, "value").unwrap(), r#"{"a":[1,"}"]}"#);
        assert_eq!(json_field(json, "c").unwrap(), "r\"pc");
        assert_eq!(json_number(json, "id"), Some(42));
        assert_eq!(json_escape("a\"<\n"), "a\\\"\\u003c\\u000a");
    }
}
