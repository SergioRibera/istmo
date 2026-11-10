//! Unix domain socket endpoint guarded by an `flock`ed lock file.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};

use istmo_core::Runtime;

use super::{
    CONNECT_ATTEMPTS, CONNECT_BACKOFF, DocumentsError, ForwardRequest, IO_TIMEOUT, InstanceKey,
    handle_client, send_request,
};

/// Files of one instance key inside the private runtime directory.
#[derive(Debug)]
struct Paths {
    lock: PathBuf,
    socket: PathBuf,
}

impl Paths {
    fn resolve(key: &InstanceKey) -> Result<Self, DocumentsError> {
        let dir = private_dir().map_err(DocumentsError::io("preparing the runtime directory"))?;
        Ok(Self {
            lock: dir.join(format!("{}.lock", key.as_str())),
            socket: dir.join(format!("{}.sock", key.as_str())),
        })
    }
}

/// A directory only the current user can enter.
fn private_dir() -> io::Result<PathBuf> {
    // `$XDG_RUNTIME_DIR` is created `0700` and owned by the user
    // (XDG Base Directory spec); macOS gives every user a private
    // `$TMPDIR` under `/var/folders`.
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if dir.is_absolute() && dir.is_dir() {
            return Ok(dir);
        }
    }
    if cfg!(target_os = "macos") {
        return Ok(std::env::temp_dir());
    }
    // SAFETY: `getuid` takes no arguments, cannot fail and has no
    // side effects.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("istmo-{uid}"));
    match DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    // Someone else may have created the directory first (shared
    // `/tmp`): refuse anything that is not ours and private. A symlink
    // fails the `is_dir` check of `symlink_metadata`.
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory of this user", dir.display()),
        ));
    }
    Ok(dir)
}

/// Exclusive, non-blocking `flock`. Held for the life of the file
/// descriptor; the kernel releases it when the process dies.
fn try_lock(file: &File) -> io::Result<bool> {
    loop {
        // SAFETY: the descriptor is owned by `file`, which outlives the
        // call; `flock` only reads its arguments.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => {}
            _ => return Err(err),
        }
    }
}

#[derive(Debug)]
pub(super) struct Endpoint {
    listener: UnixListener,
    socket: PathBuf,
    lock: File,
}

#[derive(Debug)]
pub(super) enum Claim {
    Primary(Endpoint),
    Secondary,
}

pub(super) fn claim(key: &InstanceKey) -> Result<Claim, DocumentsError> {
    let paths = Paths::resolve(key)?;
    // std opens with `O_CLOEXEC`: child processes never inherit the lock.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)
        .map_err(DocumentsError::io("opening the instance lock"))?;
    if !try_lock(&lock).map_err(DocumentsError::io("locking the instance lock"))? {
        return Ok(Claim::Secondary);
    }
    // Holding the lock makes any socket file left behind stale.
    match fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(DocumentsError::io("removing a stale socket")(err)),
    }
    let listener =
        UnixListener::bind(&paths.socket).map_err(DocumentsError::io("binding the socket"))?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))
        .map_err(DocumentsError::io("restricting the socket"))?;
    Ok(Claim::Primary(Endpoint {
        listener,
        socket: paths.socket,
        lock,
    }))
}

pub(super) fn forward(key: &InstanceKey, request: &ForwardRequest) -> Result<(), DocumentsError> {
    let paths = Paths::resolve(key)?;
    for _ in 0..CONNECT_ATTEMPTS {
        match UnixStream::connect(&paths.socket) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(IO_TIMEOUT))
                    .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
                    .map_err(DocumentsError::io("configuring the socket"))?;
                return send_request(&mut stream, request);
            }
            // The primary holds the lock but is not listening yet.
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
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
    socket: PathBuf,
    thread: Option<JoinHandle<()>>,
    /// Dropped after the thread has joined and the socket is removed.
    _lock: File,
}

impl Server {
    pub(super) fn spawn(
        endpoint: Endpoint,
        runtime: Weak<Runtime>,
    ) -> Result<Self, DocumentsError> {
        let Endpoint {
            listener,
            socket,
            lock,
        } = endpoint;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("istmo-documents".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || serve(&listener, &stop, &runtime)
            })
            .map_err(DocumentsError::io("spawning the documents thread"))?;
        Ok(Self {
            stop,
            socket,
            thread: Some(thread),
            _lock: lock,
        })
    }
}

fn serve(listener: &UnixListener, stop: &AtomicBool, runtime: &Weak<Runtime>) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                // EMFILE and friends: back off instead of spinning.
                tracing::warn!(target: "istmo.documents", %err, "accept failed");
                thread::sleep(super::CONNECT_BACKOFF);
                continue;
            }
        };
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let configured = stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)));
        if let Err(err) = configured {
            tracing::warn!(target: "istmo.documents", %err, "configuring a client socket");
            continue;
        }
        if let Err(err) = handle_client(&mut stream, &runtime) {
            tracing::warn!(target: "istmo.documents", %err, "rejected a forwarded launch");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocking `accept`.
        drop(UnixStream::connect(&self.socket));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        remove_socket(&self.socket);
    }
}

fn remove_socket(path: &Path) {
    if let Err(err) = fs::remove_file(path) {
        if err.kind() != io::ErrorKind::NotFound {
            tracing::debug!(target: "istmo.documents", %err, "removing the socket");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_is_private() {
        let dir = private_dir().unwrap();
        let meta = fs::metadata(&dir).unwrap();
        assert!(meta.is_dir());
        // SAFETY: see `private_dir`.
        assert_eq!(meta.uid(), unsafe { libc::getuid() });
    }

    #[test]
    fn second_lock_is_refused() {
        let key = InstanceKey::try_from(format!("istmo-test-lock-{}", std::process::id()).as_str())
            .unwrap();
        let Claim::Primary(endpoint) = claim(&key).unwrap() else {
            panic!("first claim must win");
        };
        // A second descriptor of the same file conflicts with the first
        // even inside one process: `flock` locks are per open file.
        assert!(matches!(claim(&key).unwrap(), Claim::Secondary));
        drop(endpoint);
        let paths = Paths::resolve(&key).unwrap();
        let _ = fs::remove_file(paths.lock);
        let _ = fs::remove_file(paths.socket);
    }
}
