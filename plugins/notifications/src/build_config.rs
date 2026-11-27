//! What the app's `istmo.toml` hands to this crate at build time.
//!
//! `istmo-build` turns `[[app.notification_channel]]` into the
//! `ISTMO_NOTIFICATION_CHANNELS` rustc env of the app crate (hex wire
//! bytes of `Vec<ChannelSpec>`). The env belongs to the app, so it is
//! read where [`static_channels!`](crate::static_channels) expands:
//!
//! ```no_run
//! # #[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
//! # fn demo() {
//! use istmo_notifications::DesktopNotifications;
//!
//! let host = DesktopNotifications::platform()
//!     .with_channels(istmo_notifications::static_channels!())
//!     .build();
//! # drop(host);
//! # }
//! ```

use crate::ChannelSpec;

/// The static channels of the app crate (`[[app.notification_channel]]`),
/// empty when it declares none or does not build with `istmo-build`.
#[macro_export]
macro_rules! static_channels {
    () => {
        $crate::channels_from_build(::core::option_env!("ISTMO_NOTIFICATION_CHANNELS"))
    };
}

/// Decode the `ISTMO_NOTIFICATION_CHANNELS` value. `None`, or a value
/// this version cannot read (logged at warn), is no channel.
#[must_use]
pub fn channels_from_build(hex: Option<&str>) -> Vec<ChannelSpec> {
    let Some(hex) = hex.filter(|hex| !hex.is_empty()) else {
        return Vec::new();
    };
    let decoded = hex_decode(hex).and_then(|bytes| {
        istmo_core::codec::decode_exact::<Vec<ChannelSpec>>(&bytes).map_err(|err| err.to_string())
    });
    match decoded {
        Ok(channels) => channels,
        Err(err) => {
            tracing::warn!(
                "ISTMO_NOTIFICATION_CHANNELS is unreadable ({err}); was the app built with \
                 another istmo-build version? Static channels skipped"
            );
            Vec::new()
        }
    }
}

fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    let digit = |byte: u8| -> Result<u8, String> {
        char::from(byte)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
            .ok_or_else(|| format!("`{}` is not a hex digit", char::from(byte)))
    };
    if !hex.len().is_multiple_of(2) {
        return Err("odd number of hex digits".to_owned());
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_and_garbage_are_no_channels() {
        assert_eq!(channels_from_build(None), Vec::new());
        assert_eq!(channels_from_build(Some("")), Vec::new());
        assert_eq!(channels_from_build(Some("0")), Vec::new());
        assert_eq!(channels_from_build(Some("zz")), Vec::new());
        // One channel announced, none present.
        assert_eq!(channels_from_build(Some("01")), Vec::new());
    }
}
