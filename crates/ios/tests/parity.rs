//! The Swift runtime, the C header and this crate agree on every export,
//! status code and kind tag.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use istmo_core::native_frame::{NATIVE_ABI_VERSION, SubmitStatus};
use istmo_ios::RetainedKind;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> Option<String> {
    std::fs::read_to_string(root().join(rel)).ok()
}

fn exports(source: &str) -> BTreeSet<String> {
    source
        .split("#[unsafe(no_mangle)]")
        .skip(1)
        .filter_map(|item| {
            let at = item.find("fn ")? + 3;
            let name = &item[at..];
            Some(name[..name.find('(')?].trim().to_owned())
        })
        .collect()
}

/// `SCREAMING_SNAKE` → `lowerCamel`.
fn camel(name: &str) -> String {
    let mut out = String::new();
    for (i, part) in name.split('_').enumerate() {
        let lower = part.to_ascii_lowercase();
        if i == 0 {
            out.push_str(&lower);
        } else {
            let mut chars = lower.chars();
            if let Some(first) = chars.next() {
                out.push(first.to_ascii_uppercase());
                out.extend(chars);
            }
        }
    }
    out
}

/// `case <name> = <code>` lines of the Swift enum `name`.
fn swift_cases(source: &str, name: &str) -> Vec<(String, i64)> {
    let start = source.find(&format!("enum {name}")).expect("swift enum");
    let body = &source[start..start + source[start..].find("\n}").expect("enum end")];
    body.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("case ")?;
            let (case, code) = rest.split_once('=')?;
            Some((
                case.trim().trim_matches('`').to_owned(),
                code.trim().parse().ok()?,
            ))
        })
        .collect()
}

/// `<PREFIX><NAME> = <code>` constants of the C header.
fn c_constants(header: &str, prefix: &str) -> Vec<(String, i64)> {
    header
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(prefix)?;
            let (name, code) = rest.split_once('=')?;
            Some((
                name.trim().to_owned(),
                code.trim().trim_end_matches(',').parse().ok()?,
            ))
        })
        .collect()
}

#[test]
fn the_entrypoint_lists_every_export() {
    let cdecl = read("crates/ios/src/cdecl_exports.rs").expect("cdecl_exports.rs");
    let frames = read("crates/ios/src/native_buffer.rs").expect("native_buffer.rs");
    let entry = read("crates/ios/src/entrypoint.rs").expect("entrypoint.rs");
    let mut all = exports(&cdecl);
    all.extend(exports(&frames));
    let listed: BTreeSet<String> = entry
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '}')
        .filter(|t| t.starts_with("istmo_"))
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(
        listed, all,
        "entrypoint.rs (what the app links) vs the crate's exports"
    );
}

#[test]
fn status_codes_match_in_swift_and_c() {
    let (Some(swift), Some(header)) = (
        read("runtime/ios/Sources/IstmoRuntime/IstmoFrameSender.swift"),
        read("runtime/ios/Sources/IstmoFFI/include/istmo_ffi.h"),
    ) else {
        return;
    };
    let rust_camel: Vec<(String, i64)> = SubmitStatus::ALL
        .iter()
        .map(|s| (camel(s.name()), i64::from(s.code())))
        .collect();
    assert_eq!(
        swift_cases(&swift, "IstmoSubmitStatus"),
        rust_camel,
        "Swift statuses"
    );
    let rust_screaming: Vec<(String, i64)> = SubmitStatus::ALL
        .iter()
        .map(|s| (s.name().to_owned(), i64::from(s.code())))
        .collect();
    assert_eq!(
        c_constants(&header, "ISTMO_SUBMIT_"),
        rust_screaming,
        "C statuses"
    );

    let kinds = |rename: fn(&str) -> String| -> Vec<(String, i64)> {
        RetainedKind::ALL
            .iter()
            .map(|(kind, tag)| {
                let name = match kind {
                    RetainedKind::Other => "OTHER",
                    RetainedKind::CvPixelBuffer => "CV_PIXEL_BUFFER",
                    RetainedKind::IoSurface => "IO_SURFACE",
                    RetainedKind::CmSampleBuffer => "CM_SAMPLE_BUFFER",
                    _ => "UNKNOWN",
                };
                (rename(name), i64::from(*tag))
            })
            .collect()
    };
    assert_eq!(
        c_constants(&header, "ISTMO_RETAINED_"),
        kinds(str::to_owned),
        "C kinds"
    );
    assert_eq!(
        swift_cases(&swift, "IstmoRetainedKind"),
        kinds(camel),
        "Swift kinds"
    );
    for (kind, tag) in RetainedKind::ALL {
        assert_eq!(RetainedKind::from_tag(tag), kind);
    }
    assert!(
        swift.contains(&format!(
            "static let abiVersion: UInt32 = {NATIVE_ABI_VERSION}"
        )),
        "Swift abiVersion must be {NATIVE_ABI_VERSION}"
    );
}
