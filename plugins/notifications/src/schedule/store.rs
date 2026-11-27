//! The engine's entries on disk.
//!
//! Layout: one format byte ([`STORE_FORMAT`]), then the wire encoding
//! (wincode, bincode 2 `standard()` layout) of a
//! `Vec<(Notification, Trigger, i64, Option<i64>)>`: notification,
//! trigger, anchor and next fire time of every entry.
//!
//! Writes replace the file atomically (a sibling temporary file, synced,
//! then renamed over it), so a crash leaves the old or the new file.
//!
//! Single writer: one process owns the store. On desktop that is the
//! instance owning the app's single-instance endpoint
//! (`istmo_plugins::local_endpoint`); a second instance that forwards
//! its arguments and exits must not register a host with a store
//! ([`crate::DesktopNotifications`] builder `without_store`). Two
//! writers would each overwrite the other's entries.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use istmo_core::codec;

use super::Entry;
use crate::{Notification, Trigger};

/// Version of the file layout. A file with another first byte is
/// ignored (and replaced on the next write).
pub const STORE_FORMAT: u8 = 1;

/// One entry as stored.
type Stored = (Notification, Trigger, i64, Option<i64>);

/// Why the store could not be read or written.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// Reading, writing or renaming `path` failed.
    Io {
        /// The file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
    /// The file has another format byte.
    Format {
        /// The byte found.
        found: u8,
    },
    /// The entries do not decode (or encode).
    Codec(istmo_core::CodecError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "notification store {}: {source}", path.display())
            }
            Self::Format { found } => write!(
                f,
                "notification store format {found}, expected {STORE_FORMAT}"
            ),
            Self::Codec(err) => write!(f, "notification store codec: {err}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Codec(err) => Some(err),
            Self::Format { .. } => None,
        }
    }
}

/// The store file of one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// The store at `path` (created on the first write).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where it lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Encodes `entries` in the store layout.
    pub fn encode<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Result<Vec<u8>, StoreError> {
        // Borrowed tuples encode exactly like `Stored`: no copy of the
        // notifications.
        let stored: Vec<(&Notification, &Trigger, i64, Option<i64>)> = entries
            .into_iter()
            .map(|entry| {
                (
                    &entry.notification,
                    &entry.trigger,
                    entry.anchor_ms,
                    entry.next_fire_ms,
                )
            })
            .collect();
        let mut out = codec::encode(&stored).map_err(StoreError::Codec)?;
        out.insert(0, STORE_FORMAT);
        Ok(out)
    }

    /// Decodes the store layout.
    pub fn decode(bytes: &[u8]) -> Result<Vec<Entry>, StoreError> {
        let Some((&format, body)) = bytes.split_first() else {
            return Ok(Vec::new());
        };
        if format != STORE_FORMAT {
            return Err(StoreError::Format { found: format });
        }
        let (stored, _) = codec::decode::<Vec<Stored>>(body).map_err(StoreError::Codec)?;
        Ok(stored
            .into_iter()
            .map(|(notification, trigger, anchor_ms, next_fire_ms)| Entry {
                notification,
                trigger,
                anchor_ms,
                next_fire_ms,
            })
            .collect())
    }

    /// The stored entries; none when the file does not exist.
    pub fn load(&self) -> Result<Vec<Entry>, StoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Self::decode(&bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(StoreError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Replaces the file with `entries` (atomically). An empty set
    /// removes the file.
    pub fn save<'a>(&self, entries: impl IntoIterator<Item = &'a Entry>) -> Result<(), StoreError> {
        let mut entries = entries.into_iter().peekable();
        if entries.peek().is_none() {
            return self.write(None);
        }
        let bytes = Self::encode(entries)?;
        self.write(Some(&bytes))
    }

    /// Replaces the file with `encoded` (from [`Self::encode`])
    /// atomically; `None` removes it. On Unix the file is private to the
    /// user (`0600`) and the directory is synced after the rename, so
    /// the new file survives a crash.
    pub fn write(&self, encoded: Option<&[u8]>) -> Result<(), StoreError> {
        let io = |source| StoreError::Io {
            path: self.path.clone(),
            source,
        };
        let Some(bytes) = encoded else {
            return match fs::remove_file(&self.path) {
                Ok(()) => self.sync_dir().map_err(io),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(io(err)),
            };
        };
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(io)?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let written = private_file(&tmp).and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
        if let Err(err) = written {
            let _cleanup = fs::remove_file(&tmp);
            return Err(io(err));
        }
        fs::rename(&tmp, &self.path).map_err(io)?;
        self.sync_dir().map_err(io)
    }

    /// Syncs the directory holding the file (its entry after a rename or
    /// a removal). Directories cannot be opened as files on Windows,
    /// where the rename is already durable.
    fn sync_dir(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}

/// Creates (or truncates) `path`, readable and writable by the user only
/// on Unix: the schedule holds notification texts and user data.
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        options.mode(0o600);
        let file = options.open(path)?;
        // A leftover temporary file keeps its old mode: set it again.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(not(unix))]
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CalendarRule, Notification};

    fn entry(id: &str, trigger: Trigger) -> Entry {
        Entry {
            notification: Notification::new(id, "t").unwrap().with_body("b"),
            trigger,
            anchor_ms: 5,
            next_fire_ms: Some(7),
        }
    }

    #[test]
    fn entries_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("istmo-notif-store-{}", std::process::id()));
        let store = Store::new(dir.join("schedule.bin"));
        assert_eq!(store.load().unwrap(), Vec::new());
        let entries = vec![
            entry("a", Trigger::After(10)),
            entry(
                "b",
                Trigger::Calendar(CalendarRule::daily(8, 0).in_zone("UTC")),
            ),
        ];
        store.save(&entries).unwrap();
        assert_eq!(fs::read(store.path()).unwrap()[0], STORE_FORMAT);
        assert_eq!(store.load().unwrap(), entries);
        store.save(&[]).unwrap();
        assert!(!store.path().exists());
        let _cleanup = fs::remove_dir_all(dir);
    }

    #[test]
    fn other_formats_are_refused() {
        assert!(matches!(
            Store::decode(&[9, 0]),
            Err(StoreError::Format { found: 9 })
        ));
        assert_eq!(Store::decode(&[]).unwrap(), Vec::new());
        assert!(matches!(
            Store::decode(&[STORE_FORMAT, 0xff, 0xff]),
            Err(StoreError::Codec(_))
        ));
    }

    #[test]
    fn borrowed_encoding_matches_the_stored_layout() {
        let entries = [entry("a", Trigger::Every(60_000))];
        let owned: Vec<Stored> = entries
            .iter()
            .map(|e| {
                (
                    e.notification.clone(),
                    e.trigger.clone(),
                    e.anchor_ms,
                    e.next_fire_ms,
                )
            })
            .collect();
        let mut expected = vec![STORE_FORMAT];
        expected.extend(codec::encode(&owned).unwrap());
        assert_eq!(Store::encode(&entries).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("istmo-notif-mode-{}", std::process::id()));
        let store = Store::new(dir.join("schedule.bin"));
        // A leftover temporary file with a wider mode is narrowed.
        fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("schedule.bin.tmp");
        fs::write(&tmp, b"x").unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644)).unwrap();
        store.save(&[entry("a", Trigger::After(1))]).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _cleanup = fs::remove_dir_all(dir);
    }
}
