//! Files dropped into a Flatpak sandbox, through the document portal.
//!
//! A sandboxed app cannot open the host paths a `text/uri-list` names.
//! Sources that know about portals (GTK, Qt, Firefox, file managers)
//! also offer `application/vnd.portal.filetransfer`: its payload is a
//! key, and `org.freedesktop.portal.FileTransfer.RetrieveFiles` trades
//! it for paths under the document portal that the sandbox can read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::zvariant::Value;

/// MIME type of a portal file transfer.
pub(super) const FILE_TRANSFER: &str = "application/vnd.portal.filetransfer";

/// Whether this process runs inside a Flatpak sandbox.
pub(super) fn sandboxed() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Trade the key a drag carried for the dropped files' paths.
pub(super) fn retrieve(payload: &[u8]) -> Result<Vec<PathBuf>, String> {
    let key = std::str::from_utf8(payload)
        .map_err(|err| format!("transfer key is not text: {err}"))?
        .trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if key.is_empty() {
        return Err("empty transfer key".to_owned());
    }
    let connection = zbus::blocking::Connection::session().map_err(|err| err.to_string())?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let reply = connection
        .call_method(
            Some("org.freedesktop.portal.Documents"),
            "/org/freedesktop/portal/documents",
            Some("org.freedesktop.portal.FileTransfer"),
            "RetrieveFiles",
            &(key, options),
        )
        .map_err(|err| err.to_string())?;
    let paths: Vec<String> = reply.body().deserialize().map_err(|err| err.to_string())?;
    Ok(paths.into_iter().map(PathBuf::from).collect())
}

/// Register `files` for a transfer out of the sandbox; the key goes to
/// the target, which trades it with the portal for the files.
pub(super) fn start_transfer(files: &[PathBuf]) -> Result<String, String> {
    use zbus::zvariant::Fd;

    let connection = zbus::blocking::Connection::session().map_err(|err| err.to_string())?;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("writable", Value::from(false));
    options.insert("autostop", Value::from(true));
    let reply = connection
        .call_method(
            Some("org.freedesktop.portal.Documents"),
            "/org/freedesktop/portal/documents",
            Some("org.freedesktop.portal.FileTransfer"),
            "StartTransfer",
            &(options,),
        )
        .map_err(|err| err.to_string())?;
    let key: String = reply.body().deserialize().map_err(|err| err.to_string())?;
    let opened: Vec<std::fs::File> = files
        .iter()
        .map(std::fs::File::open)
        .collect::<Result<_, _>>()
        .map_err(|err| err.to_string())?;
    let fds: Vec<Fd<'_>> = opened.iter().map(Fd::from).collect();
    let none: HashMap<&str, Value<'_>> = HashMap::new();
    connection
        .call_method(
            Some("org.freedesktop.portal.Documents"),
            "/org/freedesktop/portal/documents",
            Some("org.freedesktop.portal.FileTransfer"),
            "AddFiles",
            &(key.as_str(), fds, none),
        )
        .map_err(|err| err.to_string())?;
    Ok(key)
}
