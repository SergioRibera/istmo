//! Per-user local endpoint: one owner per name, request/reply messages
//! from other processes of the same user.
//!
//! A name is claimed by at most one process at a time. The owner serves
//! requests with a handler; every other process that claims the same name
//! is told it is taken and can [`LocalEndpoint::request`] the owner
//! instead. Single-instance apps, a CLI talking to its running GUI, a
//! helper process handing results back: any two processes of one user on
//! one machine.
//!
//! ```no_run
//! use istmo_plugins::{EndpointClaim, EndpointName, LocalEndpoint};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = LocalEndpoint::from(EndpointName::try_from("rs.example.app.control")?);
//! match endpoint.claim()? {
//!     EndpointClaim::Owner(owned) => {
//!         // Keep the server alive for as long as requests are welcome.
//!         let _server = owned.serve(|request: Vec<u8>| {
//!             let mut reply = b"echo: ".to_vec();
//!             reply.extend_from_slice(&request);
//!             Some(reply)
//!         })?;
//!         // … run the app …
//!     }
//!     EndpointClaim::Taken => {
//!         let reply = endpoint.request(b"hello")?;
//!         assert_eq!(reply, b"echo: hello");
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Messages are opaque bytes; encode them with [`istmo_core::codec`] (or
//! anything else) on both ends.
//!
//! Endpoints:
//!
//! - **Linux / macOS** — a Unix domain socket guarded by an `flock`ed
//!   lock file in a directory only the user can enter
//!   (`$XDG_RUNTIME_DIR`, else the per-user temp dir, else a private
//!   `istmo-<uid>` directory under `/tmp`). The lock, not the socket,
//!   decides who owns the name, so two simultaneous claims cannot both
//!   win and a crashed owner leaves no stale owner behind (the kernel
//!   drops the lock with the process).
//! - **Windows** — a named pipe created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` (atomic ownership) and
//!   `PIPE_REJECT_REMOTE_CLIENTS`, with a DACL for the user and SYSTEM
//!   only; both ends check the other process runs as the same user.
//!
//! Both servers also check who connected (`SO_PEERCRED` / `getpeereid`,
//! the pipe client's token) before reading a request. Other users never
//! reach an endpoint; nothing here authenticates processes of the same
//! user, which can reach each other's endpoints by design.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

/// Frame header: magic, then the protocol version byte, then the body
/// length (`u32`, little endian).
const MAGIC: [u8; 4] = *b"ISTL";
const PROTOCOL_VERSION: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 1 + 4;
/// A claimed name can be owned while its server is not listening yet;
/// a request retries this long before giving up.
const CONNECT_ATTEMPTS: u32 = 20;
const CONNECT_BACKOFF: Duration = Duration::from_millis(100);
const MAX_NAME_LEN: usize = 64;

/// Name of a per-user endpoint.
///
/// 1–64 ASCII letters, digits, `.`, `-` and `_`, not starting with `.`.
/// A reverse-DNS name (`rs.example.app.control`) keeps apps apart: the
/// namespace is shared by every program of the user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointName(String);

impl EndpointName {
    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A name for istmo's own endpoints, outside the public namespace
    /// (`:` never passes validation), so an app using its bundle id as
    /// an endpoint name never meets them.
    pub(crate) fn reserved(scope: &str, name: &Self) -> Self {
        Self(format!("istmo.{scope}:{}", name.0))
    }

    /// Short, fixed-length stem for the OS objects of the name: Unix
    /// socket paths are limited to 104 bytes on macOS and app sandboxes
    /// put `$TMPDIR` deep.
    fn stem(&self) -> String {
        // FNV-1a: stable across builds and platforms, no dependency.
        let hash = self
            .0
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        format!("istmo-{hash:016x}")
    }
}

impl TryFrom<&str> for EndpointName {
    type Error = EndpointError;

    /// Accepts 1–64 ASCII letters, digits, `.`, `-` and `_` — safe as a
    /// file name and a pipe name on every desktop OS.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let valid = !raw.is_empty()
            && raw.len() <= MAX_NAME_LEN
            && !raw.starts_with('.')
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if valid {
            Ok(Self(raw.to_owned()))
        } else {
            Err(EndpointError::InvalidName(raw.to_owned()))
        }
    }
}

