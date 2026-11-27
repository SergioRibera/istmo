//! Linux: an XDG autostart entry, or the Background portal in Flatpak;
//! login starts recognised by their systemd unit.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use istmo_core::Runtime;

use super::{
    LaunchCommand, Signal, decide, ensure_marker, io_error, remove_if_exists, write_atomic,
};
use crate::{AutostartConfig, AutostartError, AutostartState};

mod cgroup;
mod entry;
#[cfg(feature = "linux-portal")]
mod portal;

use entry::Entry;

/// How this install registers.
#[derive(Debug, Clone)]
pub(crate) enum Backend {
    /// `<config home>/autostart/<app id>.desktop`.
    Xdg { entry: PathBuf },
    /// Flatpak: `org.freedesktop.portal.Background`, last answer cached
    /// in `cache` (the portal has no read-back).
    #[cfg(feature = "linux-portal")]
    Portal { cache: PathBuf },
    /// Flatpak without the `linux-portal` feature, or no home directory.
    Unsupported,
}

impl Backend {
    pub(crate) fn detect(config: &AutostartConfig) -> Self {
        if is_flatpak() {
            #[cfg(feature = "linux-portal")]
            if let Some(data) = base_dir("XDG_DATA_HOME", ".local/share") {
                return Self::Portal {
                    cache: portal::cache_path(&data, config.app_id()),
                };
            }
            return Self::Unsupported;
        }
        base_dir("XDG_CONFIG_HOME", ".config")
            .map_or(Self::Unsupported, |home| Self::xdg(&home, config))
    }

    pub(crate) fn xdg(config_home: &Path, config: &AutostartConfig) -> Self {
        let mut entry = config_home.join("autostart");
        entry.push(format!("{}.desktop", config.app_id()));
        Self::Xdg { entry }
    }

    pub(crate) fn status(
        &self,
        _config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::Xdg { entry } => Ok(match read_entry(entry)? {
                None => AutostartState::Disabled,
                Some(found) if found.disabled_by_user() => AutostartState::DisabledByUser,
                Some(_) => AutostartState::Enabled,
            }),
            #[cfg(feature = "linux-portal")]
            Self::Portal { cache } => Ok(portal::cached_state(cache)),
            Self::Unsupported => Ok(AutostartState::Unsupported),
        }
    }

    /// Linux has no settings page to send the user to, so an entry the
    /// user turned off is rewritten: `enable` only runs from a user
    /// action, which is the consent.
    pub(crate) fn enable(
        &self,
        config: &AutostartConfig,
        command: &LaunchCommand,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::Xdg { entry } => {
                let text = entry::render(config.display_name(), command)
                    .ok_or_else(|| non_utf8(&command.program))?;
                write_atomic(entry, text.as_bytes()).map_err(|e| io_error("write", entry, &e))?;
                Ok(AutostartState::Enabled)
            }
            #[cfg(feature = "linux-portal")]
            Self::Portal { cache } => portal::request(cache, Some(command)),
            Self::Unsupported => Err(AutostartError::Unsupported),
        }
    }

    pub(crate) fn disable(
        &self,
        _config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::Xdg { entry } => {
                remove_if_exists(entry).map_err(|e| io_error("remove", entry, &e))?;
                Ok(AutostartState::Disabled)
            }
            #[cfg(feature = "linux-portal")]
            Self::Portal { cache } => portal::request(cache, None),
            Self::Unsupported => Err(AutostartError::Unsupported),
        }
    }

    // Keeps the signature every platform shares.
    #[expect(clippy::unused_self, reason = "no settings page on any Linux backend")]
    pub(crate) const fn open_settings(&self) -> Result<(), AutostartError> {
        Err(AutostartError::Unsupported)
    }

    /// Point an enabled XDG entry at `program` and make sure it carries
    /// `marker`. The portal entry runs `flatpak run`, whose path does not
    /// change.
    pub(crate) fn refresh(
        &self,
        _config: &AutostartConfig,
        program: &Path,
        marker: Option<&str>,
    ) -> Result<bool, AutostartError> {
        let Self::Xdg { entry: path } = self else {
            return Ok(false);
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(io_error("read", path, &err)),
        };
        let found = Entry::parse(&text);
        if found.disabled_by_user() {
            return Ok(false);
        }
        let mut exec = found.exec.unwrap_or_default().into_iter();
        let old_program = exec.next();
        let mut args: Vec<String> = exec.collect();
        let before = args.len();
        ensure_marker(&mut args, marker);
        if old_program.as_deref().map(Path::new) == Some(program) && args.len() == before {
            return Ok(false);
        }
        let command = LaunchCommand {
            program: program.to_path_buf(),
            args,
        };
        let text = entry::replace_exec(&text, &command).ok_or_else(|| non_utf8(program))?;
        write_atomic(path, text.as_bytes()).map_err(|e| io_error("write", path, &e))?;
        Ok(true)
    }
}

