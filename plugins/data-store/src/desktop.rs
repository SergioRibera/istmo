//! File-backed desktop backend for the data-store plugin.
//!
//! Persists a wire-encoded `HashMap<String, StoredValue>` per namespace
//! under [`istmo_core::path::preference_dir`] — the same directory
//! semantics `SharedPreferences` covers on Android and `UserDefaults`
//! covers on Apple platforms:
//!
//! * **Windows** — `%APPDATA%\<bundle-id>\config\data-store\<namespace>.bin`.
//! * **macOS** — `~/Library/Preferences/<bundle-id>/data-store/<namespace>.bin`.
//! * **Linux** — `$XDG_CONFIG_HOME/<bundle-id>/data-store/<namespace>.bin`,
//!   with `~/.config/…` as the default fallback.
//!
//! The `<bundle-id>` segment comes from `ISTMO_APP_BUNDLE_ID` baked by
//! `istmo-build::emit_app`; unset apps fall back to `CARGO_PKG_NAME`.
//!
//! `<namespace>` is percent-encoded (every byte outside `[a-z0-9._-]`
//! becomes `%XX`, uppercase ASCII letters included), so distinct
//! namespaces map to distinct files even on case-insensitive
//! filesystems (macOS, Windows).
//!
//! Earlier versions kept uppercase letters verbatim. A namespace whose
//! file under the current name does not exist yet adopts the file under
//! that legacy name, but only when a directory entry carries exactly
//! the legacy name: on a case-insensitive filesystem the legacy file
//! of `Settings` is also the file of `settings`, and a namespace must
//! not pick up a sibling's values just because their names differ only
//! in case. The legacy file is copied, and removed afterwards only when
//! no all-lowercase sibling can resolve to it.
//!
//! Register the backend through the macro-emitted stateful host — the
//! plugin's `init: DataStoreConfig` payload arrives as `CreateInstance`
//! and yields one [`DesktopDataStore`] per instance:
//!
//! ```ignore
//! use istmo_data_store::{DataStoreHost, DesktopDataStoreFactory};
//!
//! runtime.register_host(DataStoreHost::new(DesktopDataStoreFactory));
//! ```
//!
//! Instances opened for the same namespace in one process share a
//! single in-memory map, so neither overwrites the other's writes with
//! a stale copy. Separate processes writing the same namespace are not
//! coordinated.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::future::Future;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use istmo_core::path;
use istmo_core::sync::lock;
use wincode::{SchemaRead, SchemaWrite};

use crate::{DataStore, DataStoreConfig, DataStoreError, DataStoreFactory};

const DIR_NAME: &str = "data-store";
const FILE_EXT: &str = "bin";

#[derive(Debug, Clone, SchemaWrite, SchemaRead)]
enum StoredValue {
    Str(String),
    I64(i64),
    F64(f64),
    Bool(bool),
    Bytes(Vec<u8>),
}

impl StoredValue {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Str(_) => "string",
            Self::I64(_) => "i64",
            Self::F64(_) => "f64",
            Self::Bool(_) => "bool",
            Self::Bytes(_) => "bytes",
        }
    }
}

/// One namespace file and its in-memory copy, shared by every
/// [`DesktopDataStore`] opened for that file in this process.
#[derive(Debug)]
struct SharedFile {
    path: PathBuf,
    values: Mutex<HashMap<String, StoredValue>>,
}

/// Live files by path. Weak, so a namespace nobody holds is reloaded
/// from disk on the next open.
fn open_files() -> &'static Mutex<HashMap<PathBuf, Weak<SharedFile>>> {
    static FILES: OnceLock<Mutex<HashMap<PathBuf, Weak<SharedFile>>>> = OnceLock::new();
    FILES.get_or_init(Mutex::default)
}

/// File-backed [`DataStore`] implementation for desktop targets.
///
/// One namespace = one file. The file is loaded (if present) the first
/// time a namespace is opened and cached in memory. Every mutation is
/// applied to a copy of the map, flushed to disk atomically (temp file,
/// `fsync`, rename) and only then made visible, so a failed write
/// leaves memory and disk agreeing.
#[derive(Debug)]
pub struct DesktopDataStore {
    file: Arc<SharedFile>,
    /// Set when the namespace could not be opened; every call then
    /// fails with it instead of acting on an empty map.
    failure: Option<DataStoreError>,
}

