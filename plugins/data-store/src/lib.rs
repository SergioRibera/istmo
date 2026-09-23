//! Namespaced key-value data store plugin for
//! [`istmo`](https://docs.rs/istmo).
//!
//! Rust code sees a single async [`DataStore`] trait; the native side
//! backs it with `SharedPreferences` on Android and `UserDefaults` on
//! Apple platforms. Reference native implementations ship alongside
//! this crate.
//!
//! Enable the `codegen` feature to expose the plugin's `Contract`
//! (see [`istmo-build`](https://docs.rs/istmo-build)) for downstream
//! code generation without going through the build-script handover.

#![doc(html_root_url = "https://docs.rs/istmo-data-store")]

#[cfg(feature = "codegen")]
pub mod codegen;

use istmo_macros::{message, plugin};

/// Wire identifier for the data-store plugin.
pub const DATA_STORE_PLUGIN_ID: &str = "istmo.data_store";

/// Instance-scoped configuration for a [`DataStoreClient`].
///
/// The `namespace` is the isolation boundary the native backend uses
/// to segregate values — on Android it maps to a `SharedPreferences`
/// file name, on iOS to a `UserDefaults` suite name. Two instances
/// with different namespaces cannot see each other's keys.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataStoreConfig {
    /// Backend namespace. Convention: reverse-DNS app id
    /// (`com.example.myapp`) so different apps on the same device
    /// cannot collide.
    pub namespace: String,
}

/// Domain-level errors returned by every [`DataStore`] method.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataStoreError {
    /// Native backend rejected the operation. Carries the platform's
    /// error message verbatim (e.g. `SharedPreferences` disk failure,
    /// `UserDefaults` suite-locked-out).
    Backend(String),
    /// Stored value could not be decoded into the requested type —
    /// usually the sign that a previous version of the app wrote a
    /// different shape under the same key.
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
    #[must_use]
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
        }
    }
}

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

    async fn remove(&self, key: String) -> Result<bool, DataStoreError>;

    async fn contains(&self, key: String) -> Result<bool, DataStoreError>;

    async fn keys(&self) -> Result<Vec<String>, DataStoreError>;

    async fn clear(&self) -> Result<(), DataStoreError>;
}
