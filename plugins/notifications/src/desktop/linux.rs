//! Linux notifications over the session bus.
//!
//! `org.freedesktop.Notifications` (the notification server) or
//! `org.freedesktop.portal.Notification` (sandboxed apps, and host apps
//! the portal's registry accepts), with KDE `kuiserver` job views for
//! progress on Plasma.
//!
//! Route ([`LinuxRoute::Auto`]):
//!
//! * Inside Flatpak or Snap (`/.flatpak-info`, `container=flatpak`,
//!   `SNAP`): the portal when it serves the Notification interface, the
//!   server otherwise (a sandbox allowed to talk to it).
//! * On the host: the portal when
//!   `org.freedesktop.host.portal.Registry.Register` accepts the app id
//!   (an installed `.desktop` file with that basename), the server
//!   otherwise.
//!
//! App id: `FLATPAK_ID`, else [`LinuxOptions::with_app_id`], else the
//! executable's file name (the launcher-actions rule). It is the
//! `desktop-entry` hint, the portal registration and the bus name of the
//! exported portal actions.
//!
//! One session-bus connection per backend; signals are read on named
//! threads (`istmo-notif-*`) that hold the backend weakly and call the
//! host's sink without holding a backend lock.

mod app_activation;
mod fdo;
mod kde_job;
mod portal;

use std::path::Path;
use std::sync::{Arc, Mutex};

use istmo_core::sync::lock;
use zbus::blocking::Connection;

use super::{Backend, BackendSink, ImageFiles, ImageSlot, StartBackend};
use crate::{Notification, NotificationError, NotificationImage, PlatformError, Style};

/// Which Linux service shows the notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum LinuxRoute {
    /// The portal when sandboxed or registered, else the server.
    #[default]
    Auto,
    /// `org.freedesktop.Notifications`, also inside a sandbox.
    Server,
    /// `org.freedesktop.portal.Notification`; fails to start without it.
    Portal,
}

/// Linux options of [`super::DesktopNotifications`]:
/// `DesktopNotifications::builder(LinuxOptions::new().with_app_id(id).start())`.
#[derive(Debug, Clone, Default)]
pub struct LinuxOptions {
    app_id: Option<String>,
    route: LinuxRoute,
}

impl LinuxOptions {
    /// Defaults: [`LinuxRoute::Auto`], app id from the environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The app id (`<id>.desktop` as installed), usually `[app] id`.
    /// `FLATPAK_ID` still wins inside Flatpak.
    #[must_use]
    pub fn with_app_id(mut self, id: impl Into<String>) -> Self {
        self.app_id = Some(id.into());
        self
    }

    /// Forces a route.
    #[must_use]
    pub const fn with_route(mut self, route: LinuxRoute) -> Self {
        self.route = route;
        self
    }

    /// The start function for [`super::DesktopNotifications::builder`].
    #[must_use]
    pub fn start(self) -> StartBackend {
        Box::new(move |sink| self.open(sink))
    }

    fn open(&self, sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
        let app_id = app_id(
            std::env::var("FLATPAK_ID").ok().as_deref(),
            self.app_id.as_deref(),
        );
        let conn = Connection::session().map_err(platform)?;
        match self.route {
            LinuxRoute::Server => Ok(fdo::Fdo::open(conn, app_id, sink)?),
            LinuxRoute::Portal => portal::Portal::open(conn, &app_id, sink)?.ok_or_else(|| {
                NotificationError::Platform(PlatformError {
                    code: 0,
                    message: "the notification portal is not available".to_owned(),
                })
            }),
            LinuxRoute::Auto => {
                let use_portal = sandboxed() || portal::register_host(&conn, &app_id);
                if use_portal
                    && let Some(portal) = portal::Portal::open(conn.clone(), &app_id, sink.clone())?
                {
                    return Ok(portal);
                }
                Ok(fdo::Fdo::open(conn, app_id, sink)?)
            }
        }
    }
}

/// The backend of `DesktopNotifications::default()` on Linux.
pub(super) fn platform_backend(sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    LinuxOptions::new().open(sink)
}

/// The XDG activation token of the latest activation, taken once.
///
/// It comes from the fdo `ActivationToken` signal or the portal's
/// `activation-token`. Pass it to the toolkit (`xdg_activation_v1`, `XDG_ACTIVATION_TOKEN`) when the
/// response raises a window, so the compositor lets it take focus.
#[must_use]
pub fn take_activation_token() -> Option<String> {
    lock(&TOKEN).take()
}

static TOKEN: Mutex<Option<String>> = Mutex::new(None);