impl DesktopDataStore {
    /// Open (or create) the on-disk store for `config.namespace` under
    /// the resolved [`base_dir`]. Missing files start empty.
    ///
    /// # Errors
    ///
    /// Returns [`DataStoreError::Backend`] when the base directory
    /// cannot be created or an existing file is unreadable, and
    /// [`DataStoreError::Corrupted`] when it does not decode.
    pub fn open(config: &DataStoreConfig) -> Result<Self, DataStoreError> {
        let dir = base_dir();
        fs::create_dir_all(&dir).map_err(|err| {
            DataStoreError::Backend(format!("create data-store dir {}: {err}", dir.display()))
        })?;
        let path = namespace_path(&dir, &config.namespace);
        let legacy = legacy_namespace_path(&dir, &config.namespace);
        let legacy = (legacy != path).then_some(legacy);
        Self::open_with_legacy(path, legacy)
    }

    #[cfg(test)]
    fn open_path(path: PathBuf) -> Result<Self, DataStoreError> {
        Self::open_with_legacy(path, None)
    }

    /// Open `path`, adopting the file at `legacy` first when `path` does
    /// not exist yet. Runs under the open-files lock, so no instance of
    /// this process writes `path` while the adoption decides.
    fn open_with_legacy(path: PathBuf, legacy: Option<PathBuf>) -> Result<Self, DataStoreError> {
        let mut files = lock(open_files());
        if let Some(file) = files.get(&path).and_then(Weak::upgrade) {
            return Ok(Self {
                file,
                failure: None,
            });
        }
        if let Some(legacy) = legacy {
            adopt_legacy(&path, &legacy);
        }
        let values = load(&path)?;
        let file = Arc::new(SharedFile {
            path: path.clone(),
            values: Mutex::new(values),
        });
        files.retain(|_, weak| weak.strong_count() > 0);
        files.insert(path, Arc::downgrade(&file));
        drop(files);
        Ok(Self {
            file,
            failure: None,
        })
    }

    /// A store that fails every call with `failure`.
    fn failed(path: PathBuf, failure: DataStoreError) -> Self {
        Self {
            file: Arc::new(SharedFile {
                path,
                values: Mutex::default(),
            }),
            failure: Some(failure),
        }
    }

    /// Filesystem path this instance persists to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.file.path
    }

    fn read<T>(
        &self,
        f: impl FnOnce(&HashMap<String, StoredValue>) -> T,
    ) -> Result<T, DataStoreError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        Ok(f(&lock(&self.file.values)))
    }

    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut HashMap<String, StoredValue>) -> T,
    ) -> Result<T, DataStoreError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        // The guard is held across the write so concurrent mutations of
        // the same namespace serialise and none is persisted from a
        // stale copy.
        let mut values = lock(&self.file.values);
        let mut next = values.clone();
        let out = f(&mut next);
        persist(&self.file.path, &next)?;
        *values = next;
        drop(values);
        Ok(out)
    }
}

fn corrupted<T>(key: &str, value: &StoredValue, wanted: &str) -> Result<T, DataStoreError> {
    Err(DataStoreError::Corrupted(format!(
        "key `{key}` holds {} but caller asked for {wanted}",
        value.kind()
    )))
}

