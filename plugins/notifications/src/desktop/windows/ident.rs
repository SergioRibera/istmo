//! Pure identity rules of the Windows backend: the AUMID check, the
//! activator CLSID, the `LocalServer32` command line, the activation
//! flags COM launches the app with, scheduled toast ids and image URIs.

use std::fmt::Write as _;

use crate::{InvalidField, NotificationError, Problem};

/// Longest AUMID Windows accepts.
pub(crate) const MAX_AUMID_LEN: usize = 128;

/// The flag `LocalServer32` launches the app with; COM appends
/// [`EMBEDDING_ARG`].
pub(crate) const ACTIVATION_ARG: &str = "-istmo-notification";
/// What COM appends to a `LocalServer32` command line.
pub(crate) const EMBEDDING_ARG: &str = "-Embedding";

/// Namespace of the activator CLSIDs (UUID v5 over the AUMID). Fixed:
/// changing it orphans every registered activator.
const CLSID_NAMESPACE: [u8; 16] = [
    0x5f, 0x0b, 0x6b, 0x2e, 0x93, 0x1c, 0x4f, 0x5e, 0x8d, 0x4a, 0x1e, 0x6e, 0x2c, 0x77, 0x3a, 0x91,
];

/// Checks an AUMID: 1 to 128 characters, no whitespace, no control
/// characters, no `\` (it is a registry key name).
pub(crate) fn check_aumid(aumid: &str) -> Result<(), NotificationError> {
    let problem = if aumid.is_empty() {
        Some(Problem::Empty)
    } else if aumid.chars().count() > MAX_AUMID_LEN {
        Some(Problem::TooLong)
    } else if aumid
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        Some(Problem::BadCharset)
    } else {
        None
    };
    problem.map_or(Ok(()), |problem| {
        Err(InvalidField::new("windows.aumid", problem).into())
    })
}

/// SHA-1 (RFC 3174), only for UUID v5.
#[expect(
    clippy::many_single_char_names,
    reason = "the variable names of RFC 3174"
)]
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// UUID v5 (RFC 4122 section 4.3) of `name` in `namespace`, as the
/// big-endian `u128` of its text form.
fn uuid_v5(namespace: [u8; 16], name: &[u8]) -> u128 {
    let mut input = Vec::with_capacity(16 + name.len());
    input.extend_from_slice(&namespace);
    input.extend_from_slice(name);
    let hash = sha1(&input);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    u128::from_be_bytes(bytes)
}

