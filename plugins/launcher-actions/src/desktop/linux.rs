//! `com.canonical.Unity.LauncherEntry` on the session bus.
//!
//! The app owns an object at [`OBJECT_PATH`] and emits `Update(app_uri,
//! properties)` with the properties that changed; docks that start after
//! the app ask the object with `Query()` for the whole state. The
//! connection is opened on the first call and kept, so an update is one
//! signal on an open socket.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use istmo_core::sync::lock;
use zbus::zvariant::Value;

use super::Config;
use crate::{
    Attention, Badge, BadgeSupport, Capabilities, LauncherAction, LauncherError, Progress,
};

/// Object path of the app's launcher entry.
pub(crate) const OBJECT_PATH: &str = "/dev/istmo/LauncherEntry";
/// Interface docks listen to.
pub(crate) const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";

/// What the dock shows, as `LauncherEntry` properties.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct EntryState {
    count: i64,
    count_visible: bool,
    progress: f64,
    progress_visible: bool,
    urgent: bool,
}

/// One `LauncherEntry` property.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Property {
    Count(i64),
    CountVisible(bool),
    Progress(f64),
    ProgressVisible(bool),
    Urgent(bool),
}

impl Property {
    const fn key(self) -> &'static str {
        match self {
            Self::Count(_) => "count",
            Self::CountVisible(_) => "count-visible",
            Self::Progress(_) => "progress",
            Self::ProgressVisible(_) => "progress-visible",
            Self::Urgent(_) => "urgent",
        }
    }

    fn value(self) -> Value<'static> {
        match self {
            Self::Count(n) => Value::from(n),
            Self::Progress(p) => Value::from(p),
            Self::CountVisible(b) | Self::ProgressVisible(b) | Self::Urgent(b) => Value::from(b),
        }
    }
}

impl EntryState {
    const fn properties(self) -> [Property; 5] {
        [
            Property::Count(self.count),
            Property::CountVisible(self.count_visible),
            Property::Progress(self.progress),
            Property::ProgressVisible(self.progress_visible),
            Property::Urgent(self.urgent),
        ]
    }

    /// Properties that differ from `before`.
    fn changed_since(self, before: Self) -> Vec<Property> {
        self.properties()
            .into_iter()
            .zip(before.properties())
            .filter(|(now, then)| now != then)
            .map(|(now, _)| now)
            .collect()
    }
}

/// The exported object.
struct Entry {
    app_uri: Arc<str>,
    state: Arc<Mutex<EntryState>>,
}

#[zbus::interface(name = "com.canonical.Unity.LauncherEntry")]
impl Entry {
    /// The whole state, for a dock that started after the app.
    fn query(&self) -> (String, HashMap<String, Value<'static>>) {
        let state = *lock(&self.state);
        (
            self.app_uri.to_string(),
            state
                .properties()
                .into_iter()
                .map(|p| (p.key().to_owned(), p.value()))
                .collect(),
        )
    }
}

/// Linux side of [`super::DesktopLauncherActions`].
pub(crate) struct Platform {
    app_uri: Arc<str>,
    state: Arc<Mutex<EntryState>>,
    connection: Mutex<Option<zbus::Connection>>,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("app_uri", &self.app_uri)
            .finish_non_exhaustive()
    }
}

impl Platform {
    pub(crate) fn new(config: &Config) -> Self {
        let id = desktop_id(
            std::env::var("FLATPAK_ID").ok().as_deref(),
            config.desktop_id.as_deref(),
        );
        Self {
            app_uri: app_uri(&id).into(),
            state: Arc::default(),
            connection: Mutex::new(None),
        }
    }

    pub(crate) const fn capabilities() -> Capabilities {
        Capabilities {
            dynamic_actions: false,
            max_actions: 0,
            pin: false,
            badge: BadgeSupport::Count,
            progress: true,
            // Whether a dock listens is unknowable; a dock that does not
            // just ignores the signal.
            attention: true,
            recent_documents: false,
        }
    }

    #[expect(clippy::unused_async, reason = "same shape as the other platforms")]
    pub(crate) async fn set_actions(&self, _: Vec<LauncherAction>) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    pub(crate) async fn set_badge(&self, badge: Badge) -> Result<(), LauncherError> {
        let (count, visible) = match badge {
            Badge::Clear => (0, false),
            Badge::Count(n) => (i64::from(n), n > 0),
            Badge::Label(_) => return Err(LauncherError::Unsupported),
        };
        self.update(|s| {
            s.count = count;
            s.count_visible = visible;
        })
        .await
    }

