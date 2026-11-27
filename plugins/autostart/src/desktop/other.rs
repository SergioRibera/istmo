//! Desktop targets without a backend (the BSDs, …): everything is
//! unsupported, the launch reason comes from the marker alone.

use std::path::Path;
use std::sync::Arc;

use istmo_core::Runtime;

use super::{LaunchCommand, Signal, decide};
use crate::{AutostartConfig, AutostartError, AutostartState};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Backend;

#[expect(clippy::unused_self, reason = "same surface as the real backends")]
impl Backend {
    pub(crate) const fn detect(_config: &AutostartConfig) -> Self {
        Self
    }

    pub(crate) const fn status(
        &self,
        _config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        Ok(AutostartState::Unsupported)
    }

    pub(crate) const fn enable(
        &self,
        _config: &AutostartConfig,
        _command: &LaunchCommand,
    ) -> Result<AutostartState, AutostartError> {
        Err(AutostartError::Unsupported)
    }

    pub(crate) const fn disable(
        &self,
        _config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        Err(AutostartError::Unsupported)
    }

    pub(crate) const fn open_settings(&self) -> Result<(), AutostartError> {
        Err(AutostartError::Unsupported)
    }

    pub(crate) const fn refresh(
        &self,
        _config: &AutostartConfig,
        _program: &Path,
        _marker: Option<&str>,
    ) -> Result<bool, AutostartError> {
        Ok(false)
    }
}

pub(crate) fn publish_launch(
    runtime: &Arc<Runtime>,
    _backend: &Backend,
    config: &AutostartConfig,
    marker_found: bool,
) {
    let reason = decide(Signal::Unavailable, config.marker().is_some(), marker_found);
    crate::launch::publish(runtime, reason);
}
