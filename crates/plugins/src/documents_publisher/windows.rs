//! Named-pipe endpoint.
//!
//! - **Ownership** is decided by `FILE_FLAG_FIRST_PIPE_INSTANCE`:
//!   creating the first instance of a name is atomic, so two
//!   simultaneous launches cannot both become primary.
//! - **Name** — `\\.\pipe\istmo-<hash>-<user SID>-<session>`: the pipe
//!   namespace is machine-wide; the SID (unlike `%USERNAME%`) is unique
//!   and cannot be spoofed by the environment, and the session keeps the
//!   same user's RDP and console desktops apart.
//! - **Trust** — the pipe gets an explicit DACL granting access only to
//!   the user and `SYSTEM` (the default one also lets everyone read,
//!   enough to occupy the server), `PIPE_REJECT_REMOTE_CLIENTS` keeps SMB
//!   clients out, and the server checks the connecting process runs as
//!   the user before reading. Another user could still create a pipe
//!   with our name first: a second launch opens it with
//!   `SECURITY_IDENTIFICATION` (the server cannot impersonate it) and
//!   checks the server process runs as the same user before it sends
//!   anything.
//! - **Liveness** — each client is served on its own thread, so the
//!   accept loop is back in `ConnectNamedPipe` right away and a stalled
//!   client only parks its own thread. Synchronous pipe reads have no
//!   timeout; such a thread stays parked until the client goes away.
//!   Shutdown never joins the loop: it flags it, nudges it with one
//!   connection and lets the thread end on its own, so quitting the app
//!   cannot hang on the pipe.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread;

use istmo_core::Runtime;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{
    ACK_WAIT, CONNECT_ATTEMPTS, CONNECT_BACKOFF, ClientSlots, DocumentsError, ForwardRequest,
    InstanceKey, RecentNonces, endpoint_stem, handle_client, send_request,
};

const PIPE_BUFFER: u32 = 64 * 1024;

/// Win32 error codes are `u32`; `io::Error` stores them as `i32`.
fn is_os_error(err: &io::Error, code: u32) -> bool {
    err.raw_os_error().and_then(|raw| u32::try_from(raw).ok()) == Some(code)
}

fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Take ownership of a handle a Win32 call returned, `Err` when it
/// reports failure (`null` or `INVALID_HANDLE_VALUE`).
fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is valid and was just returned to us, so
    // nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// String SID (`S-1-5-21-…`) of the user a process token belongs to.
fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a valid process handle with query access;
    // `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut len = 0_u32;
    // SAFETY: a null buffer with length 0 asks for the required size.
    let sized = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &raw mut len,
        )
    };
    if sized == 0 {
        let err = io::Error::last_os_error();
        if !is_os_error(&err, ERROR_INSUFFICIENT_BUFFER) {
            return Err(err);
        }
    }
    // `u64` storage keeps the `TOKEN_USER` header pointer-aligned.
    let mut buffer = vec![0_u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` writable bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            len,
            &raw mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a `TOKEN_USER` whose SID
    // points inside the same buffer, which outlives this read.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID; `text` receives a `LocalAlloc`ed,
    // NUL-terminated string that is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is NUL-terminated (see above), so every unit up to
    // the terminator is readable.
    let mut len = 0;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` UTF-16 units before the terminator are readable.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    // SAFETY: `text` came from `LocalAlloc` inside the conversion and is
    // not used afterwards.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

fn current_user_sid() -> io::Result<String> {
    // SAFETY: returns the pseudo-handle of the current process, which
    // needs no closing.
    token_user_sid(unsafe { GetCurrentProcess() })
}

/// Whether the process with `pid` runs as `sid`.
fn process_runs_as(pid: u32, sid: &str) -> io::Result<bool> {
    // SAFETY: plain call; the result is checked by `owned`.
    let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
    Ok(token_user_sid(process.as_raw_handle())? == sid)
}

/// Whether the process serving `pipe` (client end) runs as `sid`.
fn server_runs_as(pipe: &File, sid: &str) -> io::Result<bool> {
    let mut pid = 0_u32;
    // SAFETY: `pipe` is the client end of a named pipe; `pid` is a
    // valid out-pointer.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &raw mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    process_runs_as(pid, sid)
}

/// Whether the process connected to `pipe` (server end) runs as `sid`.
fn client_runs_as(pipe: &File, sid: &str) -> io::Result<bool> {
    let mut pid = 0_u32;
    // SAFETY: `pipe` is the server end of a connected named pipe; `pid`
    // is a valid out-pointer.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &raw mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    process_runs_as(pid, sid)
}

fn current_session() -> io::Result<u32> {
    let mut session = 0_u32;
    // SAFETY: `session` is a valid out-pointer; `GetCurrentProcessId`
    // cannot fail.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(session)
}

#[derive(Debug, Clone)]
struct PipeName {
    path: String,
    wide: Vec<u16>,
    sid: String,
}

impl PipeName {
    fn resolve(key: &InstanceKey) -> Result<Self, DocumentsError> {
        let sid = current_user_sid().map_err(DocumentsError::io("reading the user SID"))?;
        let session = current_session().map_err(DocumentsError::io("reading the session"))?;
        let path = format!(r"\\.\pipe\{}-{sid}-{session}", endpoint_stem(key));
        Ok(Self {
            wide: wide(&path),
            path,
            sid,
        })
    }
}

/// Security descriptor of the pipe: full access for the user and
/// `SYSTEM`, nothing for anyone else (`P` blocks inherited entries).
#[derive(Debug)]
struct PipeSecurity(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is an immutable `LocalAlloc` block owned only by
// this value; Win32 reads it from any thread.
unsafe impl Send for PipeSecurity {}

impl PipeSecurity {
    fn for_user(sid: &str) -> io::Result<Self> {
        let sddl = wide(&format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and outlives the call;
        // `descriptor` receives a `LocalAlloc`ed block freed in `Drop`.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: the block came from `LocalAlloc` in the conversion and
        // nothing references it after this value.
        unsafe { LocalFree(self.0) };
    }
}

/// Create one server instance of the pipe. `first` asks for exclusive
/// ownership of the name.
fn create_instance(name: &[u16], security: &PipeSecurity, first: bool) -> io::Result<OwnedHandle> {
    let open_mode = PIPE_ACCESS_DUPLEX
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let attributes = security.attributes();
    // SAFETY: `name` is a NUL-terminated UTF-16 string and `attributes`
    // points at a live descriptor; both outlive the call. The returned
    // handle is checked by `owned`.
    owned(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            &raw const attributes,
        )
    })
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
    name: PipeName,
    security: PipeSecurity,
    first: OwnedHandle,
}

#[derive(Debug)]
pub(super) enum Claim {
    Primary(Endpoint),
    Secondary,
}

pub(super) fn claim(key: &InstanceKey) -> Result<Claim, DocumentsError> {
    let name = PipeName::resolve(key)?;
    let security =
        PipeSecurity::for_user(&name.sid).map_err(DocumentsError::io("building the pipe DACL"))?;
    match create_instance(&name.wide, &security, true) {
        Ok(first) => Ok(Claim::Primary(Endpoint {
            name,
            security,
            first,
        })),
        Err(err) if is_os_error(&err, ERROR_ACCESS_DENIED) => Ok(Claim::Secondary),
        Err(err) => Err(DocumentsError::io("creating the pipe")(err)),
    }
}

/// Open the client end. `SECURITY_IDENTIFICATION` (std adds
/// `SECURITY_SQOS_PRESENT`) lets the server learn who we are but never
/// act as us, in case the pipe belongs to an impostor.
fn open_client(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(path)
}

pub(super) fn forward(key: &InstanceKey, request: &ForwardRequest) -> Result<(), DocumentsError> {
    let name = PipeName::resolve(key)?;
    let request = request.clone();
    // Synchronous pipe handles have no read timeout: run the exchange on
    // a helper thread and stop waiting if the primary hangs.
    let (tx, rx) = flume::bounded(1);
    thread::Builder::new()
        .name("istmo-documents-forward".to_owned())
        .spawn(move || {
            let _ = tx.send(exchange(&name, &request));
        })
        .map_err(DocumentsError::io("spawning the forward thread"))?;
    let deadline = ACK_WAIT + CONNECT_BACKOFF * CONNECT_ATTEMPTS;
    rx.recv_timeout(deadline)
        .unwrap_or(Err(DocumentsError::Protocol(
            "the running instance did not answer",
        )))
}

fn exchange(name: &PipeName, request: &ForwardRequest) -> Result<(), DocumentsError> {
    for _ in 0..CONNECT_ATTEMPTS {
        match open_client(&name.path) {
            Ok(mut pipe) => {
                let trusted = server_runs_as(&pipe, &name.sid)
                    .map_err(DocumentsError::io("checking who serves the pipe"))?;
                if !trusted {
                    return Err(DocumentsError::Protocol(
                        "the pipe is served by another user's process",
                    ));
                }
                return send_request(&mut pipe, request);
            }
            Err(err)
                if is_os_error(&err, ERROR_PIPE_BUSY)
                    || is_os_error(&err, ERROR_FILE_NOT_FOUND) =>
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
struct Shared {
    stop: AtomicBool,
    alive: AtomicBool,
    slots: ClientSlots,
    seen: RecentNonces,
}

#[derive(Debug)]
pub(super) struct Server {
    shared: Arc<Shared>,
    path: String,
}

impl Server {
    pub(super) fn spawn(
        endpoint: Endpoint,
        runtime: Weak<Runtime>,
    ) -> Result<Self, DocumentsError> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            slots: ClientSlots::default(),
            seen: RecentNonces::default(),
        });
        let path = endpoint.name.path.clone();
        thread::Builder::new()
            .name("istmo-documents".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    serve(endpoint, &shared, &runtime);
                    shared.alive.store(false, Ordering::Release);
                }
            })
            .map_err(DocumentsError::io("spawning the documents thread"))?;
        Ok(Self { shared, path })
    }

    pub(super) fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
    }
}

