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
