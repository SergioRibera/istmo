//! Filesystem helpers for files istmo-build generates.
//!
//! Every generated file goes through [`write_if_changed`]: an unchanged
//! file keeps its mtime, so Gradle, Xcode and Cargo's own
//! `rerun-if-changed` tracking do not see churn on every build.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes temp files of concurrent writers inside one process
/// (parallel test threads); the pid covers concurrent build scripts.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to `dest` unless it already holds exactly these
/// bytes, creating parent directories as needed. Returns whether the
/// file was (re)written.
///
/// The bytes land in a temp file next to `dest` that is then renamed
/// over it, so a reader never observes a half-written file. An existing
/// file's permissions carry over to the replacement.
pub(crate) fn write_if_changed(dest: &Path, contents: &[u8]) -> io::Result<bool> {
    if fs::read(dest).is_ok_and(|existing| existing == contents) {
        return Ok(false);
    }
    let parent = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = dest.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", dest.display()),
        )
    })?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = parent.join(temp_name);
    let result = fs::write(&temp, contents).and_then(|()| {
        if let Ok(meta) = fs::metadata(dest) {
            fs::set_permissions(&temp, meta.permissions())?;
        }
        fs::rename(&temp, dest)
    });
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(true)
}

/// [`write_if_changed`] for files inside generated directories: an I/O
/// failure becomes a `cargo::warning` instead of failing the build, since
/// the next build retries and a stale generated file is recoverable.
pub(crate) fn write_generated(dest: &Path, contents: &str) {
    if let Err(error) = write_if_changed(dest, contents.as_bytes()) {
        crate::directive::warning(&format!("failed to write {}: {error}", dest.display()));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("istmo-fsx-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_parents_and_reports_writes() {
        let dir = scratch("create");
        let dest = dir.join("a/b/file.txt");
        assert!(write_if_changed(&dest, b"one").unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"one");
        assert!(write_if_changed(&dest, b"two").unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"two");
    }

    #[test]
    fn unchanged_contents_keep_the_mtime() {
        let dir = scratch("mtime");
        let dest = dir.join("file.txt");
        write_if_changed(&dest, b"same").unwrap();
        let before = fs::metadata(&dest).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!write_if_changed(&dest, b"same").unwrap());
        assert_eq!(fs::metadata(&dest).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let dir = scratch("temp");
        write_if_changed(&dir.join("file.txt"), b"x").unwrap();
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![OsString::from("file.txt")]);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perms");
        let dest = dir.join("run.sh");
        fs::write(&dest, "old").unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        write_if_changed(&dest, b"new").unwrap();
        assert_eq!(
            fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
