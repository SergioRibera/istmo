//! Turning dropped paths into a [`DroppedFiles`]: types, sizes, folders.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{DEFAULT_MAX_BYTES, DropOptions, DroppedFile, DroppedFiles, SkipReason, SkippedFile};

/// MIME type of dropped folders.
const DIRECTORY: &str = "inode/directory";

/// Check every path against `options`, in order.
pub(super) fn collect(paths: Vec<PathBuf>, options: &DropOptions) -> DroppedFiles {
    let limit = if options.max_bytes == 0 {
        DEFAULT_MAX_BYTES
    } else {
        options.max_bytes
    };
    let mut dropped = DroppedFiles::default();
    for path in paths {
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let skip = |reason| SkippedFile {
            display_name: display_name.clone(),
            reason,
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            dropped.skipped.push(skip(SkipReason::Unreadable));
            continue;
        };
        if meta.is_dir() {
            if options.folders {
                dropped.files.push(DroppedFile {
                    path: path.display().to_string(),
                    display_name,
                    mime_type: Some(DIRECTORY.to_owned()),
                    size: 0,
                });
            } else {
                dropped.skipped.push(skip(SkipReason::NotAFile));
            }
            continue;
        }
        if !meta.is_file() {
            dropped.skipped.push(skip(SkipReason::NotAFile));
            continue;
        }
        if meta.len() > limit {
            dropped.skipped.push(skip(SkipReason::TooLarge));
            continue;
        }
        let mime = guess_mime(&path);
        if !accepts(&options.mime_types, mime) {
            dropped.skipped.push(skip(SkipReason::WrongType));
            continue;
        }
        dropped.files.push(DroppedFile {
            path: path.display().to_string(),
            display_name,
            mime_type: mime.map(str::to_owned),
            size: meta.len(),
        });
    }
    dropped
}

/// Skip entries for `count` dragged items that were not local files
/// (links, remote URIs); unnamed, so a link never shows as a file name.
pub(super) fn not_files(count: usize) -> impl Iterator<Item = SkippedFile> {
    (0..count).map(|_| SkippedFile {
        display_name: String::new(),
        reason: SkipReason::NotAFile,
    })
}

/// Whether a file of `mime` passes `accepted` (empty takes all). A type
/// that cannot be told from the name only passes `*/*` and
/// `application/octet-stream`.
fn accepts(accepted: &[String], mime: Option<&str>) -> bool {
    if accepted.is_empty() {
        return true;
    }
    let mime = mime.unwrap_or("application/octet-stream");
    accepted.iter().any(|pattern| mime_matches(mime, pattern))
}

/// `image/png` against `image/png`, `image/*` or `*/*`.
fn mime_matches(mime: &str, pattern: &str) -> bool {
    if pattern == "*/*" || pattern.eq_ignore_ascii_case(mime) {
        return true;
    }
    pattern
        .strip_suffix("/*")
        .zip(mime.split_once('/'))
        .is_some_and(|(kind, (mime_kind, _))| kind.eq_ignore_ascii_case(mime_kind))
}

/// MIME type from the extension, for the types apps commonly filter on.
fn guess_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => "application/pdf",
        "epub" => "application/epub+zip",
        "zip" => "application/zip",
        "json" => "application/json",
        "xml" => "application/xml",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "txt" | "text" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "rtf" => "text/rtf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

/// Local paths in a `text/uri-list` payload, and how many entries were
/// not local files.
pub(super) fn parse_uri_list(list: &str) -> (Vec<PathBuf>, usize) {
    let mut paths = Vec::new();
    let mut others = 0;
    for line in list.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match file_url_path(line) {
            Some(path) => paths.push(path),
            None => others += 1,
        }
    }
    (paths, others)
}

/// The local path of a `file:` URL (`file:///a%20b`, `file://localhost/x`).
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .get(..5)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file:"))
        .map(|_| &url[5..])?;
    let path = match rest.strip_prefix("//") {
        Some(authority_and_path) => {
            let slash = authority_and_path.find('/')?;
            let host = &authority_and_path[..slash];
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return None;
            }
            &authority_and_path[slash..]
        }
        None if rest.starts_with('/') => rest,
        None => return None,
    };
    let decoded = percent_decode(path)?;
    Some(PathBuf::from(decoded))
}

#[cfg(unix)]
fn percent_decode(text: &str) -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;

    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    // A NUL cannot be part of a path; refuse instead of truncating.
    if out.contains(&0) {
        return None;
    }
    Some(std::ffi::OsString::from_vec(out))
}

#[cfg(not(unix))]
fn percent_decode(text: &str) -> Option<std::ffi::OsString> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    let text = String::from_utf8(out).ok()?;
    // `file:///C:/x` names `C:/x`.
    let text = text
        .strip_prefix('/')
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .map_or(text.clone(), str::to_owned);
    Some(text.into())
}

pub(super) fn now_ms() -> Option<u64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_keep_local_files_and_count_the_rest() {
        let (paths, others) = parse_uri_list(
            "# from a file manager\r\nfile:///home/me/a%20b.pdf\r\nfile://localhost/tmp/c\r\n\
             https://example.com/x\r\nfile://other-host/d\r\n\r\n",
        );
        assert_eq!(
            paths,
            [PathBuf::from("/home/me/a b.pdf"), PathBuf::from("/tmp/c")]
        );
        assert_eq!(others, 2);
        assert_eq!(parse_uri_list("file:///a%00b").1, 1, "NUL refused");
        assert_eq!(parse_uri_list("file:///a%zz").1, 1, "bad escape refused");
    }

    #[test]
    fn mime_patterns() {
        assert!(mime_matches("image/png", "image/*"));
        assert!(mime_matches("image/png", "*/*"));
        assert!(mime_matches("application/pdf", "Application/PDF"));
        assert!(!mime_matches("text/plain", "image/*"));
        assert!(accepts(&[], None));
        assert!(!accepts(&["application/pdf".to_owned()], None));
        assert!(accepts(&["application/octet-stream".to_owned()], None));
    }

    #[test]
    fn collect_checks_type_size_and_folders() {
        let root = std::env::temp_dir().join(format!("istmo-drop-collect-{}", std::process::id()));
        std::fs::create_dir_all(root.join("folder")).expect("folder");
        std::fs::write(root.join("a.pdf"), b"pdf").expect("pdf");
        std::fs::write(root.join("b.png"), b"png!").expect("png");
        let paths = vec![
            root.join("a.pdf"),
            root.join("b.png"),
            root.join("folder"),
            root.join("missing.pdf"),
        ];

        let only_pdf = collect(paths.clone(), &DropOptions::accepting(["application/pdf"]));
        assert_eq!(only_pdf.files.len(), 1);
        assert_eq!(
            only_pdf.files[0].mime_type.as_deref(),
            Some("application/pdf")
        );
        let reasons: Vec<_> = only_pdf.skipped.iter().map(|skip| skip.reason).collect();
        assert_eq!(
            reasons,
            [
                SkipReason::WrongType,
                SkipReason::NotAFile,
                SkipReason::Unreadable
            ]
        );

        let small = collect(paths, &DropOptions::any().max_bytes(3).with_folders());
        let names: Vec<_> = small
            .files
            .iter()
            .map(|f| f.display_name.as_str())
            .collect();
        assert_eq!(names, ["a.pdf", "folder"]);
        assert_eq!(small.skipped[0].reason, SkipReason::TooLarge);
        let _ = std::fs::remove_dir_all(&root);
    }
}
