//! Unix domain socket endpoint guarded by an `flock`ed lock file.
//!
//! The accept loop polls a non-blocking listener, so stopping never
//! depends on reaching the socket through its path (which may have been
//! removed by a temp cleaner). Every client gets its own short-lived
//! thread with a deadline for the whole request; a stalled client never
//! delays the others. The loop also checks every few seconds that the
//! lock file and the socket on disk are still the ones this process
//! owns: a deleted socket is bound again, a deleted lock means another
//! instance may already have taken over, so this one stops serving
//! instead of competing.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use istmo_core::Runtime;

use super::{
    ACK_WAIT, CONNECT_ATTEMPTS, CONNECT_BACKOFF, DocumentsError, ForwardRequest, InstanceKey,
    MAX_CLIENTS, REQUEST_BUDGET, endpoint_stem, handle_client, send_request,
};

/// How often the accept loop wakes up when idle.
const POLL: Duration = Duration::from_millis(200);
/// How often the loop checks that its files are still on disk.
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// `S_ISVTX` on a file in `$XDG_RUNTIME_DIR` exempts it from the
/// periodic clean-up the XDG Base Directory spec allows.
const STICKY_PRIVATE: u32 = 0o1600;

/// Files of one instance key inside the private runtime directory.
#[derive(Debug, Clone)]
struct Paths {
    lock: PathBuf,
    socket: PathBuf,
}

impl Paths {
    fn resolve(key: &InstanceKey) -> Result<Self, DocumentsError> {
        let dir = private_dir().map_err(DocumentsError::io("preparing the runtime directory"))?;
        let stem = endpoint_stem(key);
        Ok(Self {
            lock: dir.join(format!("{stem}.lock")),
            socket: dir.join(format!("{stem}.sock")),
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

/// Device and inode: what a path points at, to tell our file from a
/// newer one with the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

impl From<&fs::Metadata> for FileId {
    fn from(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
}

impl FileId {
    fn at(path: &Path) -> Option<Self> {
        fs::symlink_metadata(path)
            .ok()
            .map(|meta| Self::from(&meta))
    }
}

/// Bind the socket (removing whatever holds its name: the caller owns
/// the lock, so it is stale) and restrict it to the user.
fn bind(path: &Path) -> Result<(UnixListener, FileId), DocumentsError> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(DocumentsError::io("removing a stale socket")(err)),
    }
    let listener = UnixListener::bind(path).map_err(DocumentsError::io("binding the socket"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(STICKY_PRIVATE))
        .map_err(DocumentsError::io("restricting the socket"))?;
    listener
        .set_nonblocking(true)
        .map_err(DocumentsError::io("configuring the socket"))?;
    let id = FileId::at(path).ok_or(DocumentsError::Protocol("the socket vanished after bind"))?;
    Ok((listener, id))
}

#[derive(Debug)]
pub(super) struct Endpoint {
    listener: UnixListener,
    socket_id: FileId,
    paths: Paths,
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
    // Keep a lock created before this version out of clean-ups too.
    let _ = fs::set_permissions(&paths.lock, fs::Permissions::from_mode(STICKY_PRIVATE));
    let (listener, socket_id) = bind(&paths.socket)?;
    Ok(Claim::Primary(Endpoint {
        listener,
        socket_id,
        paths,
        lock,
    }))
}

pub(super) fn forward(key: &InstanceKey, request: &ForwardRequest) -> Result<(), DocumentsError> {
    let paths = Paths::resolve(key)?;
    for _ in 0..CONNECT_ATTEMPTS {
        match UnixStream::connect(&paths.socket) {
            Ok(mut stream) => {
                stream
                    .set_write_timeout(Some(REQUEST_BUDGET))
                    .and_then(|()| stream.set_read_timeout(Some(ACK_WAIT)))
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

/// A connected client with one deadline for the whole exchange: every
/// read and write gets the time that is left, so a client sending a
/// byte at a time cannot hold its handler past [`REQUEST_BUDGET`].
struct Deadline {
    stream: UnixStream,
    until: Instant,
}

impl Deadline {
    fn remaining(&self) -> io::Result<Duration> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(io::Error::new(io::ErrorKind::TimedOut, "request deadline"))
        } else {
            Ok(left)
        }
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buf)
    }
}

impl Write for Deadline {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    alive: AtomicBool,
    clients: AtomicUsize,
}

#[derive(Debug)]
pub(super) struct Server {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Option<FileId>>>,
    socket: PathBuf,
    /// Dropped after the socket is removed, so the next primary never
    /// sees our socket without our lock.
    _lock: File,
}

impl Server {
    pub(super) fn spawn(
        endpoint: Endpoint,
        runtime: Weak<Runtime>,
    ) -> Result<Self, DocumentsError> {
        let Endpoint {
            listener,
            socket_id,
            paths,
            lock,
        } = endpoint;
        let lock_id = lock
            .metadata()
            .map(|meta| FileId::from(&meta))
            .map_err(DocumentsError::io("reading the instance lock"))?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            clients: AtomicUsize::new(0),
        });
        let socket = paths.socket.clone();
        let thread = thread::Builder::new()
            .name("istmo-documents".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    let mut state = Serving {
                        listener,
                        socket_id,
                        lock_id,
                        paths,
                        runtime,
                    };
                    let owned = state.run(&shared);
                    shared.alive.store(false, Ordering::Release);
                    owned
                }
            })
            .map_err(DocumentsError::io("spawning the documents thread"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
            socket,
            _lock: lock,
        })
    }

    pub(super) fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
    }
}

