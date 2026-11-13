//! File names for content received from other apps.

/// Turn a display name from another app into a file name that cannot
/// escape the directory it is joined to.
///
/// The rules, identical on every platform and mirrored by the Kotlin
/// (`ShareBackendImpl.sanitize`) and Swift (`IstmoShareHandoff.sanitize`)
/// backends — `tests::sanitize_vectors` is the shared reference:
///
/// 1. Split on both `/` and `\` and keep the last non-empty component.
/// 2. Replace `< > : " | ? *` and C0 / C1 control characters
///    (U+0000–U+001F, U+007F–U+009F) with `_`.
/// 3. A result made only of dots (or empty) becomes `shared`, so `.`
///    and `..` never survive.
/// 4. A Windows device name ([`WINDOWS_DEVICE_NAMES`], ASCII case
///    insensitive) as the part before the first `.`, trailing spaces
///    ignored, gets a `_` prefix: `CON.txt` would open the console.
/// 5. A name longer than [`MAX_FILE_NAME_BYTES`] UTF-8 bytes is cut at a
///    character boundary, keeping an extension of up to
///    [`MAX_KEPT_EXTENSION_BYTES`] bytes (the part from the last `.`, not
///    at index 0).
#[must_use]
pub(crate) fn sanitize_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let mut name = if cleaned.trim_matches('.').is_empty() {
        "shared".to_owned()
    } else {
        cleaned
    };
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    if WINDOWS_DEVICE_NAMES
        .iter()
        .any(|device| device.eq_ignore_ascii_case(stem))
    {
        name.insert(0, '_');
    }
    truncate_file_name(name)
}

/// Longest file name every supported file system stores (ext4 and APFS
/// count UTF-8 bytes, NTFS UTF-16 units, which are never more).
const MAX_FILE_NAME_BYTES: usize = 255;

/// Longest extension [`sanitize_file_name`] keeps when it shortens a name.
const MAX_KEPT_EXTENSION_BYTES: usize = 16;

/// Names Windows resolves to devices in any directory.
const WINDOWS_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

fn truncate_file_name(name: String) -> String {
    if name.len() <= MAX_FILE_NAME_BYTES {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= MAX_KEPT_EXTENSION_BYTES => name.split_at(dot),
        _ => (name.as_str(), ""),
    };
    let mut end = MAX_FILE_NAME_BYTES - ext.len();
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", &stem[..end])
}

#[cfg(test)]
mod tests {
    use super::sanitize_file_name;

    /// Reference vectors for the file-name rules. The Kotlin
    /// (`ShareBackendImpl.sanitize`) and Swift (`IstmoShareHandoff.sanitize`)
    /// ports must map every input here to the same output.
    const SANITIZE_VECTORS: &[(&str, &str)] = &[
        ("report.pdf", "report.pdf"),
        ("../../etc/passwd", "passwd"),
        ("..\\..\\evil.exe", "evil.exe"),
        ("C:\\Users\\me\\photo.jpg", "photo.jpg"),
        ("dir/", "dir"),
        ("a:b*c?.txt", "a_b_c_.txt"),
        ("<x>|\"y\"", "_x___y_"),
        ("tab\there", "tab_here"),
        ("nul\u{0}byte", "nul_byte"),
        ("del\u{7f}c1\u{85}", "del_c1_"),
        ("zero\u{200b}width", "zero\u{200b}width"),
        ("naïve 日本.txt", "naïve 日本.txt"),
        ("..hidden", "..hidden"),
        (".", "shared"),
        ("..", "shared"),
        ("...", "shared"),
        ("a/..", "shared"),
        ("/", "shared"),
        ("", "shared"),
        ("CON", "_CON"),
        ("con.txt", "_con.txt"),
        ("Nul .tar.gz", "_Nul .tar.gz"),
        ("dir/COM1", "_COM1"),
        ("LPT¹.log", "_LPT¹.log"),
        ("COM10", "COM10"),
        ("console.txt", "console.txt"),
        (".con", ".con"),
    ];

    #[test]
    fn sanitize_vectors() {
        for (input, expected) in SANITIZE_VECTORS {
            assert_eq!(sanitize_file_name(input), *expected, "input {input:?}");
        }
    }

    /// Length vectors, built rather than spelled out: `(input, expected)`.
    fn length_vectors() -> Vec<(String, String)> {
        vec![
            ("a".repeat(255), "a".repeat(255)),
            ("a".repeat(300) + ".txt", "a".repeat(251) + ".txt"),
            // 2-byte characters: the cut backs off to a char boundary.
            ("é".repeat(200) + ".pdf", "é".repeat(125) + ".pdf"),
            // An extension longer than 16 bytes is not kept.
            (
                "a".repeat(250) + "." + &"b".repeat(20),
                "a".repeat(250) + "." + &"b".repeat(4),
            ),
            (
                "CON".to_owned() + &"x".repeat(300),
                "CON".to_owned() + &"x".repeat(252),
            ),
            ("\u{1F600}".repeat(70), "\u{1F600}".repeat(63)),
        ]
    }

    #[test]
    fn sanitize_length_vectors() {
        for (input, expected) in length_vectors() {
            let got = sanitize_file_name(&input);
            assert!(got.len() <= 255, "{} bytes", got.len());
            assert_eq!(got, expected, "input of {} bytes", input.len());
        }
    }
}