/// The activator CLSID of `aumid`: UUID v5 in the plugin's namespace,
/// stable across builds and exe paths.
pub(crate) fn activator_clsid(aumid: &str) -> u128 {
    uuid_v5(CLSID_NAMESPACE, aumid.as_bytes())
}

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (registry form, upper case).
pub(crate) fn guid_text(value: u128) -> String {
    let hex = format!("{value:032X}");
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The `LocalServer32` default value for `exe`.
pub(crate) fn server_command(exe: &str) -> String {
    format!("\"{exe}\" {ACTIVATION_ARG}")
}

/// `args` (without the program name) says COM started the process for
/// a toast activation.
pub(crate) fn is_activation_launch<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> bool {
    args.iter().any(|arg| arg.as_ref() == ACTIVATION_ARG)
}

/// `args` without the activation flags.
pub(crate) fn strip_activation_args<S: AsRef<std::ffi::OsStr>>(
    args: impl IntoIterator<Item = S>,
) -> Vec<std::ffi::OsString> {
    args.into_iter()
        .filter(|arg| {
            let arg = arg.as_ref();
            arg != ACTIVATION_ARG && arg != EMBEDDING_ARG
        })
        .map(|arg| arg.as_ref().to_os_string())
        .collect()
}

/// FNV-1a, 64 bits.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The 16-character id of a scheduled toast (the platform limit):
/// FNV-1a over the tag, the fire time and the XML, so a changed
/// delivery gets a new id and the diff replaces it.
pub(crate) fn scheduled_id(tag: &str, fire_ms: i64, xml: &str) -> String {
    let mut bytes = Vec::with_capacity(tag.len() + 9 + xml.len());
    bytes.extend_from_slice(tag.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&fire_ms.to_le_bytes());
    bytes.extend_from_slice(xml.as_bytes());
    format!("{:016x}", fnv1a(&bytes))
}

/// A `file:///` URI for a local absolute path (`C:\a b\c.png` →
/// `file:///C:/a%20b/c.png`). Unpackaged apps can only use local
/// images; `None` for anything that is not an absolute drive or UNC
/// path. Verbatim paths (`\\?\C:\...`, `\\?\UNC\server\share\...`, what
/// `canonicalize` returns) map to the same URIs as their plain forms.
pub(crate) fn file_uri(path: &str) -> Option<String> {
    let (path, unc) = match path.strip_prefix(r"\\?\") {
        Some(verbatim) => match verbatim.strip_prefix(r"UNC\") {
            // A server name must follow.
            Some(share) if share.is_empty() || share.starts_with('\\') => return None,
            Some(share) => (share, true),
            None => (verbatim, false),
        },

        None => (path, false),
    };
    let bytes = path.as_bytes();
    let (prefix, rest) = if unc {
        ("file://", path)
    } else if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        ("file:///", path)
    } else {
        ("file://", path.strip_prefix(r"\\")?)
    };
    let mut out = String::with_capacity(prefix.len() + rest.len() + 8);
    out.push_str(prefix);
    for byte in rest.bytes() {
        match byte {
            b'\\' => out.push('/'),
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'/'
            | b':'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'!'
            | b'$'
            | b'\''
            | b'('
            | b')'
            | b'+'
            | b','
            | b';'
            | b'@' => out.push(char::from(byte)),
            other => {
                let _infallible = write!(out, "%{other:02X}");
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_rfc_3174() {
        let hex = |bytes: [u8; 20]| {
            use std::fmt::Write as _;
            bytes.iter().fold(String::new(), |mut out, b| {
                let _infallible = write!(out, "{b:02x}");
                out
            })
        };
        assert_eq!(
            hex(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn uuid_v5_matches_the_python_reference() {
        // uuid.uuid5(uuid.NAMESPACE_DNS, "python.org")
        let dns = 0x6ba7_b810_9dad_11d1_80b4_00c0_4fd4_30c8_u128.to_be_bytes();
        assert_eq!(
            guid_text(uuid_v5(dns, b"python.org")),
            "{886313E1-3B8A-5372-9B90-0C9AEE199E5D}"
        );
    }

    #[test]
    fn clsid_is_stable_per_aumid() {
        let a = activator_clsid("dev.istmo.example");
        assert_eq!(a, activator_clsid("dev.istmo.example"));
        assert_ne!(a, activator_clsid("dev.istmo.other"));
        let text = guid_text(a);
        assert_eq!(text.len(), 38);
        assert_eq!(&text[15..16], "5");
    }

    #[test]
    fn aumids_are_checked() {
        assert!(check_aumid("dev.istmo.example").is_ok());
        assert!(check_aumid("").is_err());
        assert!(check_aumid("has space").is_err());
        assert!(check_aumid(r"a\b").is_err());
        assert!(check_aumid(&"a".repeat(129)).is_err());
        assert!(check_aumid(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn activation_flags_are_found_and_stripped() {
        let args = ["-istmo-notification", "-Embedding"];
        assert!(is_activation_launch(&args));
        assert!(!is_activation_launch(&["file.txt"]));
        assert_eq!(
            strip_activation_args(["a", "-istmo-notification", "-Embedding", "b"]),
            vec![std::ffi::OsString::from("a"), "b".into()]
        );
        assert_eq!(
            server_command(r"C:\Apps\My App\app.exe"),
            r#""C:\Apps\My App\app.exe" -istmo-notification"#
        );
    }

    #[test]
    fn file_uris() {
        assert_eq!(
            file_uri(r"C:\Users\Ana\AppData\Local\x y\1#2.png").as_deref(),
            Some("file:///C:/Users/Ana/AppData/Local/x%20y/1%232.png")
        );
        assert_eq!(
            file_uri(r"\\?\C:\a.png").as_deref(),
            Some("file:///C:/a.png")
        );
        assert_eq!(
            file_uri(r"\\server\share\a.png").as_deref(),
            Some("file://server/share/a.png")
        );
        assert_eq!(
            file_uri(r"\\?\UNC\server\share\a b.png").as_deref(),
            Some("file://server/share/a%20b.png")
        );
        assert_eq!(file_uri(r"\\?\UNC\"), None);
        assert_eq!(file_uri(r"\\?\Volume{x}\a.png"), None);
        assert_eq!(file_uri("relative.png"), None);
        assert_eq!(file_uri("/tmp/a.png"), None);
        assert_eq!(
            file_uri("C:/caf\u{e9}.png").as_deref(),
            Some("file:///C:/caf%C3%A9.png")
        );
    }

    #[test]
    fn scheduled_ids_are_sixteen_hex() {
        let id = scheduled_id("n", 1, "<toast/>");
        assert_eq!(id.len(), 16);
        assert_ne!(id, scheduled_id("n", 2, "<toast/>"));
        assert_ne!(id, scheduled_id("n", 1, "<toast />"));
    }
}
