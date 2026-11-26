//! Desktop host and launch-reason publisher.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;

use istmo_core::{IstmoError, Runtime};

use crate::{
    AUTOSTART_PLUGIN_ID, Autostart, AutostartConfig, AutostartError, AutostartOptions,
    AutostartState, Detection, LaunchReason,
};

mod exe;

// Formats of other platforms are compiled everywhere so their unit
// tests run on any host.
#[cfg_attr(
    not(target_os = "windows"),
    allow(dead_code, reason = "Windows command lines; tested on every host")
)]
mod cmdline;
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "LaunchAgent plists; tested on every host")
)]
mod plist;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod other;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use other as platform;

pub(crate) use exe::ExeEnv;

/// The command an autostart entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchCommand {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
}

impl LaunchCommand {
    /// `program args… marker`; the marker is not repeated when `args`
    /// already carries it.
    pub(crate) fn new(program: PathBuf, args: &[String], marker: Option<&str>) -> Self {
        let mut args = args.to_vec();
        ensure_marker(&mut args, marker);
        Self { program, args }
    }

    /// The program as UTF-8, which the portal command line needs.
    #[cfg(all(target_os = "linux", feature = "linux-portal"))]
    pub(crate) fn program_str(&self) -> Result<&str, AutostartError> {
        self.program.to_str().ok_or_else(|| {
            AutostartError::Backend(format!(
                "executable path {} is not UTF-8",
                self.program.display()
            ))
        })
    }
}

/// Append `marker` to `args` unless it is already there.
pub(crate) fn ensure_marker(args: &mut Vec<String>, marker: Option<&str>) {
    if let Some(marker) = marker
        && !args.iter().any(|arg| arg == marker)
    {
        args.push(marker.to_owned());
    }
}

/// Whether the OS can tell a login start apart on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// The OS says this was a login start.
    Login,
    /// The OS would have said so, and did not.
    Absent,
    /// The OS says nothing either way here.
    Unavailable,
}

/// The reason to publish: the OS first, then the marker. No heuristics.
pub(crate) const fn decide(
    signal: Signal,
    marker_configured: bool,
    marker_found: bool,
) -> LaunchReason {
    match signal {
        Signal::Login => LaunchReason::Login(Detection::System),
        _ if marker_found => LaunchReason::Login(Detection::Marker),
        Signal::Absent => LaunchReason::User,
        Signal::Unavailable if marker_configured => LaunchReason::User,
        Signal::Unavailable => LaunchReason::Unknown,
    }
}

/// Command line with the autostart marker removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrippedArgs {
    /// Whether the marker was on the command line.
    pub marker_found: bool,
    /// The other arguments, in order.
    pub args: Vec<OsString>,
}

/// Publishes the [`LaunchReason`] of this process and keeps an enabled
/// autostart entry pointing at the current executable.
#[derive(Debug)]
pub struct AutostartPublisher;

impl AutostartPublisher {
    /// Remove the marker from `args` (the program name excluded:
    /// `std::env::args_os().skip(1)`). Arguments after a literal `--`
    /// are left alone.
    #[must_use]
    pub fn strip_marker(
        config: &AutostartConfig,
        args: impl IntoIterator<Item = OsString>,
    ) -> StrippedArgs {
        let args = args.into_iter();
        let Some(marker) = config.marker().map(OsStr::new) else {
            return StrippedArgs {
                marker_found: false,
                args: args.collect(),
            };
        };
        let mut marker_found = false;
        let mut options_ended = false;
        let mut kept = Vec::with_capacity(args.size_hint().0);
        for arg in args {
            if !options_ended && arg == marker {
                marker_found = true;
                continue;
            }
            options_ended |= arg == "--";
            kept.push(arg);
        }
        StrippedArgs {
            marker_found,
            args: kept,
        }
    }

    /// Detect why this process started, publish it on
    /// [`LAUNCH_CHANNEL`](crate::LAUNCH_CHANNEL), rewrite an enabled
    /// entry whose path or marker is stale (never creating one), and
    /// return `args` without the marker — pass those on to the app's own
    /// argument handling (`DocumentsPublisher::start`, a CLI parser).
    ///
    /// Call it early, on the main thread: on macOS the system answer is
    /// read when `NSApplication` finishes launching, which the publisher
    /// has to observe before the event loop starts. Failures to rewrite
    /// an entry are logged, never returned.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and
    /// `istmo.autostart` was not declared.
    pub fn install(
        runtime: &Arc<Runtime>,
        config: &AutostartConfig,
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<Vec<OsString>, IstmoError> {
        runtime.check_declared(AUTOSTART_PLUGIN_ID)?;
        let stripped = Self::strip_marker(config, args);
        let host = DesktopAutostart::new(config.clone());
        if let Err(err) = host.refresh() {
            tracing::warn!(target: "istmo.autostart", %err, "autostart entry not refreshed");
        }
        platform::publish_launch(runtime, &host.backend, config, stripped.marker_found);
        Ok(stripped.args)
    }
}

/// Desktop [`Autostart`] host.
#[derive(Debug, Clone)]
pub struct DesktopAutostart {
    config: AutostartConfig,
    executable: Option<PathBuf>,
    backend: platform::Backend,
}

impl DesktopAutostart {
    /// Host for the app `config` describes, on the backend this install
    /// calls for (see the crate docs).
    #[must_use]
    pub fn new(config: AutostartConfig) -> Self {
        let backend = platform::Backend::detect(&config);
        Self {
            config,
            executable: None,
            backend,
        }
    }