impl fmt::Display for EndpointName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A local endpoint name with the limits both ends apply.
///
/// Built from an [`EndpointName`]; the `with_*` methods change the
/// defaults. Owner and requesters should agree on
/// [`Self::with_max_message_len`]: each end refuses a bigger frame.
#[derive(Debug, Clone)]
pub struct LocalEndpoint {
    name: EndpointName,
    limits: Limits,
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    max_message_len: u32,
    max_clients: usize,
    request_budget: Duration,
    reply_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_len: 1 << 20,
            max_clients: 8,
            request_budget: Duration::from_millis(1500),
            reply_wait: Duration::from_secs(5),
        }
    }
}

impl From<EndpointName> for LocalEndpoint {
    fn from(name: EndpointName) -> Self {
        Self {
            name,
            limits: Limits::default(),
        }
    }
}

impl LocalEndpoint {
    /// Largest request or reply body, in bytes. Default 1 MiB.
    #[must_use]
    pub const fn with_max_message_len(mut self, len: u32) -> Self {
        self.limits.max_message_len = len;
        self
    }

    /// Requests the owner serves at once; more wait in the OS queue
    /// (Unix) or are turned away (Windows). Default 8, at least 1.
    #[must_use]
    pub fn with_max_clients(mut self, clients: usize) -> Self {
        self.limits.max_clients = clients.max(1);
        self
    }

    /// Total time the owner gives one requester to send its whole
    /// request: a deadline, not a per-read timeout, so a trickling
    /// client cannot hold a handler forever. Default 1.5 s. Unix only:
    /// synchronous Windows pipes have no read timeout.
    #[must_use]
    pub const fn with_request_budget(mut self, budget: Duration) -> Self {
        self.limits.request_budget = budget;
        self
    }

    /// How long [`Self::request`] waits for the reply once the request
    /// is sent; it covers the handler's run time. Default 5 s.
    #[must_use]
    pub const fn with_reply_wait(mut self, wait: Duration) -> Self {
        self.limits.reply_wait = wait;
        self
    }

    /// The endpoint's name.
    #[must_use]
    pub const fn name(&self) -> &EndpointName {
        &self.name
    }

    /// Try to own the name.
    ///
    /// # Errors
    ///
    /// [`EndpointError::Io`] when the OS objects of the endpoint cannot
    /// be created (no private runtime directory, pipe creation failed).
    pub fn claim(&self) -> Result<EndpointClaim, EndpointError> {
        Ok(match platform::claim(&self.name)? {
            platform::Claim::Owner(endpoint) => EndpointClaim::Owner(OwnedEndpoint {
                endpoint,
                limits: self.limits,
            }),
            platform::Claim::Taken => EndpointClaim::Taken,
        })
    }

    /// Send `body` to the owner of the name and return its reply.
    ///
    /// Retries for about two seconds while nobody listens (the owner
    /// may have claimed the name and not be serving yet).
    ///
    /// # Errors
    ///
    /// - [`EndpointError::NotListening`] — nobody served the name in time.
    /// - [`EndpointError::NoReply`] — the owner closed the connection or
    ///   did not answer within [`Self::with_reply_wait`].
    /// - [`EndpointError::ForeignOwner`] — the endpoint belongs to
    ///   another user.
    /// - [`EndpointError::TooLarge`] — `body` or the reply exceeds
    ///   [`Self::with_max_message_len`].
    /// - [`EndpointError::Io`] / [`EndpointError::Protocol`] — the
    ///   exchange failed.
    pub fn request(&self, body: &[u8]) -> Result<Vec<u8>, EndpointError> {
        check_len(body.len(), self.limits.max_message_len)?;
        platform::request(&self.name, &self.limits, body)
    }
}

/// Outcome of [`LocalEndpoint::claim`].
#[derive(Debug)]
pub enum EndpointClaim {
    /// This process owns the name until the value is dropped.
    Owner(OwnedEndpoint),
    /// Another process owns the name; [`LocalEndpoint::request`] reaches it.
    Taken,
}

