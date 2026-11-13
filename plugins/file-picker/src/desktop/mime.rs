//! MIME type ↔ extension table for the desktop dialogs.
//!
//! `rfd` only filters by extension, so the MIME types in a
//! [`FileFilter`](crate::FileFilter) are translated through this table.
//! It covers the types apps commonly filter on; a MIME type it does not
//! know contributes no extension.

/// `(MIME type, extensions)`; the first extension is the canonical one.
type Row = (&'static str, &'static [&'static str]);

/// Sorted by MIME type, so each top-level type is one contiguous run.
const TABLE: &[Row] = &[
    ("application/epub+zip", &["epub"]),
    ("application/gzip", &["gz"]),
    ("application/json", &["json"]),
    ("application/msword", &["doc"]),
    ("application/pdf", &["pdf"]),
    ("application/rtf", &["rtf"]),
    ("application/vnd.ms-excel", &["xls"]),
    ("application/vnd.ms-powerpoint", &["ppt"]),
    ("application/vnd.oasis.opendocument.presentation", &["odp"]),
    ("application/vnd.oasis.opendocument.spreadsheet", &["ods"]),
    ("application/vnd.oasis.opendocument.text", &["odt"]),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        &["pptx"],
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        &["xlsx"],
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        &["docx"],
    ),
    ("application/vnd.sqlite3", &["sqlite", "db"]),
    ("application/wasm", &["wasm"]),
    ("application/x-7z-compressed", &["7z"]),
    ("application/x-tar", &["tar"]),
    ("application/xml", &["xml"]),
    ("application/zip", &["zip"]),
    ("audio/aac", &["aac"]),
    ("audio/flac", &["flac"]),
    ("audio/mp4", &["m4a"]),
    ("audio/mpeg", &["mp3"]),
    ("audio/ogg", &["ogg", "oga", "opus"]),
    ("audio/wav", &["wav"]),
    ("audio/webm", &["weba"]),
    ("font/otf", &["otf"]),
    ("font/ttf", &["ttf"]),
    ("font/woff", &["woff"]),
    ("font/woff2", &["woff2"]),
    ("image/avif", &["avif"]),
    ("image/bmp", &["bmp"]),
    ("image/gif", &["gif"]),
    ("image/heic", &["heic"]),
    ("image/heif", &["heif"]),
    ("image/jpeg", &["jpg", "jpeg"]),
    ("image/png", &["png"]),
    ("image/svg+xml", &["svg"]),
    ("image/tiff", &["tiff", "tif"]),
    ("image/webp", &["webp"]),
    ("text/calendar", &["ics"]),
    ("text/css", &["css"]),
    ("text/csv", &["csv"]),
    ("text/html", &["html", "htm"]),
    ("text/javascript", &["js", "mjs"]),
    ("text/markdown", &["md", "markdown"]),
    ("text/plain", &["txt", "text", "log"]),
    ("text/tab-separated-values", &["tsv"]),
    ("text/vcard", &["vcf"]),
    ("video/mp4", &["mp4", "m4v"]),
    ("video/mpeg", &["mpeg", "mpg"]),
    ("video/ogg", &["ogv"]),
    ("video/quicktime", &["mov"]),
    ("video/webm", &["webm"]),
    ("video/x-matroska", &["mkv"]),
    ("video/x-msvideo", &["avi"]),
];

/// What a MIME type means for an extension filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MimeExtensions {
    /// `*/*`: the filter accepts every file.
    Any,
    /// The extensions the table knows for the type (empty when it knows
    /// none). A `type/*` wildcard yields every extension of that type.
    Known(&'static [Row]),
}

impl MimeExtensions {
    /// Look up `mime` (case-insensitive, parameters such as `;charset=`
    /// ignored).
    pub(super) fn of(mime: &str) -> Self {
        let essence = mime.split(';').next().unwrap_or_default().trim();
        if essence == "*/*" {
            return Self::Any;
        }
        Self::Known(
            essence
                .strip_suffix("/*")
                .map_or_else(|| exact_rows(essence), top_level_rows),
        )
    }

    /// Every extension of the matched types.
    pub(super) fn extensions(self) -> impl Iterator<Item = &'static str> {
        let rows = match self {
            Self::Any => &[],
            Self::Known(rows) => rows,
        };
        rows.iter().flat_map(|(_, exts)| exts.iter().copied())
    }
}

/// The row for `mime` (case-insensitive), or none.
fn exact_rows(mime: &str) -> &'static [Row] {
    TABLE
        .iter()
        .position(|(m, _)| m.eq_ignore_ascii_case(mime))
        .map_or(&[], |i| &TABLE[i..=i])
}

/// Every row of top-level type `top` — one contiguous run, as `TABLE`
/// is sorted.
fn top_level_rows(top: &str) -> &'static [Row] {
    let start = TABLE
        .iter()
        .position(|(m, _)| starts_with_top(m, top))
        .unwrap_or(TABLE.len());
    let len = TABLE[start..]
        .iter()
        .take_while(|(m, _)| starts_with_top(m, top))
        .count();
    &TABLE[start..start + len]
}

/// `true` when `mime`'s top-level type is `top` (case-insensitive).
fn starts_with_top(mime: &str, top: &str) -> bool {
    mime.split_once('/')
        .is_some_and(|(t, _)| t.eq_ignore_ascii_case(top))
}

/// MIME type of a file with extension `ext` (case-insensitive), when the
/// table knows it.
pub(super) fn for_extension(ext: &str) -> Option<&'static str> {
    TABLE
        .iter()
        .find(|(_, exts)| exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .map(|(mime, _)| *mime)
}

#[cfg(test)]
mod tests {
    use super::{MimeExtensions, TABLE, for_extension};

    fn exts(mime: &str) -> Vec<&'static str> {
        MimeExtensions::of(mime).extensions().collect()
    }

    #[test]
    fn table_is_sorted_and_lowercase() {
        assert!(
            TABLE.windows(2).all(|w| w[0].0 < w[1].0),
            "TABLE must stay sorted"
        );
        for (mime, extensions) in TABLE {
            assert_eq!(*mime, mime.to_ascii_lowercase());
            assert!(!extensions.is_empty(), "{mime}");
        }
    }

    #[test]
    fn exact_type() {
        assert_eq!(exts("application/pdf"), ["pdf"]);
        assert_eq!(exts("Image/JPEG"), ["jpg", "jpeg"]);
        assert_eq!(exts("text/plain; charset=utf-8"), ["txt", "text", "log"]);
    }

    #[test]
    fn wildcard_covers_the_whole_top_level_type() {
        let images = exts("image/*");
        assert!(images.contains(&"png") && images.contains(&"svg"));
        assert!(
            images
                .iter()
                .all(|e| for_extension(e).is_some_and(|m| m.starts_with("image/")))
        );
        assert!(!exts("video/*").is_empty());
    }

    #[test]
    fn any_and_unknown() {
        assert_eq!(MimeExtensions::of("*/*"), MimeExtensions::Any);
        assert!(exts("application/x-unknown").is_empty());
        assert!(exts("chemical/*").is_empty());
    }

    #[test]
    fn reverse_lookup() {
        assert_eq!(for_extension("PNG"), Some("image/png"));
        assert_eq!(for_extension("jpeg"), Some("image/jpeg"));
        assert_eq!(for_extension("unknownext"), None);
    }
}