impl DataStore for DesktopDataStore {
    fn get_string(
        &self,
        key: String,
    ) -> impl Future<Output = Result<Option<String>, DataStoreError>> + Send + '_ {
        async move {
            match self.read(|m| m.get(&key).cloned())? {
                None => Ok(None),
                Some(StoredValue::Str(s)) => Ok(Some(s)),
                Some(other) => corrupted(&key, &other, "string"),
            }
        }
    }

    fn set_string(
        &self,
        key: String,
        value: String,
    ) -> impl Future<Output = Result<(), DataStoreError>> + Send + '_ {
        async move {
            self.mutate(|m| {
                m.insert(key, StoredValue::Str(value));
            })
        }
    }

    fn get_i64(
        &self,
        key: String,
    ) -> impl Future<Output = Result<Option<i64>, DataStoreError>> + Send + '_ {
        async move {
            match self.read(|m| m.get(&key).cloned())? {
                None => Ok(None),
                Some(StoredValue::I64(n)) => Ok(Some(n)),
                Some(other) => corrupted(&key, &other, "i64"),
            }
        }
    }

    fn set_i64(
        &self,
        key: String,
        value: i64,
    ) -> impl Future<Output = Result<(), DataStoreError>> + Send + '_ {
        async move {
            self.mutate(|m| {
                m.insert(key, StoredValue::I64(value));
            })
        }
    }

    fn get_f64(
        &self,
        key: String,
    ) -> impl Future<Output = Result<Option<f64>, DataStoreError>> + Send + '_ {
        async move {
            match self.read(|m| m.get(&key).cloned())? {
                None => Ok(None),
                Some(StoredValue::F64(n)) => Ok(Some(n)),
                Some(other) => corrupted(&key, &other, "f64"),
            }
        }
    }

    fn set_f64(
        &self,
        key: String,
        value: f64,
    ) -> impl Future<Output = Result<(), DataStoreError>> + Send + '_ {
        async move {
            self.mutate(|m| {
                m.insert(key, StoredValue::F64(value));
            })
        }
    }

    fn get_bool(
        &self,
        key: String,
    ) -> impl Future<Output = Result<Option<bool>, DataStoreError>> + Send + '_ {
        async move {
            match self.read(|m| m.get(&key).cloned())? {
                None => Ok(None),
                Some(StoredValue::Bool(b)) => Ok(Some(b)),
                Some(other) => corrupted(&key, &other, "bool"),
            }
        }
    }

    fn set_bool(
        &self,
        key: String,
        value: bool,
    ) -> impl Future<Output = Result<(), DataStoreError>> + Send + '_ {
        async move {
            self.mutate(|m| {
                m.insert(key, StoredValue::Bool(value));
            })
        }
    }

    fn get_bytes(
        &self,
        key: String,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, DataStoreError>> + Send + '_ {
        async move {
            match self.read(|m| m.get(&key).cloned())? {
                None => Ok(None),
                Some(StoredValue::Bytes(b)) => Ok(Some(b)),
                Some(other) => corrupted(&key, &other, "bytes"),
            }
        }
    }

    fn set_bytes(
        &self,
        key: String,
        value: Vec<u8>,
    ) -> impl Future<Output = Result<(), DataStoreError>> + Send + '_ {
        async move {
            self.mutate(|m| {
                m.insert(key, StoredValue::Bytes(value));
            })
        }
    }

    fn remove(
        &self,
        key: String,
    ) -> impl Future<Output = Result<bool, DataStoreError>> + Send + '_ {
        async move { self.mutate(|m| m.remove(&key).is_some()) }
    }

    fn contains(
        &self,
        key: String,
    ) -> impl Future<Output = Result<bool, DataStoreError>> + Send + '_ {
        async move { self.read(|m| m.contains_key(&key)) }
    }

    fn keys(&self) -> impl Future<Output = Result<Vec<String>, DataStoreError>> + Send + '_ {
        async move {
            self.read(|m| {
                let mut ks: Vec<String> = m.keys().cloned().collect();
                ks.sort();
                ks
            })
        }
    }

    fn clear(&self) -> impl Future<Output = Result<(), DataStoreError>> + Send + '_ {
        async move {
            self.mutate(|m| {
                m.clear();
            })
        }
    }
}

/// Factory that instantiates a [`DesktopDataStore`] for every
/// `CreateInstance` frame carrying a [`DataStoreConfig`].
///
/// Stateless; each namespace it sees results in one backend rooted at
/// its own file. Register with the macro-emitted
/// [`DataStoreHost`](crate::DataStoreHost) via
/// `DataStoreHost::new(DesktopDataStoreFactory)`.
///
/// A namespace file that no longer decodes is moved aside to
/// `<namespace>.bin.corrupt` and the namespace starts empty. Any other
/// open failure yields an instance whose every call fails with it.
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopDataStoreFactory;

impl DataStoreFactory for DesktopDataStoreFactory {
    type Instance = DesktopDataStore;

    fn create(&self, config: DataStoreConfig) -> DesktopDataStore {
        let path = namespace_path(&base_dir(), &config.namespace);
        let err = match DesktopDataStore::open(&config) {
            Ok(store) => return store,
            Err(DataStoreError::Corrupted(reason)) => {
                let aside = sibling_path(&path, ".corrupt");
                tracing::warn!(
                    namespace = %config.namespace,
                    %reason,
                    moved_to = %aside.display(),
                    "data-store file is corrupt; starting the namespace empty",
                );
                match fs::rename(&path, &aside) {
                    Ok(()) => match DesktopDataStore::open(&config) {
                        Ok(store) => return store,
                        Err(err) => err,
                    },
                    Err(err) => DataStoreError::Backend(format!(
                        "move corrupt {} aside: {err}",
                        path.display()
                    )),
                }
            }
            Err(err) => err,
        };
        tracing::warn!(namespace = %config.namespace, %err, "data-store namespace unavailable");
        DesktopDataStore::failed(path, err)
    }
}