    pub(crate) async fn set_progress(&self, progress: Progress) -> Result<(), LauncherError> {
        // LauncherEntry has a fraction and a visibility, nothing else:
        // paused and failed bars show as plain bars.
        if progress == Progress::Indeterminate {
            return Err(LauncherError::Unsupported);
        }
        let fraction = progress.fraction();
        self.update(|s| {
            s.progress_visible = fraction.is_some();
            s.progress = fraction.map_or(0.0, f64::from);
        })
        .await
    }

    pub(crate) async fn request_attention(
        &self,
        attention: Attention,
    ) -> Result<(), LauncherError> {
        let urgent = !matches!(attention, Attention::Cancel);
        self.update(|s| s.urgent = urgent).await
    }

    #[expect(clippy::unused_async, reason = "same shape as the other platforms")]
    pub(crate) async fn add_recent_document(&self, _: String) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    #[expect(clippy::unused_async, reason = "same shape as the other platforms")]
    pub(crate) async fn clear_recent_documents(&self) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    /// Apply `change` and signal what it changed.
    async fn update(&self, change: impl FnOnce(&mut EntryState)) -> Result<(), LauncherError> {
        let changed = {
            let mut state = lock(&self.state);
            let before = *state;
            change(&mut state);
            state.changed_since(before)
        };
        // The connection serves `Query` too: open it even when nothing
        // changed, so a dock asking later sees the app.
        let connection = self.connection().await?;
        if changed.is_empty() {
            return Ok(());
        }
        let properties: HashMap<&str, Value<'static>> =
            changed.into_iter().map(|p| (p.key(), p.value())).collect();
        connection
            .emit_signal(
                None::<zbus::names::BusName<'_>>,
                OBJECT_PATH,
                INTERFACE,
                "Update",
                &(&*self.app_uri, properties),
            )
            .await
            .map_err(|e| LauncherError::Backend(format!("LauncherEntry update: {e}")))
    }

    async fn connection(&self) -> Result<zbus::Connection, LauncherError> {
        let kept = lock(&self.connection).clone();
        if let Some(connection) = kept {
            return Ok(connection);
        }
        let fresh = zbus::connection::Builder::session()
            .and_then(|builder| {
                builder.serve_at(
                    OBJECT_PATH,
                    Entry {
                        app_uri: Arc::clone(&self.app_uri),
                        state: Arc::clone(&self.state),
                    },
                )
            })
            .map_err(|e| LauncherError::Backend(format!("session bus: {e}")))?
            .build()
            .await
            .map_err(|e| LauncherError::Backend(format!("session bus: {e}")))?;
        // Two first calls racing both connect; the first stored wins.
        Ok(lock(&self.connection).get_or_insert(fresh).clone())
    }
}

/// `FLATPAK_ID`, else the configured id, else the executable's name.
fn desktop_id(flatpak: Option<&str>, configured: Option<&str>) -> String {
    flatpak
        .filter(|id| !id.is_empty())
        .or_else(|| configured.filter(|id| !id.is_empty()))
        .map(str::to_owned)
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .file_stem()?
                .to_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "istmo-app".to_owned())
}

/// `application://<id>.desktop`.
fn app_uri(desktop_id: &str) -> String {
    let id = desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id);
    format!("application://{id}.desktop")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_id_prefers_flatpak_then_configured() {
        assert_eq!(desktop_id(Some("org.a.B"), Some("x")), "org.a.B");
        assert_eq!(desktop_id(Some(""), Some("com.x.Y")), "com.x.Y");
        assert!(!desktop_id(None, None).is_empty());
    }

    #[test]
    fn app_uri_appends_desktop_once() {
        assert_eq!(app_uri("com.x.Y"), "application://com.x.Y.desktop");
        assert_eq!(app_uri("com.x.Y.desktop"), "application://com.x.Y.desktop");
    }

    #[test]
    fn only_changed_properties_are_signalled() {
        let before = EntryState::default();
        let after = EntryState {
            count: 3,
            count_visible: true,
            ..before
        };
        assert_eq!(
            after.changed_since(before),
            [Property::Count(3), Property::CountVisible(true)]
        );
        assert!(after.changed_since(after).is_empty());
    }
}
