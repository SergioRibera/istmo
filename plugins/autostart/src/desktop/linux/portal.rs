//! Flatpak: `org.freedesktop.portal.Background.RequestBackground`.
//!
//! The portal writes the host's autostart entry (`flatpak run …`) and
//! answers whether autostart is allowed. It has no call to read that
//! back, so the last answer is cached next to the app's data.

use std::path::{Path, PathBuf};

use ashpd::desktop::ResponseError;
use ashpd::desktop::background::Background;

use crate::desktop::{LaunchCommand, io_error, write_atomic};
use crate::{AutostartError, AutostartState};

const ENABLED: &[u8] = b"enabled\n";
const DISABLED: &[u8] = b"disabled\n";

/// `<data home>/istmo-autostart/<app id>.state` (in Flatpak the data
/// home is the app's own `~/.var/app/<id>/data`).
pub(crate) fn cache_path(data_home: &Path, app_id: &str) -> PathBuf {
    let mut path = data_home.join("istmo-autostart");
    path.push(format!("{app_id}.state"));
    path
}

pub(crate) fn cached_state(cache: &Path) -> AutostartState {
    match std::fs::read(cache) {
        Ok(bytes) if bytes == ENABLED => AutostartState::Enabled,
        _ => AutostartState::Disabled,
    }
}

/// Ask for autostart with `command`, or for none with `None`.
pub(crate) fn request(
    cache: &Path,
    command: Option<&LaunchCommand>,
) -> Result<AutostartState, AutostartError> {
    let argv: Vec<String> = match command {
        Some(command) => std::iter::once(command.program_str()?.to_owned())
            .chain(command.args.iter().cloned())
            .collect(),
        None => Vec::new(),
    };
    let want = command.is_some();
    let answer = pollster::block_on(async {
        Background::request()
            .reason("Start when you log in")
            .auto_start(want)
            .dbus_activatable(false)
            .command(argv)
            .send()
            .await?
            .response()
    });
    let granted = match answer {
        Ok(answer) => answer.auto_start(),
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => false,
        Err(err) => return Err(AutostartError::Backend(format!("background portal: {err}"))),
    };
    write_atomic(cache, if granted { ENABLED } else { DISABLED })
        .map_err(|e| io_error("write", cache, &e))?;
    match (want, granted) {
        (true, true) => Ok(AutostartState::Enabled),
        (true, false) => Err(AutostartError::Denied),
        (false, _) => Ok(AutostartState::Disabled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_reads_back_only_an_exact_enabled() {
        let dir =
            std::env::temp_dir().join(format!("istmo-autostart-portal-{}", std::process::id()));
        let cache = cache_path(&dir, "com.example.notes");
        assert_eq!(cached_state(&cache), AutostartState::Disabled);
        write_atomic(&cache, ENABLED).unwrap();
        assert_eq!(cached_state(&cache), AutostartState::Enabled);
        write_atomic(&cache, b"garbage").unwrap();
        assert_eq!(cached_state(&cache), AutostartState::Disabled);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