    /// Register `path` instead of the resolved executable (a launcher
    /// script, a test binary).
    #[must_use]
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Use the XDG autostart directory under `config_home` (instead of
    /// `$XDG_CONFIG_HOME`), and never the Background portal.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn with_xdg_config_home(mut self, config_home: &std::path::Path) -> Self {
        self.backend = platform::Backend::xdg(config_home, &self.config);
        self
    }

    /// The config this host was built with.
    #[must_use]
    pub const fn config(&self) -> &AutostartConfig {
        &self.config
    }

    /// The executable an entry names.
    fn program(&self) -> Result<PathBuf, AutostartError> {
        if let Some(path) = &self.executable {
            return Ok(path.clone());
        }
        let env = ExeEnv::from_process()
            .map_err(|e| AutostartError::Backend(format!("current executable: {e}")))?;
        Ok(env.resolve(|path| std::fs::canonicalize(path).ok()))
    }

    fn command(&self, options: &AutostartOptions) -> Result<LaunchCommand, AutostartError> {
        Ok(LaunchCommand::new(
            self.program()?,
            &options.args,
            self.config.marker(),
        ))
    }

    /// Rewrite an enabled entry whose program or marker is stale.
    /// Returns whether it was rewritten.
    pub(crate) fn refresh(&self) -> Result<bool, AutostartError> {
        let program = self.program()?;
        self.backend
            .refresh(&self.config, &program, self.config.marker())
    }
}

impl Autostart for DesktopAutostart {
    async fn status(&self) -> Result<AutostartState, AutostartError> {
        self.backend.status(&self.config)
    }

    async fn enable(&self, options: AutostartOptions) -> Result<AutostartState, AutostartError> {
        let command = self.command(&options)?;
        self.backend.enable(&self.config, &command)
    }

    async fn disable(&self) -> Result<AutostartState, AutostartError> {
        self.backend.disable(&self.config)
    }

    async fn open_settings(&self) -> Result<(), AutostartError> {
        self.backend.open_settings()
    }
}

/// `path`, written whole or not at all: a temporary sibling renamed over
/// it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".istmo-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        // Best effort: the rename error is the one worth reporting.
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `Ok(())` for a file that is already gone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn remove_if_exists(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        Ok(()) | Err(_) => Ok(()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn io_error(what: &str, path: &std::path::Path, err: &std::io::Error) -> AutostartError {
    AutostartError::Backend(format!("{what} {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn marker_is_stripped_before_double_dash_only() {
        let config = AutostartConfig::new("com.example.notes").with_marker("--autostart");
        let stripped = AutostartPublisher::strip_marker(
            &config,
            os(&["a.txt", "--autostart", "--", "--autostart"]),
        );
        assert!(stripped.marker_found);
        assert_eq!(stripped.args, os(&["a.txt", "--", "--autostart"]));

        let stripped = AutostartPublisher::strip_marker(&config, os(&["--autostart=1", "b"]));
        assert!(!stripped.marker_found);
        assert_eq!(stripped.args, os(&["--autostart=1", "b"]));
    }

    #[test]
    fn without_a_marker_nothing_is_stripped() {
        let config = AutostartConfig::new("com.example.notes");
        let stripped = AutostartPublisher::strip_marker(&config, os(&["--autostart"]));
        assert!(!stripped.marker_found);
        assert_eq!(stripped.args, os(&["--autostart"]));
    }

    #[test]
    fn reason_prefers_the_system_then_the_marker() {
        use LaunchReason::{Login, Unknown, User};
        assert_eq!(
            decide(Signal::Login, false, false),
            Login(Detection::System)
        );
        assert_eq!(decide(Signal::Login, true, true), Login(Detection::System));
        assert_eq!(decide(Signal::Absent, true, true), Login(Detection::Marker));
        assert_eq!(
            decide(Signal::Unavailable, true, true),
            Login(Detection::Marker)
        );
        assert_eq!(decide(Signal::Absent, false, false), User);
        assert_eq!(decide(Signal::Unavailable, true, false), User);
        assert_eq!(decide(Signal::Unavailable, false, false), Unknown);
    }

    #[test]
    fn command_appends_the_marker_once() {
        let cmd = LaunchCommand::new(
            PathBuf::from("/usr/bin/notes"),
            &["--tray".to_owned(), "--autostart".to_owned()],
            Some("--autostart"),
        );
        assert_eq!(cmd.args, ["--tray", "--autostart"]);
        let cmd = LaunchCommand::new(PathBuf::from("/usr/bin/notes"), &[], Some("--autostart"));
        assert_eq!(cmd.args, ["--autostart"]);
    }
}
