//! File-backed desktop backend for the data-store plugin.
//!
//! Persists a bincoded `HashMap<String, StoredValue>` per namespace
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
//! Register the backend through the macro-emitted stateful host — the
//! plugin's `init: DataStoreConfig` payload arrives as `CreateInstance`
//! and yields one [`DesktopDataStore`] per namespace:
//!
//! ```ignore
//! use istmo_data_store::{DataStoreHost, DesktopDataStoreFactory};
//!
//! runtime.register_host(DataStoreHost::new(DesktopDataStoreFactory));
//! ```

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bincode::{Decode, Encode, config};
use istmo_core::path;

use crate::{DataStore, DataStoreConfig, DataStoreError, DataStoreFactory};

const DIR_NAME: &str = "data-store";
const FILE_EXT: &str = "bin";

#[derive(Debug, Clone, Encode, Decode)]
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

/// File-backed [`DataStore`] implementation for desktop targets.
///
/// One instance = one namespace = one file. Loads the file (if present)
/// once on [`Self::open`] and caches values in memory. Every mutation
/// flushes the whole map to disk atomically (temp + rename).
#[derive(Debug)]
pub struct DesktopDataStore {
    path: PathBuf,
    values: Mutex<HashMap<String, StoredValue>>,
}

impl DesktopDataStore {
    /// Open (or create) the on-disk store for `config.namespace` under
    /// the resolved [`base_dir`]. Missing files start empty.
    ///
    /// # Errors
    ///
    /// Returns [`DataStoreError::Backend`] when the base directory
    /// cannot be created, or when an existing file is unreadable or
    /// bincode-corrupt.
    pub fn open(config: &DataStoreConfig) -> Result<Self, DataStoreError> {
        let dir = base_dir();
        fs::create_dir_all(&dir).map_err(|err| {
            DataStoreError::Backend(format!("create data-store dir {}: {err}", dir.display()))
        })?;
        let path = dir.join(format!(
            "{name}.{FILE_EXT}",
            name = sanitise_namespace(&config.namespace)
        ));
        let values = load(&path)?;
        Ok(Self {
            path,
            values: Mutex::new(values),
        })
    }

    /// Filesystem path this instance persists to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read<T>(&self, f: impl FnOnce(&HashMap<String, StoredValue>) -> T) -> T {
        let guard = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&guard)
    }

    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut HashMap<String, StoredValue>) -> T,
    ) -> Result<T, DataStoreError> {
        let mut guard = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let out = f(&mut guard);
        persist(&self.path, &guard)?;
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
            match self.read(|m| m.get(&key).cloned()) {
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
            match self.read(|m| m.get(&key).cloned()) {
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
            match self.read(|m| m.get(&key).cloned()) {
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
            match self.read(|m| m.get(&key).cloned()) {
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
            match self.read(|m| m.get(&key).cloned()) {
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
        async move { Ok(self.read(|m| m.contains_key(&key))) }
    }

    fn keys(&self) -> impl Future<Output = Result<Vec<String>, DataStoreError>> + Send + '_ {
        async move {
            Ok(self.read(|m| {
                let mut ks: Vec<String> = m.keys().cloned().collect();
                ks.sort();
                ks
            }))
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
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopDataStoreFactory;

impl DataStoreFactory for DesktopDataStoreFactory {
    type Instance = DesktopDataStore;

    fn create(&self, config: DataStoreConfig) -> DesktopDataStore {
        DesktopDataStore::open(&config).unwrap_or_else(|err| {
            panic!(
                "DesktopDataStore::open({namespace:?}) failed: {err}",
                namespace = config.namespace
            )
        })
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

fn sanitise_namespace(namespace: &str) -> String {
    namespace
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
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
    let (values, _) =
        bincode::decode_from_slice::<HashMap<String, StoredValue>, _>(&bytes, config::standard())
            .map_err(|err| {
            DataStoreError::Corrupted(format!("decode {}: {err}", path.display()))
        })?;
    Ok(values)
}

fn persist(path: &Path, values: &HashMap<String, StoredValue>) -> Result<(), DataStoreError> {
    let bytes = bincode::encode_to_vec(values, config::standard())
        .map_err(|err| DataStoreError::Backend(format!("encode {}: {err}", path.display())))?;
    let tmp = tmp_path(path);
    fs::write(&tmp, &bytes)
        .map_err(|err| DataStoreError::Backend(format!("write {}: {err}", tmp.display())))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
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

fn tmp_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    file_name.push(".tmp");
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_replaces_path_separators() {
        assert_eq!(sanitise_namespace("com/example:foo"), "com_example_foo");
    }

    #[test]
    fn roundtrip_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "istmo-data-store-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.bin");

        let store = DesktopDataStore {
            path: path.clone(),
            values: Mutex::new(HashMap::new()),
        };
        pollster::block_on(store.set_string("k".to_owned(), "v".to_owned())).unwrap();
        pollster::block_on(store.set_i64("n".to_owned(), 42)).unwrap();

        let reopened = DesktopDataStore {
            path: path.clone(),
            values: Mutex::new(load(&path).unwrap()),
        };
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
    fn type_mismatch_reports_corrupted() {
        let path = std::env::temp_dir().join(format!(
            "istmo-data-store-mismatch-{}.bin",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let store = DesktopDataStore {
            path: path.clone(),
            values: Mutex::new(HashMap::new()),
        };
        pollster::block_on(store.set_string("k".to_owned(), "v".to_owned())).unwrap();
        let err = pollster::block_on(store.get_i64("k".to_owned())).unwrap_err();
        assert!(matches!(err, DataStoreError::Corrupted(_)));
        let _ = fs::remove_file(&path);
    }
}
