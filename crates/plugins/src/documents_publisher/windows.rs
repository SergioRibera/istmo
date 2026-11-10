//! Named-pipe endpoint.
//!
//! Ownership is decided by `FILE_FLAG_FIRST_PIPE_INSTANCE`: creating
//! the first instance of a name is atomic, so two simultaneous
//! launches cannot both become primary. The pipe uses the default
//! security descriptor, which grants write access only to the creating
//! user, `SYSTEM` and administrators (others get read access, which is
//! not enough to send a request), and `PIPE_REJECT_REMOTE_CLIENTS`
//! keeps SMB clients out.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};

use istmo_core::Runtime;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use super::{
    CONNECT_ATTEMPTS, CONNECT_BACKOFF, DocumentsError, ForwardRequest, IO_TIMEOUT, InstanceKey,
    handle_client, send_request,
};

const PIPE_BUFFER: u32 = 64 * 1024;

/// `\\.\pipe\istmo-<key>-<user>`. The user name keeps the endpoints of
/// different accounts of one machine (fast user switching, terminal
/// server sessions) apart.
fn pipe_name(key: &InstanceKey) -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(r"\\.\pipe\istmo-{}-{user}", key.as_str())
}

/// Win32 error codes are `u32`; `io::Error` stores them as `i32`.
fn is_os_error(err: &io::Error, code: u32) -> bool {
    err.raw_os_error()
        .and_then(|raw| u32::try_from(raw).ok())
        == Some(code)
}

fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create one server instance of the pipe. `first` asks for exclusive
/// ownership of the name.
fn create_instance(name: &[u16], first: bool) -> io::Result<OwnedHandle> {
    let open_mode = PIPE_ACCESS_DUPLEX
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives
    // the call; a null security-attributes pointer selects the default
    // descriptor. The returned handle is checked before use.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a valid pipe handle we just created and
    // nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Block until a client connects to `instance`.
fn wait_for_client(instance: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the handle is a valid server end of a pipe opened without
    // `FILE_FLAG_OVERLAPPED`, so a null `OVERLAPPED` is allowed.
    let ok = unsafe { ConnectNamedPipe(instance.as_raw_handle(), std::ptr::null_mut()) };
    if ok != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // The client connected between `CreateNamedPipeW` and here.
    if is_os_error(&err, ERROR_PIPE_CONNECTED) {
        Ok(())
    } else {
        Err(err)
    }
}

#[derive(Debug)]
pub(super) struct Endpoint {
    name: Vec<u16>,
    path: String,
    first: OwnedHandle,
}

#[derive(Debug)]
pub(super) enum Claim {
    Primary(Endpoint),
    Secondary,
}

pub(super) fn claim(key: &InstanceKey) -> Result<Claim, DocumentsError> {
    let path = pipe_name(key);
    let name = wide(&path);
    match create_instance(&name, true) {
        Ok(first) => Ok(Claim::Primary(Endpoint { name, path, first })),
        Err(err) if is_os_error(&err, ERROR_ACCESS_DENIED) => {
            Ok(Claim::Secondary)
        }
        Err(err) => Err(DocumentsError::io("creating the pipe")(err)),
    }
}

fn open_client(path: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

pub(super) fn forward(key: &InstanceKey, request: &ForwardRequest) -> Result<(), DocumentsError> {
    let path = pipe_name(key);
    let request = request.clone();
    // Synchronous pipe handles have no read timeout: run the exchange on
    // a helper thread and stop waiting if the primary hangs.
    let (tx, rx) = flume::bounded(1);
    thread::Builder::new()
        .name("istmo-documents-forward".to_owned())
        .spawn(move || {
            let _ = tx.send(exchange(&path, &request));
        })
        .map_err(DocumentsError::io("spawning the forward thread"))?;
    let deadline = IO_TIMEOUT * 2 + CONNECT_BACKOFF * CONNECT_ATTEMPTS;
    rx.recv_timeout(deadline)
        .unwrap_or(Err(DocumentsError::Protocol(
            "the running instance did not answer",
        )))
}

fn exchange(path: &str, request: &ForwardRequest) -> Result<(), DocumentsError> {
    for _ in 0..CONNECT_ATTEMPTS {
        match open_client(path) {
            Ok(mut pipe) => return send_request(&mut pipe, request),
            Err(err)
                if is_os_error(&err, ERROR_PIPE_BUSY) || is_os_error(&err, ERROR_FILE_NOT_FOUND) =>
            {
                thread::sleep(CONNECT_BACKOFF);
            }
            Err(err) => {
                return Err(DocumentsError::io("connecting to the running instance")(
                    err,
                ));
            }
        }
    }
    Err(DocumentsError::Protocol(
        "the running instance is not listening",
    ))
}

#[derive(Debug)]
pub(super) struct Server {
    stop: Arc<AtomicBool>,
    path: String,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub(super) fn spawn(
        endpoint: Endpoint,
        runtime: Weak<Runtime>,
    ) -> Result<Self, DocumentsError> {
        let stop = Arc::new(AtomicBool::new(false));
        let path = endpoint.path.clone();
        let thread = thread::Builder::new()
            .name("istmo-documents".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || serve(endpoint, &stop, &runtime)
            })
            .map_err(DocumentsError::io("spawning the documents thread"))?;
        Ok(Self {
            stop,
            path,
            thread: Some(thread),
        })
    }
}

fn serve(endpoint: Endpoint, stop: &AtomicBool, runtime: &Weak<Runtime>) {
    let Endpoint { name, first, .. } = endpoint;
    let mut pending = first;
    loop {
        if let Err(err) = wait_for_client(&pending) {
            // e.g. `ERROR_NO_DATA`: the client left before we accepted.
            // The instance cannot be reused without disconnecting it;
            // replace it (the new one exists before the old closes).
            tracing::warn!(target: "istmo.documents", %err, "waiting for a client failed");
            match create_instance(&name, false) {
                Ok(next) => pending = next,
                Err(err) => {
                    tracing::warn!(target: "istmo.documents", %err, "creating a pipe instance failed");
                    break;
                }
            }
            thread::sleep(CONNECT_BACKOFF);
            continue;
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        // Keep one instance listening while this client is served, so
        // a launch in between never sees "no such pipe" and starts a
        // second primary.
        let next = match create_instance(&name, false) {
            Ok(next) => next,
            Err(err) => {
                tracing::warn!(target: "istmo.documents", %err, "creating a pipe instance failed");
                break;
            }
        };
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let mut client = File::from(std::mem::replace(&mut pending, next));
        match handle_client(&mut client, &runtime) {
            // Closing a pipe discards unread data: wait until the
            // client has read the acknowledgement.
            Ok(()) => drop(client.sync_all()),
            Err(err) => {
                tracing::warn!(target: "istmo.documents", %err, "rejected a forwarded launch");
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocking `ConnectNamedPipe`.
        drop(open_client(&self.path));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
