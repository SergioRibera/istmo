//! Windows: `StartupTask` for MSIX packages, the `Run` key otherwise.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use ::windows::ApplicationModel::Activation::ActivationKind;
use ::windows::ApplicationModel::{AppInstance, Package, StartupTask, StartupTaskState};
use ::windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use ::windows::core::{HRESULT, HSTRING};
use istmo_core::Runtime;
use windows_registry::{CURRENT_USER, Key};

use super::{LaunchCommand, Signal, cmdline, decide, ensure_marker};
use crate::{AutostartConfig, AutostartError, AutostartState};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Task Manager's "Startup apps" switch: 12 bytes, the first one odd
/// when the user turned the entry off.
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const SETTINGS_URI: &str = "ms-settings:startupapps";

/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
const NOT_FOUND: HRESULT = HRESULT::from_win32(2);

#[derive(Debug, Clone, Copy)]
pub(crate) enum Backend {
    /// MSIX: the package's `uap5:StartupTask`.
    Packaged,
    /// `HKCU\…\Run`.
    RunKey,
}

impl Backend {
    pub(crate) fn detect(_config: &AutostartConfig) -> Self {
        init_winrt();
        if Package::Current().is_ok() {
            Self::Packaged
        } else {
            Self::RunKey
        }
    }

    pub(crate) fn status(self, config: &AutostartConfig) -> Result<AutostartState, AutostartError> {
        match self {
            Self::Packaged => Ok(map_task_state(
                startup_task(config)?.State().map_err(winrt)?,
            )),
            Self::RunKey => run_key_state(config),
        }
    }

    pub(crate) fn enable(
        self,
        config: &AutostartConfig,
        command: &LaunchCommand,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::Packaged => {
                let task = startup_task(config)?;
                let state = map_task_state(task.State().map_err(winrt)?);
                if state != AutostartState::Disabled {
                    if state == AutostartState::DisabledByUser {
                        self.open_settings()?;
                    }
                    return Ok(state);
                }
                let state = task
                    .RequestEnableAsync()
                    .and_then(|op| op.join())
                    .map_err(winrt)?;
                match map_task_state(state) {
                    AutostartState::Disabled | AutostartState::DisabledByUser => {
                        Err(AutostartError::Denied)
                    }
                    other => Ok(other),
                }
            }
            Self::RunKey => {
                let line = cmdline::render(command).ok_or_else(|| non_utf8(&command.program))?;
                run_key(true)?
                    .set_string(config.app_id(), &line)
                    .map_err(registry)?;
                // Never touch the approval value: only the user flips it.
                let state = run_key_state(config)?;
                if state == AutostartState::DisabledByUser {
                    self.open_settings()?;
                }
                Ok(state)
            }
        }
    }

    pub(crate) fn disable(
        self,
        config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::Packaged => {
                let task = startup_task(config)?;
                task.Disable().map_err(winrt)?;
                Ok(map_task_state(task.State().map_err(winrt)?))
            }
            Self::RunKey => {
                match run_key(false)?.remove_value(config.app_id()) {
                    Err(err) if err.code() != NOT_FOUND => return Err(registry(err)),
                    Ok(()) | Err(_) => {}
                }
                Ok(AutostartState::Disabled)
            }
        }
    }

    #[expect(clippy::unused_self, reason = "same surface as the other backends")]
    pub(crate) fn open_settings(self) -> Result<(), AutostartError> {
        // `explorer <uri>` hands the URI to the shell, like a Run dialog.
        Command::new("explorer.exe")
            .arg(SETTINGS_URI)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| AutostartError::Backend(format!("open {SETTINGS_URI}: {e}")))
    }

    /// Point an existing `Run` value at `program` and make sure it
    /// carries `marker`. Packages start through their `StartupTask`, whose
    /// target the package manifest names.
    pub(crate) fn refresh(
        self,
        config: &AutostartConfig,
        program: &Path,
        marker: Option<&str>,
    ) -> Result<bool, AutostartError> {
        let Self::RunKey = self else {
            return Ok(false);
        };
        let Some(line) = run_value(config)? else {
            return Ok(false);
        };
        let (old_program, rest) = cmdline::split_program(&line);
        let mut args = cmdline::parse_args(rest);
        let before = args.len();
        ensure_marker(&mut args, marker);
        if Path::new(old_program) == program && args.len() == before {
            return Ok(false);
        }
        let command = LaunchCommand {
            program: program.to_path_buf(),
            args,
        };
        let line = cmdline::render(&command).ok_or_else(|| non_utf8(program))?;
        run_key(true)?
            .set_string(config.app_id(), &line)
            .map_err(registry)?;
        Ok(true)
    }
}