fn serve(endpoint: Endpoint, shared: &Arc<Shared>, runtime: &Weak<Runtime>) {
    let Endpoint {
        name,
        security,
        first,
    } = endpoint;
    let mut pending = first;
    while !shared.stop.load(Ordering::Acquire) {
        let connected = wait_for_client(&pending);
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        // Keep one instance listening while this client is served, so
        // a launch in between never sees "no such pipe" and starts a
        // second primary. The old instance closes only after the new
        // one exists.
        let next = match create_instance(&name.wide, &security, false) {
            Ok(next) => next,
            Err(err) => {
                tracing::warn!(target: "istmo.documents", %err, "creating a pipe instance failed");
                break;
            }
        };
        let client = std::mem::replace(&mut pending, next);
        match connected {
            Ok(()) => spawn_client(File::from(client), &name.sid, shared, runtime),
            Err(err) => {
                // e.g. `ERROR_NO_DATA`: the client left before we
                // accepted; its instance is dropped and replaced.
                tracing::debug!(target: "istmo.documents", %err, "waiting for a client failed");
                thread::sleep(CONNECT_BACKOFF);
            }
        }
        if runtime.strong_count() == 0 {
            break;
        }
    }
}

fn spawn_client(mut pipe: File, sid: &str, shared: &Arc<Shared>, runtime: &Weak<Runtime>) {
    let Some(slot) = shared.slots.take() else {
        tracing::warn!(target: "istmo.documents", "too many forwarded launches at once; dropping one");
        return;
    };
    let Some(runtime) = runtime.upgrade() else {
        return;
    };
    let sid = sid.to_owned();
    let shared_client = Arc::clone(shared);
    let spawned = thread::Builder::new()
        .name("istmo-documents-client".to_owned())
        .spawn(move || {
            let _slot = slot;
            // Quitting: no acknowledgement, so the launch retries and
            // takes over once this process lets go of the name.
            if shared_client.stop.load(Ordering::Acquire) {
                return;
            }
            // The DACL already limits who can connect; this keeps the
            // check next to the read it protects.
            match client_runs_as(&pipe, &sid) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(target: "istmo.documents", "refused a launch forwarded by another user");
                    return;
                }
                Err(err) => {
                    tracing::warn!(target: "istmo.documents", %err, "could not tell who connected");
                    return;
                }
            }
            match handle_client(&mut pipe, &runtime, &shared_client.seen) {
                // Closing a pipe discards unread data: wait until the
                // client has read the acknowledgement.
                Ok(()) => drop(pipe.sync_all()),
                Err(err) => {
                    tracing::warn!(target: "istmo.documents", %err, "rejected a forwarded launch");
                }
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(target: "istmo.documents", %err, "spawning a client thread");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Wake the blocking `ConnectNamedPipe` so the loop sees the flag
        // and closes its listening instance. Not joined: if the nudge
        // loses a race the thread ends with the process instead of
        // holding up quitting.
        drop(open_client(&self.path));
    }
}
