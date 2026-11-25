//! What a drag out of the app offers, in the forms other apps read.

// URI lists are for Wayland and X11; other desktops use their own types.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::DragItem;

/// Paths of the files the drag carries.
pub(super) fn files(items: &[DragItem]) -> Vec<PathBuf> {
    items
        .iter()
        .filter_map(|item| match item {
            DragItem::File(file) => Some(PathBuf::from(&file.path)),
            DragItem::Text(_) | DragItem::Url(_) => None,
        })
        .collect()
}

/// Links the drag carries.
pub(super) fn urls(items: &[DragItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            DragItem::Url(url) => Some(url.as_str()),
            DragItem::Text(_) | DragItem::File(_) => None,
        })
        .collect()
}

/// `text/uri-list`: files as `file://` URIs, then the links; `None` when
/// the drag has neither.
pub(super) fn uri_list(items: &[DragItem]) -> Option<String> {
    let mut lines: Vec<String> = files(items).iter().map(|path| file_uri(path)).collect();
    lines.extend(urls(items).into_iter().map(str::to_owned));
    (!lines.is_empty()).then(|| {
        let mut list = lines.join("\r\n");
        list.push_str("\r\n");
        list
    })
}

/// Plain text of the drag: its texts, or else its links, one per line;
/// `None` for files alone.
pub(super) fn plain_text(items: &[DragItem]) -> Option<String> {
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            DragItem::Text(text) => Some(text.as_str()),
            DragItem::Url(_) | DragItem::File(_) => None,
        })
        .collect();
    let lines = if texts.is_empty() { urls(items) } else { texts };
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// `path` as a `file://` URI, every byte outside the unreserved set and
/// `/` percent-encoded.
pub(super) fn file_uri(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    let text = text.replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        // `C:/x` on Windows.
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DragFile;

    #[test]
    fn files_and_links_as_uri_list_and_text() {
        let items = [
            DragItem::File(DragFile {
                path: "/home/me/Mis notas/a ñ.md".into(),
                mime_type: None,
            }),
            DragItem::Url("https://example.com/x".into()),
        ];
        assert_eq!(
            uri_list(&items).as_deref(),
            Some("file:///home/me/Mis%20notas/a%20%C3%B1.md\r\nhttps://example.com/x\r\n")
        );
        assert_eq!(plain_text(&items).as_deref(), Some("https://example.com/x"));
        let text = [
            DragItem::Text("hola".into()),
            DragItem::Url("https://x".into()),
        ];
        assert_eq!(plain_text(&text).as_deref(), Some("hola"));
        assert_eq!(uri_list(&[DragItem::Text("hola".into())]), None);
    }
}