/// State of the accept loop.
struct Serving {
    listener: UnixListener,
    socket_id: FileId,
    lock_id: FileId,
    paths: Paths,
    runtime: Weak<Runtime>,
}

impl Serving {
    /// Accept until asked to stop. Returns the identity of the socket
    /// file this process still owns, if any, for removal.
    fn run(&mut self, shared: &Arc<Shared>) -> Option<FileId> {
        let mut last_check = Instant::now();
        while !shared.stop.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((stream, _)) => self.serve(stream, shared),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(err) => {
                    // EMFILE and friends: back off instead of spinning.
                    tracing::warn!(target: "istmo.documents", %err, "accept failed");
                    thread::sleep(POLL);
                }
            }
            if self.runtime.strong_count() == 0 {
                return Some(self.socket_id);
            }
            if last_check.elapsed() >= CHECK_EVERY {
                last_check = Instant::now();
                if !self.endpoint_still_ours() {
                    return None;
                }
            }
        }
        Some(self.socket_id)
    }

    /// `false` when another instance may own the name now.
    fn endpoint_still_ours(&mut self) -> bool {
        if FileId::at(&self.paths.lock) != Some(self.lock_id) {
            // A new lock file means a later launch could lock it and
            // become primary: two servers must not compete.
            tracing::error!(
                target: "istmo.documents",
                lock = %self.paths.lock.display(),
                "the instance lock was removed; no longer accepting forwarded launches"
            );
            return false;
        }
        if FileId::at(&self.paths.socket) != Some(self.socket_id) {
            tracing::warn!(target: "istmo.documents", "the socket was removed; binding it again");
            match bind(&self.paths.socket) {
                Ok((listener, id)) => {
                    self.listener = listener;
                    self.socket_id = id;
                }
                Err(err) => {
                    tracing::error!(target: "istmo.documents", %err, "binding the socket again failed");
                    return false;
                }
            }
        }
        true
    }

    fn serve(&self, stream: UnixStream, shared: &Arc<Shared>) {
        if shared.clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
            shared.clients.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(target: "istmo.documents", "too many forwarded launches at once; dropping one");
            return;
        }
        let Some(runtime) = self.runtime.upgrade() else {
            shared.clients.fetch_sub(1, Ordering::AcqRel);
            return;
        };
        let shared_client = Arc::clone(shared);
        let spawned = thread::Builder::new()
            .name("istmo-documents-client".to_owned())
            .spawn(move || {
                let mut client = Deadline {
                    stream,
                    until: Instant::now() + REQUEST_BUDGET,
                };
                let ready = client.stream.set_nonblocking(false);
                match ready.map_err(DocumentsError::io("configuring a client socket")) {
                    Ok(()) => {
                        if let Err(err) = handle_client(&mut client, &runtime) {
                            tracing::warn!(target: "istmo.documents", %err, "rejected a forwarded launch");
                        }
                    }
                    Err(err) => tracing::warn!(target: "istmo.documents", %err, "client socket"),
                }
                shared_client.clients.fetch_sub(1, Ordering::AcqRel);
            });
        if let Err(err) = spawned {
            shared.clients.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(target: "istmo.documents", %err, "spawning a client thread");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // The loop polls: it notices within `POLL`, no wake-up needed.
        let owned = self
            .thread
            .take()
            .and_then(|thread| thread.join().ok().flatten());
        // Only remove the socket if it is still ours: a later primary
        // may own the name after our lock was lost.
        if owned.is_some() && FileId::at(&self.socket) == owned {
            if let Err(err) = fs::remove_file(&self.socket) {
                tracing::debug!(target: "istmo.documents", %err, "removing the socket");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(name: &str) -> InstanceKey {
        InstanceKey::try_from(format!("istmo-test-{name}-{}", std::process::id()).as_str()).unwrap()
    }

    fn cleanup(key: &InstanceKey) {
        let paths = Paths::resolve(key).unwrap();
        let _ = fs::remove_file(paths.lock);
        let _ = fs::remove_file(paths.socket);
    }

    #[test]
    fn private_dir_is_private() {
        let dir = private_dir().unwrap();
        let meta = fs::metadata(&dir).unwrap();
        assert!(meta.is_dir());
        // SAFETY: see `private_dir`.
        assert_eq!(meta.uid(), unsafe { libc::getuid() });
    }

    #[test]
    fn socket_path_is_short() {
        let key = InstanceKey::try_from("x".repeat(64).as_str()).unwrap();
        let paths = Paths::resolve(&key).unwrap();
        let name = paths.socket.file_name().unwrap().len();
        assert_eq!(name, "istmo-0123456789abcdef.sock".len());
    }

    #[test]
    fn second_lock_is_refused() {
        let key = test_key("lock");
        let Claim::Primary(endpoint) = claim(&key).unwrap() else {
            panic!("first claim must win");
        };
        // A second descriptor of the same file conflicts with the first
        // even inside one process: `flock` locks are per open file.
        assert!(matches!(claim(&key).unwrap(), Claim::Secondary));
        drop(endpoint);
        cleanup(&key);
    }

    #[test]
    fn removed_socket_is_bound_again_and_shutdown_does_not_hang() {
        let key = test_key("rebind");
        let Claim::Primary(endpoint) = claim(&key).unwrap() else {
            panic!("first claim must win");
        };
        let runtime = istmo_core::Runtime::mock().runtime;
        let server = Server::spawn(endpoint, Arc::downgrade(&runtime)).unwrap();
        let paths = Paths::resolve(&key).unwrap();
        fs::remove_file(&paths.socket).unwrap();
        let rebound = Instant::now();
        while FileId::at(&paths.socket).is_none() {
            assert!(
                rebound.elapsed() < CHECK_EVERY * 3,
                "socket never came back"
            );
            thread::sleep(POLL);
        }
        let started = Instant::now();
        drop(server);
        assert!(
            started.elapsed() < POLL * 5,
            "shutdown took {:?}",
            started.elapsed()
        );
        assert!(
            FileId::at(&paths.socket).is_none(),
            "own socket removed on exit"
        );
        cleanup(&key);
    }

    #[test]
    fn removed_lock_stops_serving() {
        let key = test_key("lostlock");
        let Claim::Primary(endpoint) = claim(&key).unwrap() else {
            panic!("first claim must win");
        };
        let runtime = istmo_core::Runtime::mock().runtime;
        let server = Server::spawn(endpoint, Arc::downgrade(&runtime)).unwrap();
        let paths = Paths::resolve(&key).unwrap();
        fs::remove_file(&paths.lock).unwrap();
        let since = Instant::now();
        while server.is_alive() {
            assert!(
                since.elapsed() < CHECK_EVERY * 3,
                "kept serving without a lock"
            );
            thread::sleep(POLL);
        }
        drop(server);
        cleanup(&key);
    }
}