/// Ownership of an endpoint name. Dropping it frees the name.
///
/// Holding it without serving is a plain single-instance lock: requests
/// to the name then time out instead of reaching anyone.
#[derive(Debug)]
pub struct OwnedEndpoint {
    endpoint: platform::Endpoint,
    limits: Limits,
}

/// What a server does with one request. `None` closes the connection
/// without a reply; the requester gets [`EndpointError::NoReply`].
type Handler = Arc<dyn Fn(Vec<u8>) -> Option<Vec<u8>> + Send + Sync>;

impl OwnedEndpoint {
    /// Serve requests on background threads until the returned server
    /// is dropped.
    ///
    /// `handler` runs on a thread of its own per request (at most
    /// [`LocalEndpoint::with_max_clients`] at once) and returns the reply
    /// body, or `None` to close the connection without one. It must
    /// return within the requester's reply wait.
    ///
    /// # Errors
    ///
    /// [`EndpointError::Io`] when the server thread cannot be spawned.
    pub fn serve(
        self,
        handler: impl Fn(Vec<u8>) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Result<LocalServer, EndpointError> {
        let server = platform::Server::spawn(self.endpoint, self.limits, Arc::new(handler))?;
        Ok(LocalServer(server))
    }
}

/// A running endpoint server. Dropping it stops serving and frees the
/// name.
#[derive(Debug)]
pub struct LocalServer(platform::Server);

impl LocalServer {
    /// Whether the server still accepts requests. Turns `false` when the
    /// endpoint is lost (its lock file removed, the pipe failed); the
    /// name may belong to another process by then.
    #[must_use]
    pub fn is_serving(&self) -> bool {
        self.0.is_alive()
    }
}

/// Failure of a local endpoint.
#[derive(Debug)]
#[non_exhaustive]
pub enum EndpointError {
    /// The name is not a valid [`EndpointName`].
    InvalidName(String),
    /// An I/O step of the endpoint failed.
    Io {
        /// What was being done.
        op: &'static str,
        /// The OS error.
        source: io::Error,
    },
    /// The name is owned, but nobody accepted the request in time.
    NotListening,
    /// The owner closed the connection or did not reply in time.
    NoReply,
    /// The endpoint is served by another user's process.
    ForeignOwner,
    /// A request or reply body exceeds the endpoint's message limit.
    TooLarge {
        /// Size of the body.
        len: usize,
        /// The limit.
        max: u32,
    },
    /// The peer broke the framing protocol.
    Protocol(&'static str),
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(
                f,
                "invalid endpoint name `{name}` (expected 1-64 ASCII letters, digits, `.`, `-` or `_`)"
            ),
            Self::Io { op, source } => write!(f, "{op}: {source}"),
            Self::NotListening => f.write_str("the endpoint owner is not listening"),
            Self::NoReply => f.write_str("the endpoint owner did not reply"),
            Self::ForeignOwner => f.write_str("the endpoint belongs to another user"),
            Self::TooLarge { len, max } => {
                write!(f, "message of {len} bytes exceeds the {max}-byte limit")
            }
            Self::Protocol(what) => write!(f, "local endpoint protocol: {what}"),
        }
    }
}

impl std::error::Error for EndpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidName(_)
            | Self::NotListening
            | Self::NoReply
            | Self::ForeignOwner
            | Self::TooLarge { .. }
            | Self::Protocol(_) => None,
        }
    }
}

impl EndpointError {
    const fn io(op: &'static str) -> impl FnOnce(io::Error) -> Self {
        move |source| Self::Io { op, source }
    }

    /// A read that ran out of time or hit the end of the stream: the peer
    /// went away or stalled.
    fn read(op: &'static str) -> impl FnOnce(io::Error) -> Self {
        move |source| match source.kind() {
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset => Self::NoReply,
            _ => Self::Io { op, source },
        }
    }
}

fn check_len(len: usize, max: u32) -> Result<u32, EndpointError> {
    u32::try_from(len)
        .ok()
        .filter(|&fits| fits <= max)
        .ok_or(EndpointError::TooLarge { len, max })
}