fn set_activation_token(token: String) {
    if !token.is_empty() {
        *lock(&TOKEN) = Some(token);
    }
}

/// Inside Flatpak or Snap.
fn sandboxed() -> bool {
    Path::new("/.flatpak-info").exists()
        || std::env::var_os("container").is_some_and(|c| c == "flatpak")
        || std::env::var_os("SNAP").is_some()
}

/// `FLATPAK_ID`, else the configured id, else the executable's name.
fn app_id(flatpak: Option<&str>, configured: Option<&str>) -> String {
    flatpak
        .filter(|id| !id.is_empty())
        .or_else(|| configured.filter(|id| !id.is_empty()))
        .map(|id| id.strip_suffix(".desktop").unwrap_or(id).to_owned())
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .file_stem()?
                .to_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "istmo-app".to_owned())
}

#[expect(clippy::needless_pass_by_value, reason = "used as `map_err(platform)`")]
fn platform(err: zbus::Error) -> NotificationError {
    let code = match &err {
        zbus::Error::MethodError(..) => 1,
        zbus::Error::InputOutput(_) => 2,
        _ => 0,
    };
    NotificationError::Platform(PlatformError {
        code,
        message: err.to_string(),
    })
}

/// The image the notification shows: a file (from the host's cache or
/// as given) or an icon-theme name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picked<'a> {
    File(&'a Path),
    Named(&'a str),
}

/// The participant whose avatar stands for the notification.
fn avatar_of(n: &Notification) -> Option<(&str, Option<&NotificationImage>)> {
    match &n.style {
        Style::Call(call) => Some((call.caller.key.as_str(), call.caller.avatar.as_ref())),
        Style::Messaging(conversation) => conversation
            .messages
            .iter()
            .rev()
            .find_map(|m| m.sender.as_ref())
            .map(|p| (p.key.as_str(), p.avatar.as_ref())),
        _ => None,
    }
}

/// Picks one image: `picture` first when asked, then the icon, then the
/// avatar of the caller / last sender.
fn pick_image<'a>(n: &'a Notification, files: &'a ImageFiles, picture: bool) -> Option<Picked<'a>> {
    let one = |slot: ImageSlot, image: Option<&'a NotificationImage>| {
        files.get(&slot).map(Picked::File).or(match image {
            Some(NotificationImage::Named(name)) => Some(Picked::Named(name.as_str())),
            _ => None,
        })
    };
    let pic = match &n.style {
        Style::Picture(image) if picture => one(ImageSlot::Picture, Some(image)),
        _ => None,
    };
    pic.or_else(|| one(ImageSlot::Icon, n.icon.as_ref()))
        .or_else(|| {
            let (key, image) = avatar_of(n)?;
            one(ImageSlot::Avatar(key.to_owned()), image)
        })
}

/// `file://` URI of an absolute path, percent-encoding every byte outside
/// the RFC 3986 unreserved set and `/`.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = path.as_os_str().as_bytes();
    let mut out = String::with_capacity(7 + bytes.len());
    out.push_str("file://");
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

/// The unique name owning a well-known name, cached: signals are
/// accepted only from it.
#[derive(Debug)]
struct Owner {
    name: &'static str,
    unique: Mutex<Option<String>>,
}

impl Owner {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            unique: Mutex::new(None),
        }
    }

    /// Whether `sender` owns the name; asks the bus again on a mismatch
    /// (the owner may have changed since it was cached).
    fn is(&self, conn: &Connection, sender: &str) -> bool {
        if lock(&self.unique).as_deref() == Some(sender) {
            return true;
        }
        let owner = zbus::blocking::fdo::DBusProxy::new(conn)
            .ok()
            .and_then(|proxy| proxy.get_name_owner(self.name.try_into().ok()?).ok())
            .map(|owner| owner.to_string());
        let same = owner.as_deref() == Some(sender);
        *lock(&self.unique) = owner;
        same
    }

    fn forget(&self) {
        *lock(&self.unique) = None;
    }
}

/// Runs `f` on a named thread.
fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), NotificationError> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .map(drop)
        .map_err(|err| {
            NotificationError::Platform(PlatformError {
                code: 0,
                message: format!("cannot start {name}: {err}"),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_prefers_flatpak_then_configured_then_exe() {
        assert_eq!(app_id(Some("org.x.App"), Some("dev.y")), "org.x.App");
        assert_eq!(app_id(Some(""), Some("dev.y.desktop")), "dev.y");
        assert!(!app_id(None, None).is_empty());
    }

    #[test]
    fn file_uri_escapes() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/ü.png")),
            "file:///tmp/a%20b/%C3%BC.png"
        );
    }
}