/// Directory that houses every data-store namespace file for the
/// current app. Nested under [`istmo_core::path::preference_dir`] so it
/// tracks the same semantics as `SharedPreferences` / `UserDefaults` on
/// mobile.
#[must_use]
pub fn base_dir() -> PathBuf {
    path::preference_dir().join(DIR_NAME)
}

fn namespace_path(dir: &Path, namespace: &str) -> PathBuf {
    dir.join(format!("{}.{FILE_EXT}", encode_namespace(namespace)))
}

/// Path earlier versions used for `namespace`: uppercase kept verbatim.
fn legacy_namespace_path(dir: &Path, namespace: &str) -> PathBuf {
    dir.join(format!(
        "{}.{FILE_EXT}",
        encode_namespace_with(namespace, false)
    ))
}

/// Copy the legacy file of a namespace to `path`, when `path` does not
/// exist and a directory entry is named exactly like `legacy`.
///
/// Requiring the exact entry name keeps a namespace from adopting the
/// file of a sibling that differs only in case on a case-insensitive
/// filesystem (`Settings` must not adopt `settings.bin`). The legacy
/// file is removed afterwards only when its all-lowercase spelling does
/// not resolve to a file: on a case-insensitive filesystem it does, and
/// the file then still belongs to the all-lowercase namespace.
///
/// Best effort: on any failure the namespace opens under `path` as is.
fn adopt_legacy(path: &Path, legacy: &Path) {
    if path.try_exists().unwrap_or(true) {
        return;
    }
    let (Some(dir), Some(legacy_name)) = (legacy.parent(), legacy.file_name()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut folded_match = false;
    let mut exact_match = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == legacy_name {
            exact_match = true;
            break;
        }
        folded_match |= name.eq_ignore_ascii_case(legacy_name);
    }
    if !exact_match {
        if folded_match {
            tracing::info!(
                path = %path.display(),
                legacy = %legacy.display(),
                "legacy data-store file differs in case; not adopting it",
            );
        }
        return;
    }
    let values = match load(legacy) {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!(legacy = %legacy.display(), %err, "legacy data-store file unreadable; not adopting it");
            return;
        }
    };
    if let Err(err) = persist(path, &values) {
        tracing::warn!(legacy = %legacy.display(), %err, "copying legacy data-store file failed");
        return;
    }
    let lower = legacy.with_file_name(legacy_name.to_ascii_lowercase());
    if !lower.try_exists().unwrap_or(true) {
        let _ = fs::remove_file(legacy);
    }
}

/// Filesystem-safe file stem for `namespace`, injective even when the
/// filesystem ignores ASCII case.
///
/// Bytes outside `[a-z0-9._-]` — `%` and uppercase letters included —
/// become `%XX` (hex digits uppercase, and `%` never stays verbatim), so
/// two namespaces never share a file: `a/b` and `a_b` stay apart, and so
/// do `Settings` and `settings` on macOS and Windows. A name Windows
/// reserves for devices (`CON`, `NUL`, `COM1`, ...) gets its first byte
/// escaped too. Windows matches those names on the part before the first
/// dot (`nul.cache` names the device as well), so that is what is checked.
fn encode_namespace(namespace: &str) -> String {
    encode_namespace_with(namespace, true)
}

fn encode_namespace_with(namespace: &str, escape_upper: bool) -> String {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
        "LPT9",
    ];
    let base = namespace.split('.').next().unwrap_or_default();
    // Windows also ignores trailing spaces here (`nul .x`); a space is
    // escaped anyway, but the base still names the device.
    let base = base.trim_end_matches(' ');
    let reserved = RESERVED.iter().any(|r| r.eq_ignore_ascii_case(base));
    let mut out = String::with_capacity(namespace.len());
    for (i, byte) in namespace.bytes().enumerate() {
        let plain = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-')
            || (!escape_upper && byte.is_ascii_uppercase());
        if plain && !(reserved && i == 0) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn load(path: &Path) -> Result<HashMap<String, StoredValue>, DataStoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => {
            return Err(DataStoreError::Backend(format!(
                "read {}: {err}",
                path.display()
            )));
        }
    };
    if bytes.is_empty() {
        return Ok(HashMap::new());
    }
    let (values, _) = istmo_core::codec::decode::<HashMap<String, StoredValue>>(&bytes)
        .map_err(|err| DataStoreError::Corrupted(format!("decode {}: {err}", path.display())))?;
    Ok(values)
}

