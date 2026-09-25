//! Reader for the Share Extension hand-off directory
//! (`native/ios/ShareHandoff.swift` writes it).
//!
//! ```text
//! <inbox>/<uuid>/share.json
//! <inbox>/<uuid>/<file>…
//! <inbox>/<uuid>.partial/…   ← still being written, skipped
//! ```
//!
//! Only macOS uses it at runtime (App Group container), but it is plain
//! filesystem code, so it compiles — and is tested — on every desktop.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{IncomingFile, IncomingShare, ShareError, ShareInbox};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    text: Option<String>,
    url: Option<String>,
    subject: Option<String>,
    #[serde(default)]
    files: Vec<ManifestFile>,
    source_app: Option<String>,
    received_at_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    file: String,
    name: String,
    mime_type: Option<String>,
}

/// Publish every complete entry of `inbox_dir` to `inbox`, moving its
/// files under `dest_root/<uuid>/`. Malformed entries are logged and
/// deleted so they cannot wedge the inbox. Returns how many shares were
/// published.
pub(crate) fn drain_dir(
    inbox_dir: &Path,
    dest_root: &Path,
    inbox: &ShareInbox,
) -> Result<usize, ShareError> {
    let entries = match std::fs::read_dir(inbox_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(ShareError::Io(format!("{}: {err}", inbox_dir.display()))),
    };
    let mut published = 0;
    for entry in entries.filter_map(Result::ok) {
        let dir = entry.path();
        if !dir.is_dir() || dir.extension().is_some_and(|ext| ext == "partial") {
            continue;
        }
        match read_entry(&dir, dest_root) {
            Ok(share) => {
                inbox
                    .publish(share)
                    .map_err(|err| ShareError::Backend(err.to_string()))?;
                published += 1;
            }
            Err(err) => {
                tracing::warn!(%err, entry = %dir.display(), "dropping malformed share entry");
            }
        }
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(?err, entry = %dir.display(), "removing drained share entry");
        }
    }
    Ok(published)
}

fn read_entry(dir: &Path, dest_root: &Path) -> Result<IncomingShare, ShareError> {
    let manifest_path = dir.join("share.json");
    let json = std::fs::read(&manifest_path)
        .map_err(|err| ShareError::Io(format!("{}: {err}", manifest_path.display())))?;
    let manifest: Manifest = serde_json::from_slice(&json)
        .map_err(|err| ShareError::Backend(format!("{}: {err}", manifest_path.display())))?;
    let dest = dest_root.join(dir.file_name().unwrap_or_default());
    std::fs::create_dir_all(&dest)
        .map_err(|err| ShareError::Io(format!("{}: {err}", dest.display())))?;
    let files = manifest
        .files
        .into_iter()
        .filter_map(|file| move_file(dir, &dest, file))
        .collect();
    Ok(IncomingShare {
        text: manifest.text,
        url: manifest.url,
        subject: manifest.subject,
        files,
        source_app: manifest.source_app,
        received_at_ms: manifest.received_at_ms,
    })
}

fn move_file(from_dir: &Path, to_dir: &Path, file: ManifestFile) -> Option<IncomingFile> {
    // `file` comes from disk: never let it name anything outside the
    // entry directory.
    let name = Path::new(&file.file).file_name()?;
    let src = from_dir.join(name);
    let dest: PathBuf = to_dir.join(name);
    if std::fs::rename(&src, &dest).is_err() {
        std::fs::copy(&src, &dest).ok()?;
    }
    let size = std::fs::metadata(&dest).ok().map(|m| m.len());
    Some(IncomingFile {
        path: dest.display().to_string(),
        name: file.name,
        mime_type: file.mime_type,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use istmo_core::Runtime;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("istmo-share-handoff-{tag}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn drains_complete_entries_and_skips_partial_ones() {
        let root = scratch("drain");
        let inbox_dir = root.join("inbox");
        let entry = inbox_dir.join("A1");
        std::fs::create_dir_all(&entry).expect("entry");
        std::fs::write(entry.join("photo.png"), b"png").expect("file");
        std::fs::write(
            entry.join("share.json"),
            br#"{"text":"look","url":null,"subject":"Hi","files":[{"file":"photo.png","name":"Photo.png","mimeType":"image/png"},{"file":"../../escape","name":"x"}],"sourceApp":null,"receivedAtMs":42}"#,
        )
        .expect("manifest");
        std::fs::create_dir_all(inbox_dir.join("B2.partial")).expect("partial");

        let init = Runtime::mock();
        let inbox = ShareInbox::from_runtime(&init.runtime).expect("inbox");
        let stream = inbox.stream();
        let dest = root.join("caches");
        assert_eq!(drain_dir(&inbox_dir, &dest, &inbox).expect("drain"), 1);

        let share = stream.try_recv().expect("published").expect("decoded");
        assert_eq!(share.text.as_deref(), Some("look"));
        assert_eq!(share.subject.as_deref(), Some("Hi"));
        assert_eq!(share.received_at_ms, Some(42));
        assert_eq!(share.files.len(), 1, "escaping entry must be ignored");
        let file = &share.files[0];
        assert_eq!(file.name, "Photo.png");
        assert_eq!(file.size, Some(3));
        assert_eq!(std::fs::read(&file.path).expect("moved"), b"png");
        assert!(!entry.exists(), "drained entry is removed");
        assert!(
            inbox_dir.join("B2.partial").exists(),
            "partial entry untouched"
        );

        drop(std::fs::remove_dir_all(&root));
    }

    #[test]
    fn missing_inbox_is_empty() {
        let init = Runtime::mock();
        let inbox = ShareInbox::from_runtime(&init.runtime).expect("inbox");
        let n = drain_dir(Path::new("/nonexistent/istmo"), Path::new("/tmp"), &inbox).expect("ok");
        assert_eq!(n, 0);
    }
}