/// Packages: the activation kind of this launch. Unpackaged `Run`
/// starts carry nothing but the marker.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "same signature on every platform"
)]
pub(crate) fn publish_launch(
    runtime: &Arc<Runtime>,
    backend: &Backend,
    config: &AutostartConfig,
    marker_found: bool,
) {
    let signal = match backend {
        Backend::Packaged => {
            let kind = AppInstance::GetActivatedEventArgs().and_then(|args| args.Kind());
            match kind {
                Ok(ActivationKind::StartupTask) => Signal::Login,
                Ok(_) => Signal::Absent,
                Err(err) => {
                    tracing::debug!(target: "istmo.autostart", %err, "no activation arguments");
                    Signal::Unavailable
                }
            }
        }
        Backend::RunKey => Signal::Unavailable,
    };
    let reason = decide(signal, config.marker().is_some(), marker_found);
    crate::launch::publish(runtime, reason);
}

/// `WinRT` calls need an apartment on the calling thread.
#[expect(unsafe_code, reason = "RoInitialize is an unsafe extern function")]
fn init_winrt() {
    // SAFETY: RoInitialize takes no pointers and may be called on any
    // thread any number of times; RPC_E_CHANGED_MODE (the thread is
    // already an STA) leaves the existing apartment, which works too.
    drop(unsafe { RoInitialize(RO_INIT_MULTITHREADED) });
}

fn startup_task(config: &AutostartConfig) -> Result<StartupTask, AutostartError> {
    init_winrt();
    StartupTask::GetAsync(&HSTRING::from(config.windows_task_id()))
        .and_then(|op| op.join())
        .map_err(|err| {
            AutostartError::Backend(format!(
                "StartupTask `{}` (declared as uap5:StartupTask in the package manifest?): {err}",
                config.windows_task_id()
            ))
        })
}

const fn map_task_state(state: StartupTaskState) -> AutostartState {
    match state {
        StartupTaskState::Enabled => AutostartState::Enabled,
        StartupTaskState::DisabledByUser => AutostartState::DisabledByUser,
        StartupTaskState::DisabledByPolicy => AutostartState::DisabledByPolicy,
        StartupTaskState::EnabledByPolicy => AutostartState::EnabledByPolicy,
        // `Disabled`, and values newer than this crate.
        _ => AutostartState::Disabled,
    }
}

fn run_key(create: bool) -> Result<Key, AutostartError> {
    if create {
        CURRENT_USER.create(RUN)
    } else {
        CURRENT_USER.open(RUN)
    }
    .map_err(registry)
}

fn run_value(config: &AutostartConfig) -> Result<Option<String>, AutostartError> {
    let key = match CURRENT_USER.open(RUN) {
        Ok(key) => key,
        Err(err) if err.code() == NOT_FOUND => return Ok(None),
        Err(err) => return Err(registry(err)),
    };
    match key.get_string(config.app_id()) {
        Ok(line) => Ok(Some(line)),
        Err(err) if err.code() == NOT_FOUND => Ok(None),
        Err(err) => Err(registry(err)),
    }
}

fn run_key_state(config: &AutostartConfig) -> Result<AutostartState, AutostartError> {
    if run_value(config)?.is_none() {
        return Ok(AutostartState::Disabled);
    }
    let approved = CURRENT_USER
        .open(APPROVED)
        .and_then(|key| key.get_value(config.app_id()));
    Ok(match approved {
        Ok(value) if value.first().is_some_and(|flags| flags & 1 == 1) => {
            AutostartState::DisabledByUser
        }
        _ => AutostartState::Enabled,
    })
}

#[expect(clippy::needless_pass_by_value, reason = "a `map_err` callback")]
fn registry(err: ::windows::core::Error) -> AutostartError {
    AutostartError::Backend(format!("registry: {err}"))
}

#[expect(clippy::needless_pass_by_value, reason = "a `map_err` callback")]
fn winrt(err: ::windows::core::Error) -> AutostartError {
    AutostartError::Backend(format!("StartupTask: {err}"))
}

fn non_utf8(path: &Path) -> AutostartError {
    AutostartError::Backend(format!("executable path {} is not UTF-8", path.display()))
}
