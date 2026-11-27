//! KDE Plasma job views (`org.kde.JobViewServerV2` / `org.kde.JobViewV3`):
//! the progress UI Plasma draws for transfers, with a cancel button.
//! Used instead of a notification while one has progress, because
//! Plasma ignores the `value` hint.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::message::Flags;
use zbus::zvariant::{OwnedObjectPath, Value};

pub(super) const SERVER: &str = "org.kde.JobViewServer";
const SERVER_PATH: &str = "/JobViewServer";
const SERVER_IFACE: &str = "org.kde.JobViewServerV2";
pub(super) const VIEW_IFACE: &str = "org.kde.JobViewV3";

/// The user can cancel the job (`cancelRequested`).
const KILLABLE: i32 = 0x0001;

/// What a job view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JobProps<'a> {
    pub(super) title: &'a str,
    pub(super) info: Option<&'a str>,
    pub(super) percent: Option<u32>,
}

impl JobProps<'_> {
    fn dict(&self) -> HashMap<&'static str, Value<'_>> {
        // `request` adds two more.
        let mut out = HashMap::with_capacity(5);
        out.insert("title", Value::from(self.title));
        if let Some(info) = self.info {
            out.insert("infoMessage", Value::from(info));
        }
        if let Some(percent) = self.percent {
            out.insert("percent", Value::from(percent));
        }
        out
    }
}

/// Whether a job server is on the bus.
pub(super) fn available(conn: &Connection) -> bool {
    zbus::blocking::fdo::DBusProxy::new(conn)
        .and_then(|proxy| {
            proxy
                .name_has_owner(SERVER.try_into()?)
                .map_err(zbus::Error::from)
        })
        .unwrap_or(false)
}

/// A new job view for `desktop_entry`; its object path.
pub(super) fn request(
    conn: &Connection,
    desktop_entry: &str,
    props: &JobProps<'_>,
) -> zbus::Result<String> {
    let mut hints = props.dict();
    // Shown at once; the finished view goes away (the plugin posts the
    // final notification itself).
    hints.insert("immediate", Value::from(true));
    hints.insert("transient", Value::from(true));
    let reply = conn.call_method(
        Some(SERVER),
        SERVER_PATH,
        Some(SERVER_IFACE),
        "requestView",
        &(desktop_entry, KILLABLE, hints),
    )?;
    let path: OwnedObjectPath = reply.body().deserialize()?;
    let path = path.as_str().to_owned();
    // Plasma before 5.22 reads only `update`.
    update(conn, &path, props)?;
    Ok(path)
}

/// New properties for the view at `path`.
pub(super) fn update(conn: &Connection, path: &str, props: &JobProps<'_>) -> zbus::Result<()> {
    conn.send(&no_reply(path, "update")?.build(&(props.dict(),))?)
}

/// Ends the view at `path` (no error).
pub(super) fn terminate(conn: &Connection, path: &str) -> zbus::Result<()> {
    let hints: HashMap<&str, Value<'_>> = HashMap::new();
    conn.send(&no_reply(path, "terminate")?.build(&(0_u32, "", hints))?)
}

/// `update` and `terminate` are `NoReply` methods: waiting for an answer
/// would wait for the call timeout.
fn no_reply<'a>(path: &'a str, member: &'a str) -> zbus::Result<zbus::message::Builder<'a>> {
    zbus::Message::method_call(path, member)?
        .destination(SERVER)?
        .interface(VIEW_IFACE)?
        .with_flags(Flags::NoReplyExpected)
}
