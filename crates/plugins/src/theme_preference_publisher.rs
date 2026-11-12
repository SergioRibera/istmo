//! Pure-Rust desktop sampler for
//! [`ThemePreferenceClient`](super::theme_preference::ThemePreferenceClient).
//!
//! Covers the single most-asked signal — the user's light/dark
//! preference — on Linux (`gsettings`), macOS (`defaults read`) and
//! Windows (Registry `AppsUseLightTheme`). Font scale, accessibility
//! flags and accent-colour pickers vary wildly across desktop DEs;
//! leave those to the per-app observer until a real consumer surfaces.

use std::sync::Arc;
use std::time::{Duration, Instant};

use istmo_core::{IstmoError, Runtime};

use super::theme_preference::{ColorScheme, ThemePreference};
use crate::early::LatestPublisher;

/// Shortest time between two samples.
///
/// Linux and macOS sample by running `gsettings` / `defaults` as a child
/// process, far too costly for a per-frame tick; a user toggling dark
/// mode tolerates a two-second lag.
pub const MIN_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// Publishes the host's light/dark preference.
///
/// [`tick`](Self::tick) may be called every frame: it samples at most
/// once per [`MIN_SAMPLE_INTERVAL`] and publishes only on change.
#[derive(Debug)]
pub struct ThemePreferencePublisher {
    publisher: LatestPublisher<ThemePreference>,
    sampled_at: Option<Instant>,
}

impl ThemePreferencePublisher {
    /// Publisher bound to `runtime`.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and the
    /// plugin was not declared.
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        Ok(Self {
            publisher: LatestPublisher::install(runtime)?,
            sampled_at: None,
        })
    }

    /// Sample the host unless the last sample is younger than
    /// [`MIN_SAMPLE_INTERVAL`], and publish if the preference changed.
    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(at) = self.sampled_at
            && now.duration_since(at) < MIN_SAMPLE_INTERVAL
        {
            return;
        }
        self.sampled_at = Some(now);
        self.publisher.publish_if_changed(sample());
    }
}

fn sample() -> ThemePreference {
    ThemePreference {
        color_scheme: color_scheme(),
        accent_color: None,
        high_contrast: false,
        font_scale: 1.0,
        reduce_motion: false,
        reduce_transparency: false,
        bold_text: false,
        invert_colors: false,
        screen_reader_active: false,
    }
}

fn color_scheme() -> ColorScheme {
    #[cfg(target_os = "linux")]
    {
        linux::color_scheme()
    }
    #[cfg(target_os = "macos")]
    {
        macos::color_scheme()
    }
    #[cfg(target_os = "windows")]
    {
        windows::color_scheme()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        ColorScheme::Auto
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Command;

    use super::ColorScheme;

    pub(super) fn color_scheme() -> ColorScheme {
        // GNOME 42+ exposes `color-scheme` directly ("prefer-dark" /
        // "prefer-light" / "default"); earlier versions key off the
        // `gtk-theme` name suffix.
        if let Some(value) = read_gsettings("org.gnome.desktop.interface", "color-scheme") {
            return match value.trim() {
                "'prefer-dark'" | "prefer-dark" => ColorScheme::Dark,
                "'prefer-light'" | "prefer-light" => ColorScheme::Light,
                _ => ColorScheme::Auto,
            };
        }
        if let Some(theme) = read_gsettings("org.gnome.desktop.interface", "gtk-theme") {
            let lower = theme.to_ascii_lowercase();
            if lower.contains("dark") {
                return ColorScheme::Dark;
            }
        }
        ColorScheme::Auto
    }

    fn read_gsettings(schema: &str, key: &str) -> Option<String> {
        let out = Command::new("gsettings")
            .args(["get", schema, key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::Command;

    use super::ColorScheme;

    pub(super) fn color_scheme() -> ColorScheme {
        // `defaults read -g AppleInterfaceStyle` prints "Dark" when
        // Dark Mode is enabled and errors otherwise (light / auto).
        let out = Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if s.eq_ignore_ascii_case("dark") {
                    ColorScheme::Dark
                } else {
                    ColorScheme::Light
                }
            }
            _ => ColorScheme::Light,
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use std::mem;
    use std::ptr;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW,
        RegOpenKeyExW,
    };

    use super::ColorScheme;

    pub(super) fn color_scheme() -> ColorScheme {
        const PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
        const VALUE: &str = "AppsUseLightTheme";
        let path_w: Vec<u16> = PATH.encode_utf16().chain(Some(0)).collect();
        let value_w: Vec<u16> = VALUE.encode_utf16().chain(Some(0)).collect();
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: NUL-terminated UTF-16 paths; `hkey` is out-param.
        let rc = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path_w.as_ptr(),
                0,
                KEY_READ,
                &raw mut hkey,
            )
        };
        if rc != ERROR_SUCCESS {
            return ColorScheme::Auto;
        }
        let mut dword: u32 = 0;
        let mut size = u32::try_from(mem::size_of::<u32>()).unwrap_or(4);
        // SAFETY: `dword` is a stack-allocated u32; `size` tracks its
        // byte length.
        let rc = unsafe {
            RegGetValueW(
                hkey,
                ptr::null(),
                value_w.as_ptr(),
                RRF_RT_REG_DWORD,
                ptr::null_mut(),
                (&raw mut dword).cast(),
                &raw mut size,
            )
        };
        // SAFETY: `hkey` was opened successfully above.
        unsafe { RegCloseKey(hkey) };
        if rc != ERROR_SUCCESS {
            return ColorScheme::Auto;
        }
        // AppsUseLightTheme: 1 = light, 0 = dark.
        if dword == 0 {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        }
    }
}
