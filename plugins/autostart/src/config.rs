//! [`AutostartConfig`]: the app's identity and `[app.autostart]` keys.

use crate::DEFAULT_WINDOWS_TASK_ID;

/// Longest marker, task id or app id kept; longer ones are dropped with
/// a warning (they cannot come from a valid `istmo.toml`).
const MAX_LEN: usize = 255;

/// What every backend needs to know about the app.
///
/// Built from the app crate's build env with
/// [`autostart_config!`](crate::autostart_config) or
/// [`Self::from_build`]; [`Self::new`] for apps that do not use
/// `istmo-build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartConfig {
    app_id: String,
    name: Option<String>,
    marker: Option<String>,
    windows_task_id: String,
}

impl AutostartConfig {
    /// Config for the app `app_id` (reverse DNS, e.g. `com.example.notes`),
    /// with no marker and the default MSIX task id.
    ///
    /// `app_id` names the `.desktop` file, the `Run` value and the
    /// `LaunchAgent` label. Characters outside `[A-Za-z0-9._-]` are
    /// replaced with `_`, so it is always a safe file name.
    #[must_use]
    pub fn new(app_id: &str) -> Self {
        Self {
            app_id: sanitize_id(app_id),
            name: None,
            marker: None,
            windows_task_id: DEFAULT_WINDOWS_TASK_ID.to_owned(),
        }
    }

    /// Config from the build env of the app crate (`option_env!` values
    /// of `ISTMO_APP_BUNDLE_ID`, `ISTMO_AUTOSTART_MARKER`,
    /// `ISTMO_AUTOSTART_TASK_ID`). A missing bundle id falls back to the
    /// executable's name; an invalid marker or task id is dropped with a
    /// warning.
    #[must_use]
    pub fn from_build(
        bundle_id: Option<&str>,
        marker: Option<&str>,
        task_id: Option<&str>,
    ) -> Self {
        let app_id = bundle_id
            .filter(|id| !id.is_empty())
            .map_or_else(executable_name, str::to_owned);
        let mut config = Self::new(&app_id);
        if let Some(marker) = marker {
            config = config.with_marker(marker);
        }
        if let Some(task_id) = task_id {
            config = config.with_windows_task_id(task_id);
        }
        config
    }

    /// Set the marker argument (see `[app.autostart] marker`). Ignored,
    /// with a warning, unless it starts with `-`, has a character after
    /// the dashes, and is printable ASCII without quotes, `\`, `` ` ``,
    /// `$` or `%`.
    #[must_use]
    pub fn with_marker(mut self, marker: &str) -> Self {
        if is_valid_marker(marker) {
            self.marker = Some(marker.to_owned());
        } else {
            tracing::warn!(target: "istmo.autostart", marker, "invalid autostart marker ignored");
        }
        self
    }

    /// Set the MSIX `StartupTask` id. Ignored, with a warning, unless it
    /// is 1 to 255 ASCII letters, digits, `.`, `_` or `-`.
    #[must_use]
    pub fn with_windows_task_id(mut self, task_id: &str) -> Self {
        if is_valid_task_id(task_id) {
            task_id.clone_into(&mut self.windows_task_id);
        } else {
            tracing::warn!(target: "istmo.autostart", task_id, "invalid StartupTask id ignored");
        }
        self
    }

    /// Display name written to the Linux `.desktop` entry (`Name=`).
    /// Defaults to the app id.
    #[must_use]
    pub fn with_display_name(mut self, name: &str) -> Self {
        self.name = Some(name.replace(['\n', '\r'], " "));
        self
    }

    /// The app id, safe as a file name.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The display name, or the app id.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.app_id)
    }

    /// The marker argument, when configured.
    #[must_use]
    pub fn marker(&self) -> Option<&str> {
        self.marker.as_deref()
    }

    /// The MSIX `StartupTask` id.
    #[must_use]
    pub fn windows_task_id(&self) -> &str {
        &self.windows_task_id
    }
}

fn sanitize_id(raw: &str) -> String {
    let id: String = raw
        .chars()
        .take(MAX_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // `.` / `..` / a leading dot would be a hidden or special path.
    if id.is_empty() || id.starts_with('.') {
        format!("app{id}")
    } else {
        id
    }
}

fn executable_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
        .unwrap_or_else(|| "app".to_owned())
}

/// Same rule as `istmo-build`: an option the registered command line
/// (desktop `Exec=`, Windows command line, plist string) carries
/// verbatim, without any quoting.
pub(crate) fn is_valid_marker(marker: &str) -> bool {
    marker.len() <= MAX_LEN
        && marker.starts_with('-')
        && !marker.trim_start_matches('-').is_empty()
        && marker
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '\\' | '`' | '$' | '%'))
}

fn is_valid_task_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_build_reads_every_value() {
        let config = AutostartConfig::from_build(
            Some("com.example.notes"),
            Some("--autostart"),
            Some("NotesStartup"),
        );
        assert_eq!(config.app_id(), "com.example.notes");
        assert_eq!(config.marker(), Some("--autostart"));
        assert_eq!(config.windows_task_id(), "NotesStartup");
        assert_eq!(config.display_name(), "com.example.notes");
    }

    #[test]
    fn invalid_values_are_dropped() {
        let config =
            AutostartConfig::from_build(Some("com.example.notes"), Some("hidden"), Some("a b"));
        assert_eq!(config.marker(), None);
        assert_eq!(config.windows_task_id(), DEFAULT_WINDOWS_TASK_ID);
        for marker in ["-", "--", "--a b", "--\"x", "--%u", "--$HOME", "--ü"] {
            assert!(!is_valid_marker(marker), "{marker:?}");
        }
    }

    #[test]
    fn app_id_is_a_safe_file_name() {
        assert_eq!(
            AutostartConfig::new("../etc/passwd").app_id(),
            "app.._etc_passwd"
        );
        assert_eq!(AutostartConfig::new("a b/c").app_id(), "a_b_c");
        assert_eq!(AutostartConfig::new("").app_id(), "app");
    }

    #[test]
    fn missing_bundle_id_falls_back_to_the_executable() {
        let config = AutostartConfig::from_build(None, None, None);
        assert_ne!(config.app_id(), "");
        assert_eq!(config.marker(), None);
    }
}
