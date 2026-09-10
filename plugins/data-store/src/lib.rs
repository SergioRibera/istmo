//! Persistent key-value store plugin.
//!
//! Same shape as Flutter's `shared_preferences` — a small typed KV store
//! backed by the platform-native persistent-preferences facility. Designed
//! as a reference for integrating a plugin whose implementation lives on
//! the native side rather than in Rust:
//!
//! * **Android** — `android.content.SharedPreferences` (no external Gradle
//!   dependency).
//! * **iOS** — `Foundation.UserDefaults` (no SPM package).
//!
//! Reference native impls live under `plugins/data-store/native/` for
//! consumer apps to copy into their Gradle / Xcode source sets. The
//! `SharedPreferences` and `UserDefaults` APIs are stable enough that the
//! reference impls double as production code.
//!
//! Wire identifier is `istmo.data_store` under the `istmo.` core namespace.
//!
//! # Example
//!
//! ```no_run
//! # async fn ex(rt: &std::sync::Arc<istmo_core::Runtime>) -> Result<(), istmo_core::IstmoError> {
//! use istmo_data_store::{DataStoreClient, DataStoreConfig};
//!
//! let store = DataStoreClient::from_runtime_with(
//!     rt,
//!     DataStoreConfig { namespace: "app_prefs".to_owned() },
//! ).await?;
//!
//! store.set_string("user_id".to_owned(), "u_42".to_owned()).await?;
//! let uid = store.get_string("user_id".to_owned()).await?;
//! assert_eq!(uid.as_deref(), Some("u_42"));
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "codegen")]
pub mod codegen;

use istmo_macros::{message, plugin};

/// Wire identifier of the data-store plugin.
pub const DATA_STORE_PLUGIN_ID: &str = "istmo.data_store";

// Wire shape lives here as the single source of truth. `build.rs` calls
// [`istmo_build::emit`] which extracts the `Contract` directly from these
// declarations.

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataStoreConfig {
    pub namespace: String,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataStoreError {
    Backend(String),
    Corrupted(String),
}

impl std::fmt::Display for DataStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(msg) => write!(f, "data-store backend error: {msg}"),
            Self::Corrupted(msg) => write!(f, "data-store value corrupted: {msg}"),
        }
    }
}

impl std::error::Error for DataStoreError {}

impl DataStoreConfig {
    /// Convenience constructor. `namespace` is the logical container id —
    /// `SharedPreferences` file name on Android, suite name on iOS. Multiple
    /// namespaces can co-exist inside a single process.
    #[must_use]
    pub fn new(namespace: impl Into<String>) -> Self {
        Self { namespace: namespace.into() }
    }
}

/// Persistent key-value store plugin surface.
#[plugin(
    name = "istmo.data_store",
    init = DataStoreConfig,
    crate = "::istmo_core",
)]
pub trait DataStore {
    async fn get_string(&self, key: String) -> Result<Option<String>, DataStoreError>;
    async fn set_string(&self, key: String, value: String) -> Result<(), DataStoreError>;

    async fn get_i64(&self, key: String) -> Result<Option<i64>, DataStoreError>;
    async fn set_i64(&self, key: String, value: i64) -> Result<(), DataStoreError>;

    async fn get_f64(&self, key: String) -> Result<Option<f64>, DataStoreError>;
    async fn set_f64(&self, key: String, value: f64) -> Result<(), DataStoreError>;

    async fn get_bool(&self, key: String) -> Result<Option<bool>, DataStoreError>;
    async fn set_bool(&self, key: String, value: bool) -> Result<(), DataStoreError>;

    async fn get_bytes(&self, key: String) -> Result<Option<Vec<u8>>, DataStoreError>;
    async fn set_bytes(&self, key: String, value: Vec<u8>) -> Result<(), DataStoreError>;

    /// Returns `true` when `key` existed and was removed.
    async fn remove(&self, key: String) -> Result<bool, DataStoreError>;

    /// Returns whether `key` is present in the namespace.
    async fn contains(&self, key: String) -> Result<bool, DataStoreError>;

    /// Snapshot of every key currently stored. Order is backend-defined —
    /// callers should not rely on it.
    async fn keys(&self) -> Result<Vec<String>, DataStoreError>;

    /// Remove every key/value pair in the namespace.
    async fn clear(&self) -> Result<(), DataStoreError>;
}
