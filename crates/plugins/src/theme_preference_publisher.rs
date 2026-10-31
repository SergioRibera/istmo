//! Pure-Rust desktop sampler for
//! [`super::theme_preference::ThemePreferenceClient`].
//!
//! Covers the single most-asked signal — the user's light/dark
//! preference — on Linux (gsettings), macOS (`defaults read` or
//! objc2 NSAppearance) and Windows (Registry `AppsUseLightTheme`).
//! Fonts scale, accessibility flags and accent-colour pickers vary
//! wildly across desktop DEs; leave those to the per-app observer
//! until a real consumer surfaces.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::theme_preference::{
    ColorScheme, THEME_PREFERENCE_CHANNEL, THEME_PREFERENCE_PLUGIN_ID, ThemePreference,
};

#[derive(Debug)]
pub struct ThemePreferencePublisher {
    runtime: Weak<Runtime>,
    last: Option<ThemePreference>,
}

impl ThemePreferencePublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(THEME_PREFERENCE_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

    pub fn tick(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let Some(sample) = sample() else {
            return;
        };
        if self.last.as_ref() == Some(&sample) {
            return;
        }
        match codec::encode(&sample) {
            Ok(bytes) => {
                runtime.publish_early_latest(THEME_PREFERENCE_CHANNEL, bytes);
                self.last = Some(sample);
            }
            Err(err) => {
                tracing::warn!(target = "istmo.theme_preference", ?err, "encode failed");
            }
        }
    }
}

#[allow(clippy::unnecessary_wraps)]
fn sample() -> Option<ThemePreference> {
    Some(ThemePreference {
        color_scheme: color_scheme(),
        accent_color: None,
        high_contrast: false,
        font_scale: 1.0,
        reduce_motion: false,
        reduce_transparency: false,
        bold_text: false,
        invert_colors: false,
        screen_reader_active: false,
    })
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
        const PATH: &str =
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
        const VALUE: &str = "AppsUseLightTheme";
        let path_w: Vec<u16> = PATH.encode_utf16().chain(Some(0)).collect();
        let value_w: Vec<u16> = VALUE.encode_utf16().chain(Some(0)).collect();
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: NUL-terminated UTF-16 paths; `hkey` is out-param.
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, KEY_READ, &raw mut hkey)
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