/// Write one frame: header, then `body`.
fn write_frame(stream: &mut impl Write, body: &[u8], max: u32) -> Result<(), EndpointError> {
    let len = check_len(body.len(), max)?;
    let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
    frame.extend_from_slice(&MAGIC);
    frame.push(PROTOCOL_VERSION);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(body);
    stream
        .write_all(&frame)
        .and_then(|()| stream.flush())
        .map_err(EndpointError::io("sending a message"))
}

/// Read one frame and return its body. A length over `max` is refused
/// before anything is allocated for it.
fn read_frame(stream: &mut impl Read, max: u32) -> Result<Vec<u8>, EndpointError> {
    let mut header = [0_u8; HEADER_LEN];
    stream
        .read_exact(&mut header)
        .map_err(EndpointError::read("reading a message"))?;
    let (magic, rest) = header.split_at(MAGIC.len());
    let (&[version], len) = rest.split_at(1) else {
        return Err(EndpointError::Protocol("short header"));
    };
    if magic != MAGIC {
        return Err(EndpointError::Protocol("bad magic"));
    }
    if version != PROTOCOL_VERSION {
        return Err(EndpointError::Protocol("unsupported protocol version"));
    }
    let len = u32::from_le_bytes(
        len.try_into()
            .map_err(|_| EndpointError::Protocol("short header"))?,
    );
    if len > max {
        return Err(EndpointError::TooLarge {
            len: len as usize,
            max,
        });
    }
    let mut body = vec![0_u8; len as usize];
    stream
        .read_exact(&mut body)
        .map_err(EndpointError::read("reading a message"))?;
    Ok(body)
}

/// Serve one connection: read the request, run the handler, reply.
/// `before_reply` runs between the handler and the reply (the Unix
/// server re-arms its deadline there).
fn handle_client<S: Read + Write>(
    stream: &mut S,
    handler: &Handler,
    limits: &Limits,
    before_reply: impl FnOnce(&mut S),
) -> Result<(), EndpointError> {
    let request = read_frame(stream, limits.max_message_len)?;
    let Some(reply) = handler(request) else {
        return Ok(());
    };
    before_reply(stream);
    write_frame(stream, &reply, limits.max_message_len)
}

/// Requester side of [`handle_client`].
fn exchange(
    stream: &mut (impl Read + Write),
    body: &[u8],
    limits: &Limits,
) -> Result<Vec<u8>, EndpointError> {
    write_frame(stream, body, limits.max_message_len)?;
    read_frame(stream, limits.max_message_len)
}

/// The handler slots of a server.
#[derive(Debug)]
struct ClientSlots {
    busy: Arc<AtomicUsize>,
    max: usize,
}

impl ClientSlots {
    fn new(max: usize) -> Self {
        Self {
            busy: Arc::default(),
            max,
        }
    }

    /// Whether every slot is busy right now.
    #[cfg(unix)]
    fn is_full(&self) -> bool {
        self.busy.load(Ordering::Acquire) >= self.max
    }

    /// A slot, or `None` when every one is busy.
    fn take(&self) -> Option<ClientSlot> {
        let mut busy = self.busy.load(Ordering::Acquire);
        while busy < self.max {
            match self.busy.compare_exchange_weak(
                busy,
                busy + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(ClientSlot(Arc::clone(&self.busy))),
                Err(now) => busy = now,
            }
        }
        None
    }
}

