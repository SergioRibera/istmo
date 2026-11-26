//! One X connection: atoms, extensions, monitors (`RandR`), windows
//! (EWMH client list) and the pointer.

use std::collections::HashMap;
use std::fmt;

use x11rb::connection::Connection as _;
use x11rb::protocol::composite::ConnectionExt as _;
use x11rb::protocol::damage::ConnectionExt as _;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{
    self, AtomEnum, ConnectionExt as _, MapState, Window as XWindow, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::types::{
    BackendId, CaptureError, Orientation, Rect, Size, Source, SourceFlags, SourceId, SourceKind,
};

pub(super) const ID: BackendId = BackendId::LinuxX11;

/// A failed X request, named by its step.
pub(super) fn x_err(step: &str, err: impl fmt::Display) -> CaptureError {
    CaptureError::protocol(ID, step, err)
}

/// A failed reply: a drawable that no longer exists (or is unmapped)
/// is `SourceLost`, anything else names the step.
pub(super) fn lost_or(step: &str, err: x11rb::errors::ReplyError) -> CaptureError {
    use x11rb::protocol::ErrorKind;
    match &err {
        x11rb::errors::ReplyError::X11Error(e)
            if matches!(
                e.error_kind,
                ErrorKind::Drawable | ErrorKind::Window | ErrorKind::Match | ErrorKind::Pixmap
            ) =>
        {
            CaptureError::SourceLost
        }
        _ => x_err(step, err),
    }
}

/// Extensions the backend uses, as the server reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one extension the server may lack"
)]
pub(super) struct Extensions {
    /// MIT-SHM ≥ 1.2 (`AttachFd`): a memfd segment, no socket copy.
    pub(super) shm_fd: bool,
    pub(super) damage: bool,
    pub(super) xfixes: bool,
    /// Composite ≥ 0.2 (`NameWindowPixmap`): occluded window content.
    pub(super) composite: bool,
    pub(super) randr_monitors: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Atoms {
    pub(super) net_client_list: xproto::Atom,
    pub(super) net_wm_name: xproto::Atom,
    pub(super) net_wm_pid: xproto::Atom,
    pub(super) net_wm_state: xproto::Atom,
    pub(super) net_wm_state_hidden: xproto::Atom,
    pub(super) net_active_window: xproto::Atom,
    pub(super) utf8_string: xproto::Atom,
}

/// How a source maps to X: a rectangle of the root window or a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// A monitor: this root-window rectangle.
    Monitor(Rect),
    Window(XWindow),
}

/// The value part of a monitor source id.
fn monitor_value(name: &str) -> String {
    format!("monitor:{name}")
}

/// The value part of a window source id.
pub(super) fn window_value(window: XWindow) -> String {
    format!("window:{window:#x}")
}

/// The window a source id names, if it names one.
pub(super) fn parse_window(value: &str) -> Option<XWindow> {
    let hex = value.strip_prefix("window:")?.strip_prefix("0x")?;
    XWindow::from_str_radix(hex, 16).ok()
}

/// Refresh rate of a `RandR` mode, in millihertz.
pub(super) fn refresh_mhz(dot_clock: u32, htotal: u16, vtotal: u16) -> Option<u32> {
    let frame = u64::from(htotal) * u64::from(vtotal);
    if frame == 0 || dot_clock == 0 {
        return None;
    }
    u32::try_from(u64::from(dot_clock) * 1000 / frame).ok()
}

pub(super) struct X11 {
    pub(super) conn: RustConnection,
    pub(super) root: XWindow,
    pub(super) root_depth: u8,
    pub(super) root_size: Size,
    /// Whether the screen offers a 32-bit (ARGB) visual.
    pub(super) argb_visual: bool,
    pub(super) ext: Extensions,
    pub(super) atoms: Atoms,
}

impl fmt::Debug for X11 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("X11")
            .field("root", &self.root)
            .field("root_depth", &self.root_depth)
            .field("ext", &self.ext)
            .finish_non_exhaustive()
    }
}