/// Publish the launch reason: systemd's unit name first, the marker
/// where systemd does not start the session's apps.
pub(crate) fn publish_launch(
    runtime: &Arc<Runtime>,
    _backend: &Backend,
    config: &AutostartConfig,
    marker_found: bool,
) {
    let signal = if is_flatpak() {
        // The portal's entry runs `flatpak run`, which moves the app to a
        // scope of its own: the unit name never reaches the sandbox.
        Signal::Unavailable
    } else {
        std::fs::read_to_string("/proc/self/cgroup")
            .map_or(Signal::Unavailable, |text| cgroup::signal(&text))
    };
    let reason = decide(signal, config.marker().is_some(), marker_found);
    tracing::debug!(target: "istmo.autostart", ?reason, "launch reason");
    crate::launch::publish(runtime, reason);
}

fn read_entry(path: &Path) -> Result<Option<Entry>, AutostartError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(Entry::parse(&text))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_error("read", path, &err)),
    }
}

fn non_utf8(path: &Path) -> AutostartError {
    AutostartError::Backend(format!("executable path {} is not UTF-8", path.display()))
}

fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// `$<var>` when absolute, else `$HOME/<fallback>`.
fn base_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(fallback))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_rewrites_only_a_stale_enabled_entry() {
        let home =
            std::env::temp_dir().join(format!("istmo-autostart-refresh-{}", std::process::id()));
        let config = AutostartConfig::new("com.example.notes").with_marker("--autostart");
        let backend = Backend::xdg(&home, &config);
        let Backend::Xdg { entry } = &backend else {
            unreachable!("xdg() builds an XDG backend");
        };
        let old = LaunchCommand::new(
            PathBuf::from("/nix/store/old/bin/notes"),
            &["--tray".to_owned()],
            None,
        );

        // Nothing to refresh without an entry: it is never created.
        assert_eq!(
            backend.refresh(&config, Path::new("/new/notes"), config.marker()),
            Ok(false)
        );
        assert!(!entry.exists());

        backend.enable(&config, &old).unwrap();
        let program = Path::new("/home/ana/.nix-profile/bin/notes");
        assert_eq!(backend.refresh(&config, program, config.marker()), Ok(true));
        let found = Entry::parse(&std::fs::read_to_string(entry).unwrap());
        assert_eq!(
            found.exec.unwrap(),
            ["/home/ana/.nix-profile/bin/notes", "--tray", "--autostart"]
        );
        assert_eq!(
            backend.refresh(&config, program, config.marker()),
            Ok(false)
        );

        // A user-disabled entry is left alone.
        let text = std::fs::read_to_string(entry).unwrap();
        std::fs::write(entry, format!("{text}Hidden=true\n")).unwrap();
        assert_eq!(
            backend.refresh(&config, Path::new("/other"), None),
            Ok(false)
        );
        std::fs::remove_dir_all(&home).unwrap();
    }
}