/// One busy handler slot, given back on drop — also when the handler
/// panics, so a crashing request cannot lock the server out.
#[derive(Debug)]
struct ClientSlot(Arc<AtomicUsize>);

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const MAX: u32 = 1 << 20;

    #[test]
    fn name_validation() {
        assert!(EndpointName::try_from("rs.example.app").is_ok());
        assert!(EndpointName::try_from("my_app-2").is_ok());
        for bad in ["", ".hidden", "a/b", "a b", "a\\b", "a:b", &"x".repeat(65)] {
            assert!(EndpointName::try_from(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn reserved_names_differ_from_public_ones() {
        let public = EndpointName::try_from("rs.example.app").unwrap();
        let reserved = EndpointName::reserved("documents", &public);
        assert_ne!(public.stem(), reserved.stem());
        assert!(EndpointName::try_from(reserved.as_str()).is_err());
    }

    #[test]
    fn frame_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"payload", MAX).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(buf), MAX).unwrap(), b"payload");
        let mut empty = Vec::new();
        write_frame(&mut empty, &[], MAX).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(empty), MAX).unwrap(), b"");
    }

    #[test]
    fn rejects_bad_frames() {
        let bad_magic = b"NOPE\x01\x00\x00\x00\x00".to_vec();
        assert!(matches!(
            read_frame(&mut Cursor::new(bad_magic), MAX),
            Err(EndpointError::Protocol("bad magic"))
        ));
        let huge = [
            MAGIC.as_slice(),
            &[PROTOCOL_VERSION],
            &u32::MAX.to_le_bytes(),
        ]
        .concat();
        assert!(matches!(
            read_frame(&mut Cursor::new(huge), MAX),
            Err(EndpointError::TooLarge { .. })
        ));
        let other_version = [MAGIC.as_slice(), &[0], &0_u32.to_le_bytes()].concat();
        assert!(matches!(
            read_frame(&mut Cursor::new(other_version), MAX),
            Err(EndpointError::Protocol("unsupported protocol version"))
        ));
        let truncated = [
            MAGIC.as_slice(),
            &[PROTOCOL_VERSION],
            &8_u32.to_le_bytes(),
            b"abc",
        ]
        .concat();
        assert!(matches!(
            read_frame(&mut Cursor::new(truncated), MAX),
            Err(EndpointError::NoReply)
        ));
        assert!(matches!(
            write_frame(&mut Vec::new(), &[0; 9], 8),
            Err(EndpointError::TooLarge { len: 9, max: 8 })
        ));
    }

    #[test]
    fn client_slots_are_bounded_and_given_back() {
        let slots = ClientSlots::new(3);
        let taken: Vec<_> = (0..3).filter_map(|_| slots.take()).collect();
        assert_eq!(taken.len(), 3);
        assert!(slots.take().is_none());
        drop(taken);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _slot = slots.take().unwrap();
            panic!("handler crashed");
        }));
        assert!(panicked.is_err());
        assert_eq!((0..3).filter_map(|_| slots.take()).count(), 3);
    }

    fn test_endpoint(name: &str) -> LocalEndpoint {
        let name = format!("istmo-test-endpoint-{name}-{}", std::process::id());
        LocalEndpoint::from(EndpointName::try_from(name.as_str()).unwrap())
    }

    #[test]
    fn owner_answers_requests() {
        let endpoint = test_endpoint("echo");
        let EndpointClaim::Owner(owned) = endpoint.claim().unwrap() else {
            panic!("first claim must win");
        };
        assert!(matches!(endpoint.claim().unwrap(), EndpointClaim::Taken));
        let server = owned
            .serve(|request| (request != b"drop").then(|| [b"re: ".as_slice(), &request].concat()))
            .unwrap();
        assert!(server.is_serving());
        assert_eq!(endpoint.request(b"one").unwrap(), b"re: one");
        assert_eq!(endpoint.request(&[]).unwrap(), b"re: ");
        assert!(matches!(
            endpoint.request(b"drop"),
            Err(EndpointError::NoReply)
        ));

        // Once the owner is gone the name can be claimed again. Windows
        // does not join the pipe thread on drop: give it a moment.
        drop(server);
        let reclaimed = (0..20).any(|_| {
            let claimed = matches!(endpoint.claim().unwrap(), EndpointClaim::Owner(_));
            if !claimed {
                std::thread::sleep(Duration::from_millis(50));
            }
            claimed
        });
        assert!(
            reclaimed,
            "the name stayed taken after the server was dropped"
        );
    }

    #[test]
    fn oversized_messages_are_refused() {
        let endpoint = test_endpoint("limits").with_max_message_len(4);
        let EndpointClaim::Owner(owned) = endpoint.claim().unwrap() else {
            panic!("first claim must win");
        };
        let _server = owned.serve(|_| Some(b"too long".to_vec())).unwrap();
        assert!(matches!(
            endpoint.request(b"12345"),
            Err(EndpointError::TooLarge { len: 5, max: 4 })
        ));
        // The handler's reply breaks the limit: the server drops it.
        assert!(matches!(
            endpoint.request(b"ok"),
            Err(EndpointError::NoReply)
        ));
    }
}
