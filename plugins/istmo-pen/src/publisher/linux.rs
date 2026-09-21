//! Linux integration surface.
//!
//! Unlike Windows (`WM_POINTER*`) and macOS (`NSEvent` local monitor),
//! Linux has no first-party per-window stylus event stream a plugin can
//! subscribe to. There are two viable paths, both of which require
//! integration outside this crate's scope:
//!
//! - **Compositor protocol (recommended for standard apps).** Wayland
//!   apps hook `zwp_tablet_manager_v2` on the display and receive
//!   tablet-tool events routed to the app's own `wl_surface`. X11 apps
//!   use `XInput2` tablet devices with `XIQueryDevice`. Either way the
//!   app decodes tool axes, builds a [`crate::PenEvent`], and forwards
//!   it via [`super::PenPublisher::push_event`].
//!
//! - **Raw libinput seat (kiosks, compositors).** Apps running with
//!   direct `/dev/input/*` access (or a `systemd-logind` seat) can open
//!   a `libinput` context, filter `LIBINPUT_DEVICE_CAP_TABLET_TOOL`,
//!   and push samples the same way. Not viable for regular
//!   Wayland/X11 client apps because they lack the required
//!   capabilities.
//!
//! In both cases the sample decode logic is the same as the built-in
//! Windows and macOS publishers — pressure/tilt/rotation normalized,
//! timestamps monotonic microseconds since attach — but the transport
//! is app-side. See the [`super::PenPublisher::push_event`] and
//! [`super::PenPublisher::push_hover`] docs for the escape hatch.
//!
//! A future milestone may ship a `libinput`-backed publisher gated
//! behind a config knob for compositor / kiosk consumers; that is
//! deliberately deferred until an actual consumer justifies the
//! `libinput-sys` dependency and permissions story.