impl X11 {
    /// Connect to `$DISPLAY` and query what the backend needs.
    ///
    /// # Errors
    /// No server, or a server without screens.
    pub(super) fn connect() -> Result<Self, CaptureError> {
        let (conn, screen) = RustConnection::connect(None).map_err(|e| x_err("connect", e))?;
        let setup = conn.setup();
        let screen = setup
            .roots
            .get(screen)
            .ok_or_else(|| x_err("connect", "the X server has no screens"))?;
        let root = screen.root;
        let root_depth = screen.root_depth;
        let root_size = Size::new(
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );
        let argb_visual = screen
            .allowed_depths
            .iter()
            .any(|d| d.depth == 32 && !d.visuals.is_empty());
        let intern = |name: &[u8]| -> Result<xproto::Atom, CaptureError> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(|e| x_err("intern atom", e))?
                .reply()
                .map_err(|e| x_err("intern atom", e))?
                .atom)
        };
        let atoms = Atoms {
            net_client_list: intern(b"_NET_CLIENT_LIST")?,
            net_wm_name: intern(b"_NET_WM_NAME")?,
            net_wm_pid: intern(b"_NET_WM_PID")?,
            net_wm_state: intern(b"_NET_WM_STATE")?,
            net_wm_state_hidden: intern(b"_NET_WM_STATE_HIDDEN")?,
            net_active_window: intern(b"_NET_ACTIVE_WINDOW")?,
            utf8_string: intern(b"UTF8_STRING")?,
        };
        let ext = Self::extensions(&conn);
        Ok(Self {
            conn,
            root,
            root_depth,
            root_size,
            argb_visual,
            ext,
            atoms,
        })
    }

    fn extensions(conn: &RustConnection) -> Extensions {
        // Every extension must be asked for its version before use; a
        // missing one answers an error and the feature is off.
        let shm_fd = conn
            .shm_query_version()
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 2));
        let damage = conn
            .damage_query_version(1, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let xfixes = conn
            .xfixes_query_version(5, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| v.major_version >= 1);
        let composite = conn
            .composite_query_version(0, 4)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (0, 2));
        let randr_monitors = conn
            .randr_query_version(1, 5)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 5));
        Extensions {
            shm_fd,
            damage,
            xfixes,
            composite,
            randr_monitors,
        }
    }

    fn atom_name(&self, atom: xproto::Atom) -> Option<String> {
        let reply = self.conn.get_atom_name(atom).ok()?.reply().ok()?;
        String::from_utf8(reply.name).ok()
    }

    /// Refresh rates of the CRTCs, by CRTC.
    fn crtc_refresh(&self) -> HashMap<randr::Crtc, u32> {
        let Some(resources) = self
            .conn
            .randr_get_screen_resources_current(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return HashMap::new();
        };
        let modes: HashMap<u32, u32> = resources
            .modes
            .iter()
            .filter_map(|m| refresh_mhz(m.dot_clock, m.htotal, m.vtotal).map(|r| (m.id, r)))
            .collect();
        resources
            .crtcs
            .iter()
            .filter_map(|crtc| {
                let info = self.conn.randr_get_crtc_info(*crtc, 0).ok()?.reply().ok()?;
                modes.get(&info.mode).map(|r| (*crtc, *r))
            })
            .collect()
    }

    /// Monitors (`RandR` 1.5 `GetMonitors`, else the whole screen).
    ///
    /// # Errors
    /// The `RandR` request failed.
    pub(super) fn monitors(&self) -> Result<Vec<Source>, CaptureError> {
        let whole_screen = || {
            let mut source = Source::new(
                SourceId::new(ID, monitor_value("screen")),
                SourceKind::Monitor,
                "screen",
            );
            source.geometry = Some(Rect::from_size(self.root_size));
            source.physical_size = Some(self.root_size);
            source.scale_permille = Some(1000);
            source.flags.primary = true;
            source
        };
        if !self.ext.randr_monitors {
            return Ok(vec![whole_screen()]);
        }
        let reply = self
            .conn
            .randr_get_monitors(self.root, true)
            .map_err(|e| x_err("randr get monitors", e))?
            .reply()
            .map_err(|e| x_err("randr get monitors", e))?;
        let refresh = self.crtc_refresh();
        let crtc_of = |outputs: &[randr::Output]| -> Option<randr::Crtc> {
            outputs.iter().find_map(|o| {
                let info = self.conn.randr_get_output_info(*o, 0).ok()?.reply().ok()?;
                (info.crtc != 0).then_some(info.crtc)
            })
        };
        let monitors: Vec<Source> = reply
            .monitors
            .iter()
            .map(|m| {
                let name = self
                    .atom_name(m.name)
                    .unwrap_or_else(|| format!("monitor-{}", m.name));
                let mut source = Source::new(
                    SourceId::new(ID, monitor_value(&name)),
                    SourceKind::Monitor,
                    name,
                );
                let size = Size::new(u32::from(m.width), u32::from(m.height));
                source.geometry = Some(Rect::new(
                    i32::from(m.x),
                    i32::from(m.y),
                    size.width,
                    size.height,
                ));
                source.physical_size = Some(size);
                // X11 has no per-monitor scale.
                source.scale_permille = Some(1000);
                source.refresh_mhz = crtc_of(&m.outputs).and_then(|c| refresh.get(&c).copied());
                // RandR rotation is already applied in the root window:
                // a rectangle of it reads upright.
                source.orientation = Orientation::UPRIGHT;
                source.flags.primary = m.primary;
                source
            })
            .collect();
        if monitors.is_empty() {
            return Ok(vec![whole_screen()]);
        }
        Ok(monitors)
    }

    fn property(
        &self,
        window: XWindow,
        property: xproto::Atom,
        ty: impl Into<xproto::Atom>,
    ) -> Option<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, window, property, ty, 0, 1 << 16)
            .ok()?
            .reply()
            .ok()?;
        (!reply.value.is_empty()).then_some(reply.value)
    }

    fn property32(
        &self,
        window: XWindow,
        property: xproto::Atom,
        ty: impl Into<xproto::Atom>,
    ) -> Vec<u32> {
        self.conn
            .get_property(false, window, property, ty, 0, 1 << 16)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(Iterator::collect))
            .unwrap_or_default()
    }

    /// Managed windows (`_NET_CLIENT_LIST`); without a window manager,
    /// the mapped top-level windows.
    fn window_ids(&self) -> Result<Vec<XWindow>, CaptureError> {
        let managed = self.property32(self.root, self.atoms.net_client_list, AtomEnum::WINDOW);
        if !managed.is_empty() {
            return Ok(managed);
        }
        let tree = self
            .conn
            .query_tree(self.root)
            .map_err(|e| x_err("query tree", e))?
            .reply()
            .map_err(|e| x_err("query tree", e))?;
        Ok(tree
            .children
            .into_iter()
            .filter(|w| {
                self.conn
                    .get_window_attributes(*w)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_some_and(|a| {
                        a.map_state == MapState::VIEWABLE && a.class == WindowClass::INPUT_OUTPUT
                    })
            })
            .collect())
    }

    /// Root-relative geometry of `window` (border excluded).
    pub(super) fn window_rect(&self, window: XWindow) -> Result<Rect, CaptureError> {
        let geometry = self
            .conn
            .get_geometry(window)
            .map_err(|e| x_err("get geometry", e))?
            .reply()
            .map_err(|e| lost_or("get geometry", e))?;
        let at = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)
            .map_err(|e| x_err("translate coordinates", e))?
            .reply()
            .map_err(|e| lost_or("translate coordinates", e))?;
        Ok(Rect::new(
            i32::from(at.dst_x),
            i32::from(at.dst_y),
            u32::from(geometry.width),
            u32::from(geometry.height),
        ))
    }

    /// Whether `window` is viewable (mapped, with mapped ancestors).
    pub(super) fn viewable(&self, window: XWindow) -> Result<bool, CaptureError> {
        let attributes = self
            .conn
            .get_window_attributes(window)
            .map_err(|e| x_err("get window attributes", e))?
            .reply()
            .map_err(|e| lost_or("get window attributes", e))?;
        Ok(attributes.map_state == MapState::VIEWABLE)
    }

    fn describe(&self, window: XWindow, active: Option<XWindow>) -> Option<Source> {
        let rect = self.window_rect(window).ok()?;
        let title = self
            .property(window, self.atoms.net_wm_name, self.atoms.utf8_string)
            .or_else(|| self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::STRING))
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .unwrap_or_default();
        // WM_CLASS: instance and class, NUL-terminated; the class names
        // the application.
        let app_id = self
            .property(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING)
            .and_then(|v| {
                v.split(|b| *b == 0)
                    .filter(|p| !p.is_empty())
                    .nth(1)
                    .map(|c| String::from_utf8_lossy(c).into_owned())
            });
        let pid = self
            .property32(window, self.atoms.net_wm_pid, AtomEnum::CARDINAL)
            .first()
            .copied();
        let states = self.property32(window, self.atoms.net_wm_state, AtomEnum::ATOM);
        let mut source = Source::new(
            SourceId::new(ID, window_value(window)),
            SourceKind::Window,
            title,
        );
        source.app_id = app_id;
        source.pid = pid;
        source.geometry = Some(rect);
        source.physical_size = Some(rect.size());
        source.scale_permille = Some(1000);
        source.flags = SourceFlags {
            primary: active == Some(window),
            minimised: states.contains(&self.atoms.net_wm_state_hidden)
                || !self.viewable(window).unwrap_or(false),
            occluded: false,
            protected: false,
            own_process: pid == Some(std::process::id()),
        };
        Some(source)
    }

    /// Capturable windows.
    ///
    /// # Errors
    /// The window list could not be read.
    pub(super) fn windows(&self) -> Result<Vec<Source>, CaptureError> {
        let active = self.active_window();
        Ok(self
            .window_ids()?
            .into_iter()
            .filter_map(|w| self.describe(w, active))
            .collect())
    }

    /// `_NET_ACTIVE_WINDOW`, else the input focus.
    pub(super) fn active_window(&self) -> Option<XWindow> {
        let active = self
            .property32(self.root, self.atoms.net_active_window, AtomEnum::WINDOW)
            .first()
            .copied()
            .filter(|w| *w != 0);
        active.or_else(|| {
            let focus = self.conn.get_input_focus().ok()?.reply().ok()?.focus;
            (focus > 1 && focus != self.root).then_some(focus)
        })
    }

    /// The focused window as a source.
    ///
    /// # Errors
    /// `SourceNotFound` when nothing has the focus.
    pub(super) fn focused(&self) -> Result<Source, CaptureError> {
        let not_found = || CaptureError::SourceNotFound(SourceId::new(ID, "focused"));
        let window = self.active_window().ok_or_else(not_found)?;
        self.describe(window, Some(window)).ok_or_else(not_found)
    }

    /// The pointer position on the root window.
    ///
    /// # Errors
    /// The query failed.
    pub(super) fn pointer(&self) -> Result<(i32, i32), CaptureError> {
        let reply = self
            .conn
            .query_pointer(self.root)
            .map_err(|e| x_err("query pointer", e))?
            .reply()
            .map_err(|e| x_err("query pointer", e))?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    /// The monitor under the pointer.
    ///
    /// # Errors
    /// The pointer is outside every monitor, or a query failed.
    pub(super) fn under_cursor(&self) -> Result<Source, CaptureError> {
        let (x, y) = self.pointer()?;
        let here = Rect::new(x, y, 1, 1);
        self.monitors()?
            .into_iter()
            .find(|m| m.geometry.is_some_and(|g| g.contains(here)))
            .ok_or_else(|| CaptureError::SourceNotFound(SourceId::new(ID, "under-cursor")))
    }

    /// What `source` means to X.
    ///
    /// # Errors
    /// `SourceNotFound` for an id this backend did not issue.
    pub(super) fn target(source: &Source) -> Result<Target, CaptureError> {
        match source.kind {
            SourceKind::Monitor => source
                .geometry
                .map(Target::Monitor)
                .ok_or_else(|| CaptureError::SourceNotFound(source.id.clone())),
            SourceKind::Window => parse_window(&source.id.value)
                .map(Target::Window)
                .ok_or_else(|| CaptureError::SourceNotFound(source.id.clone())),
            SourceKind::Application => Err(CaptureError::unsatisfiable(
                Some(ID),
                crate::types::Requirement::Kind(SourceKind::Application),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_ids_round_trip() {
        assert_eq!(parse_window(&window_value(0x1a0_0003)), Some(0x1a0_0003));
        assert_eq!(parse_window("monitor:DP-1"), None);
        assert_eq!(parse_window("window:zz"), None);
    }

    #[test]
    fn refresh_comes_from_the_mode_timings() {
        // 1920x1080@60: 148.5 MHz over 2200x1125.
        assert_eq!(refresh_mhz(148_500_000, 2200, 1125), Some(60_000));
        assert_eq!(refresh_mhz(0, 2200, 1125), None);
        assert_eq!(refresh_mhz(148_500_000, 0, 1125), None);
    }
}
