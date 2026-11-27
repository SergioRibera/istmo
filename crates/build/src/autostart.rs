//! `[app.autostart]` — how the `istmo-autostart` plugin registers and
//! recognises a start at login.
//!
//! ```toml
//! [app.autostart]
//! marker = "--hidden"           # optional; no key = no marker
//! windows_task_id = "MyStartup" # optional, MSIX only
//! ```
//!
//! Nothing here turns autostart on: an app enables it from a user action
//! at run time. The keys only travel to the app crate as
//! `ISTMO_AUTOSTART_MARKER` / `ISTMO_AUTOSTART_TASK_ID` rustc envs,
//! which the app hands to `AutostartConfig::from_build`.

use toml_edit::Item;

use crate::manifest::{ManifestError, expect_string, expect_table};

const KNOWN_KEYS: &[&str] = &["marker", "windows_task_id"];

/// Longest marker or task id accepted. Both end up on a command line or
/// in a package manifest; neither needs more.
const MAX_LEN: usize = 64;

/// Parsed `[app.autostart]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutostartConfig {
    /// Argument appended to the registered command line, so a login
    /// start can be told apart where the OS does not say so.
    pub marker: Option<String>,
    /// `uap5:StartupTask` `TaskId` of an MSIX package.
    pub windows_task_id: Option<String>,
}

impl AutostartConfig {
    /// Parse the `app.autostart` table.
    ///
    /// # Errors
    ///
    /// An unknown key, a non-string value, a marker that does not start
    /// with `-`, is only dashes, or is not printable ASCII without
    /// quotes, `\`, `` ` ``, `$` and `%`,
    /// or a task id that is not 1 to 64 ASCII letters, digits, `.`, `_`
    /// or `-`.
    pub fn parse(item: &Item) -> Result<Self, ManifestError> {
        let table = expect_table(item, "app.autostart")?;
        for (name, _) in table.iter() {
            if !KNOWN_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.autostart.{name}"),
                });
            }
        }
        let marker = table
            .get("marker")
            .map(|item| expect_string(item, "app.autostart.marker"))
            .transpose()?
            .map(|marker| {
                if is_valid_marker(marker) {
                    Ok(marker.to_owned())
                } else {
                    Err(ManifestError::InvalidValue {
                        key: "app.autostart.marker".to_owned(),
                        value: marker.to_owned(),
                        expected: "an option such as `--autostart`: starts with `-`, printable ASCII \
                                   without quotes, `\\`, `` ` ``, `$` or `%`, at most 64 characters",
                    })
                }
            })
            .transpose()?;
        let windows_task_id = table
            .get("windows_task_id")
            .map(|item| expect_string(item, "app.autostart.windows_task_id"))
            .transpose()?
            .map(|id| {
                if is_valid_task_id(id) {
                    Ok(id.to_owned())
                } else {
                    Err(ManifestError::InvalidValue {
                        key: "app.autostart.windows_task_id".to_owned(),
                        value: id.to_owned(),
                        expected: "1 to 64 ASCII letters, digits, `.`, `_` or `-`",
                    })
                }
            })
            .transpose()?;
        Ok(Self {
            marker,
            windows_task_id,
        })
    }

    /// `cargo::rustc-env` lines for the app crate, one per set key.
    pub(crate) fn rustc_env_lines(&self) -> impl Iterator<Item = String> + '_ {
        let marker = self
            .marker
            .as_deref()
            .map(|marker| format!("cargo::rustc-env=ISTMO_AUTOSTART_MARKER={marker}"));
        let task = self
            .windows_task_id
            .as_deref()
            .map(|id| format!("cargo::rustc-env=ISTMO_AUTOSTART_TASK_ID={id}"));
        marker.into_iter().chain(task)
    }
}

/// `-x`, `--x`: a lone `-` or `--` would read as "stdin" / "end of
/// options" to the app's own argument parser. Printable ASCII without
/// quoting characters, so the marker goes verbatim into a desktop entry
/// `Exec=`, a Windows command line and a plist string.
fn is_valid_marker(marker: &str) -> bool {
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
    use crate::app_config::AppConfig;

    fn parse(src: &str) -> Result<AutostartConfig, ManifestError> {
        AppConfig::parse(src).map(|config| config.autostart)
    }

    #[test]
    fn absent_table_is_empty() {
        assert_eq!(parse("[app]\n").unwrap(), AutostartConfig::default());
        assert_eq!(
            AutostartConfig::default().rustc_env_lines().count(),
            0,
            "no key, no env"
        );
    }

    #[test]
    fn both_keys_parse_and_emit() {
        let config =
            parse("[app.autostart]\nmarker = \"--hidden\"\nwindows_task_id = \"MyStartup\"\n")
                .unwrap();
        assert_eq!(config.marker.as_deref(), Some("--hidden"));
        assert_eq!(config.windows_task_id.as_deref(), Some("MyStartup"));
        let lines: Vec<String> = config.rustc_env_lines().collect();
        assert_eq!(
            lines,
            [
                "cargo::rustc-env=ISTMO_AUTOSTART_MARKER=--hidden",
                "cargo::rustc-env=ISTMO_AUTOSTART_TASK_ID=MyStartup",
            ]
        );
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = parse("[app.autostart]\nenabled = true\n").unwrap_err();
        assert!(
            matches!(&err, ManifestError::UnknownKey { key } if key == "app.autostart.enabled"),
            "{err}"
        );
    }

    #[test]
    fn bad_markers_are_rejected() {
        for marker in [
            "",
            "hidden",
            "-",
            "--",
            "--a b",
            "--a\\tb",
            "--\\u0007",
            "--a\\nb",
            "--\\\"q",
            "--%u",
            "--$HOME",
            "--é",
        ] {
            let src = format!("[app.autostart]\nmarker = \"{marker}\"\n");
            assert!(
                matches!(parse(&src), Err(ManifestError::InvalidValue { .. })),
                "{marker:?} accepted"
            );
        }
        let long = format!("[app.autostart]\nmarker = \"--{}\"\n", "a".repeat(63));
        assert!(parse(&long).is_err());
        assert!(parse("[app.autostart]\nmarker = 1\n").is_err());
    }

    #[test]
    fn good_markers_are_accepted() {
        for marker in ["-h", "--autostart", "--started-at-login=1"] {
            let src = format!("[app.autostart]\nmarker = \"{marker}\"\n");
            assert_eq!(parse(&src).unwrap().marker.as_deref(), Some(marker));
        }
    }

    #[test]
    fn bad_task_ids_are_rejected() {
        for id in ["", "has space", "slash/no", "é"] {
            let src = format!("[app.autostart]\nwindows_task_id = \"{id}\"\n");
            assert!(
                matches!(parse(&src), Err(ManifestError::InvalidValue { .. })),
                "{id:?} accepted"
            );
        }
    }
}