fn persist(path: &Path, values: &HashMap<String, StoredValue>) -> Result<(), DataStoreError> {
    let bytes = istmo_core::codec::encode(values)
        .map_err(|err| DataStoreError::Backend(format!("encode {}: {err}", path.display())))?;
    let tmp = sibling_path(path, ".tmp");
    // `sync_all` before the rename: otherwise a crash can leave the
    // rename durable but the data not, i.e. an empty or torn file.
    fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .map_err(|err| DataStoreError::Backend(format!("write {}: {err}", tmp.display())))?;
    match fs::rename(&tmp, path) {
        Ok(()) => sync_parent_dir(path),
        // Windows refuses `rename` when the destination is opened by
        // another handle; fall back to copy + remove.
        Err(err) if err.kind() == ErrorKind::PermissionDenied && cfg!(target_os = "windows") => {
            fs::copy(&tmp, path)
                .and_then(|_| fs::remove_file(&tmp))
                .map(|_| ())
                .map_err(|err| {
                    DataStoreError::Backend(format!("atomic replace {}: {err}", path.display()))
                })
        }
        Err(err) => Err(DataStoreError::Backend(format!(
            "rename {} -> {}: {err}",
            tmp.display(),
            path.display()
        ))),
    }
}

/// Make the rename itself durable. Unix only: Windows cannot open a
/// directory as a file, and NTFS journals the rename.
fn sync_parent_dir(path: &Path) -> Result<(), DataStoreError> {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|err| DataStoreError::Backend(format!("sync {}: {err}", dir.display())))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    file_name.push(suffix);
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("istmo-data-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn namespace_encoding_is_injective_and_path_safe() {
        assert_eq!(encode_namespace("com.example.app"), "com.example.app");
        assert_eq!(encode_namespace("a/b"), "a%2Fb");
        assert_eq!(encode_namespace("a_b"), "a_b");
        assert_eq!(encode_namespace("a%2Fb"), "a%252%46b");
        assert_eq!(encode_namespace("..\\x:y"), "..%5Cx%3Ay");
        assert_eq!(encode_namespace("nul"), "%6Eul");
        assert_eq!(encode_namespace("Com1.cache"), "%43om1.cache");
        assert_eq!(encode_namespace("aux .x"), "%61ux%20.x");
        assert_eq!(encode_namespace("console"), "console");
        assert_eq!(encode_namespace("é"), "%C3%A9");
        assert_eq!(encode_namespace("Settings"), "%53ettings");
        assert_eq!(encode_namespace("%53ettings"), "%2553ettings");
        assert_eq!(encode_namespace_with("Settings", false), "Settings");
    }

    #[test]
    fn namespace_encoding_survives_case_folding() {
        let names = [
            "settings",
            "Settings",
            "SETTINGS",
            "sEttings",
            "%53ettings",
            "nul",
            "NUL",
        ];
        let mut seen = std::collections::HashSet::new();
        for name in names {
            assert!(
                seen.insert(encode_namespace(name).to_ascii_lowercase()),
                "{name} collides after case folding"
            );
        }
    }

    fn write_values(path: &Path, key: &str, value: &str) {
        let mut values = HashMap::new();
        values.insert(key.to_owned(), StoredValue::Str(value.to_owned()));
        persist(path, &values).unwrap();
    }

    fn open_ns(dir: &Path, namespace: &str) -> DesktopDataStore {
        let path = namespace_path(dir, namespace);
        let legacy = legacy_namespace_path(dir, namespace);
        let legacy = (legacy != path).then_some(legacy);
        DesktopDataStore::open_with_legacy(path, legacy).unwrap()
    }

    #[test]
    fn legacy_file_with_exact_name_is_adopted() {
        let dir = temp_dir("legacy-exact");
        let legacy = dir.join("Settings.bin");
        write_values(&legacy, "theme", "dark");

        let store = open_ns(&dir, "Settings");
        assert_eq!(store.path(), dir.join("%53ettings.bin"));
        assert_eq!(
            pollster::block_on(store.get_string("theme".to_owned())).unwrap(),
            Some("dark".to_owned())
        );
        let lower_exists = dir.join("settings.bin").exists();
        // Removed unless the filesystem folds case, where it doubles as
        // the all-lowercase namespace's file.
        assert_eq!(legacy.exists(), lower_exists);
        drop(store);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_file_differing_in_case_is_not_adopted() {
        let dir = temp_dir("legacy-folded");
        let sibling = dir.join("settings.bin");
        write_values(&sibling, "owner", "lowercase");

        let store = open_ns(&dir, "Settings");
        assert_eq!(
            pollster::block_on(store.keys()).unwrap(),
            Vec::<String>::new()
        );
        drop(store);
        assert!(sibling.exists());

        let lower = open_ns(&dir, "settings");
        assert_eq!(
            pollster::block_on(lower.get_string("owner".to_owned())).unwrap(),
            Some("lowercase".to_owned())
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_new_file_wins_over_legacy() {
        let dir = temp_dir("legacy-shadowed");
        write_values(&dir.join("Settings.bin"), "k", "legacy");
        write_values(&dir.join("%53ettings.bin"), "k", "current");

        let store = open_ns(&dir, "Settings");
        assert_eq!(
            pollster::block_on(store.get_string("k".to_owned())).unwrap(),
            Some("current".to_owned())
        );
        assert!(dir.join("Settings.bin").exists());
        drop(store);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_persists_across_reopen() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("roundtrip.bin");

        let store = DesktopDataStore::open_path(path.clone()).unwrap();
        pollster::block_on(store.set_string("k".to_owned(), "v".to_owned())).unwrap();
        pollster::block_on(store.set_i64("n".to_owned(), 42)).unwrap();
        drop(store);

        let reopened = DesktopDataStore::open_path(path).unwrap();
        assert_eq!(
            pollster::block_on(reopened.get_string("k".to_owned())).unwrap(),
            Some("v".to_owned())
        );
        assert_eq!(
            pollster::block_on(reopened.get_i64("n".to_owned())).unwrap(),
            Some(42)
        );
        assert_eq!(
            pollster::block_on(reopened.keys()).unwrap(),
            vec!["k".to_owned(), "n".to_owned()]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn instances_of_one_namespace_share_state() {
        let dir = temp_dir("shared");
        let path = dir.join("shared.bin");

        let a = DesktopDataStore::open_path(path.clone()).unwrap();
        let b = DesktopDataStore::open_path(path).unwrap();
        pollster::block_on(a.set_string("from_a".to_owned(), "1".to_owned())).unwrap();
        pollster::block_on(b.set_string("from_b".to_owned(), "2".to_owned())).unwrap();
        assert_eq!(
            pollster::block_on(a.keys()).unwrap(),
            vec!["from_a".to_owned(), "from_b".to_owned()]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_write_leaves_memory_untouched() {
        let dir = temp_dir("failed-write");
        // A directory where the file should be makes the rename fail.
        let path = dir.join("blocked.bin");
        let store = DesktopDataStore::open_path(path.clone()).unwrap();
        fs::create_dir_all(path.join("occupied")).unwrap();

        assert!(pollster::block_on(store.set_i64("n".to_owned(), 1)).is_err());
        assert_eq!(
            pollster::block_on(store.get_i64("n".to_owned())).unwrap(),
            None
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_reports_corrupted() {
        let dir = temp_dir("corrupt");
        let path = dir.join("corrupt.bin");
        fs::write(&path, [0xff, 0xff, 0xff]).unwrap();
        let err = DesktopDataStore::open_path(path).unwrap_err();
        assert!(matches!(err, DataStoreError::Corrupted(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn type_mismatch_reports_corrupted() {
        let dir = temp_dir("mismatch");
        let store = DesktopDataStore::open_path(dir.join("mismatch.bin")).unwrap();
        pollster::block_on(store.set_string("k".to_owned(), "v".to_owned())).unwrap();
        let err = pollster::block_on(store.get_i64("k".to_owned())).unwrap_err();
        assert!(matches!(err, DataStoreError::Corrupted(_)));
        let _ = fs::remove_dir_all(&dir);
    }
}
