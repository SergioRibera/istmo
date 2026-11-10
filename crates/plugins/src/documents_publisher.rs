//! Desktop delivery for [`crate::documents`]: command-line documents
//! and single-instance forwarding.
//!
//! The first process of an app becomes the **primary**: it publishes
//! the documents of its own command line and listens on a per-user
//! local endpoint. A later launch of the same app (double-click on a
//! file, "Open with") finds that endpoint, forwards its documents and
//! the launcher's activation token, waits for the acknowledgement and
//! returns [`Startup::Forwarded`] so the caller exits. The primary
//! publishes forwarded documents with [`DocumentOrigin::Running`]; the
//! app decides where they show up (a new window, never the current
//! one, is the intended UX).
//!
//! Endpoints:
//!
//! - **Linux / macOS** — a Unix domain socket guarded by an `flock`ed
//!   lock file in a directory only the user can enter
//!   (`$XDG_RUNTIME_DIR`, else the per-user temp dir, else a private
//!   `istmo-<uid>` directory under `/tmp`). The lock, not the socket,
//!   decides who is primary, so two simultaneous launches cannot both
//!   win and a crashed primary leaves no stale owner behind (the kernel
//!   drops the lock with the process).
//! - **Windows** — a named pipe created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` (atomic ownership) and
//!   `PIPE_REJECT_REMOTE_CLIENTS`, with a DACL for the user and SYSTEM
//!   only; both ends check the other process runs as the same user.
//!
//! Both servers also check who connected (`SO_PEERCRED` / `getpeereid`,
//! the pipe client's token) before reading a request.
//!
//! Failures never stop the app from starting: when the endpoint cannot
//! be created or reached the process runs as a primary without
//! forwarding and logs why.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use istmo_core::{CodecError, IstmoError, Runtime, codec};
use istmo_macros::message;

use crate::documents::{DocumentOrigin, Documents, OpenedDocument};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

/// Frame header: magic, then the protocol version byte.
const MAGIC: [u8; 4] = *b"ISTD";
const PROTOCOL_VERSION: u8 = 2;
/// Upper bound of a forwarded request; a command line never gets close.
const MAX_FRAME_LEN: u32 = 1 << 20;
const ACK: [u8; 2] = *b"OK";
/// Total time the primary gives one client to send its whole request.
/// A deadline, not a per-read timeout, so a trickling client cannot hold
/// a handler forever.
/// Unix only: synchronous Windows pipes have no read timeout (see
/// `windows.rs`).
#[cfg(unix)]
const REQUEST_BUDGET: Duration = Duration::from_millis(1500);
/// How long a second launch waits for the acknowledgement. Longer than
/// [`REQUEST_BUDGET`] so a slow but healthy primary is not mistaken for a
/// dead one (which would open the documents twice).
const ACK_WAIT: Duration = Duration::from_secs(5);
/// Forwarded launches served at once; more are turned away.
const MAX_CLIENTS: usize = 8;
/// Rounds of "claim, else forward" before giving up on single instance:
/// a forward that fails because the primary was quitting lets the next
/// round claim the freed lock instead of starting a second instance.
const CLAIM_ROUNDS: u32 = 3;
/// A second launch can start while the primary holds the lock but is
/// not listening yet; retry this long before giving up.
const CONNECT_ATTEMPTS: u32 = 20;
const CONNECT_BACKOFF: Duration = Duration::from_millis(100);
const MAX_KEY_LEN: usize = 64;

/// Short, fixed-length endpoint name for a key: Unix socket paths are
/// limited to 104 bytes on macOS and app sandboxes put `$TMPDIR` deep.
fn endpoint_stem(key: &InstanceKey) -> String {
    // FNV-1a: stable across builds and platforms, no dependency.
    let hash = key
        .as_str()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("istmo-{hash:016x}")
}

/// Name of the per-user endpoint, derived from the app's bundle id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceKey(String);

impl InstanceKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for InstanceKey {
    type Error = DocumentsError;

    /// Accepts 1–64 ASCII letters, digits, `.`, `-` and `_` — safe as a
    /// file name and a pipe name on every desktop OS.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let valid = !raw.is_empty()
            && raw.len() <= MAX_KEY_LEN
            && !raw.starts_with('.')
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if valid {
            Ok(Self(raw.to_owned()))
        } else {
            Err(DocumentsError::InvalidKey(raw.to_owned()))
        }
    }
}

/// Build-time single-instance setting of the app crate.
///
/// `istmo-build` emits `ISTMO_APP_BUNDLE_ID` and `ISTMO_SINGLE_INSTANCE`
/// for the app crate; read them there with `option_env!` (or the
/// `istmo::single_instance!()` macro) and pass them in.
#[must_use]
pub fn instance_key_from_build(
    bundle_id: Option<&str>,
    single_instance: Option<&str>,
) -> Option<InstanceKey> {
    if single_instance != Some("1") {
        return None;
    }
    let bundle_id = bundle_id?;
    match InstanceKey::try_from(bundle_id) {
        Ok(key) => Some(key),
        Err(err) => {
            tracing::warn!(target: "istmo.documents", %err, "single instance disabled");
            None
        }
    }
}

#[derive(Debug)]
pub enum DocumentsError {
    InvalidKey(String),
    Io { op: &'static str, source: io::Error },
    Codec(CodecError),
    Runtime(IstmoError),
    Protocol(&'static str),
}

impl fmt::Display for DocumentsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey(key) => write!(
                f,
                "invalid instance key `{key}` (expected 1-64 ASCII letters, digits, `.`, `-` or `_`)"
            ),
            Self::Io { op, source } => write!(f, "{op}: {source}"),
            Self::Codec(err) => write!(f, "forwarded request codec: {err}"),
            Self::Runtime(err) => write!(f, "documents plugin: {err}"),
            Self::Protocol(what) => write!(f, "single-instance protocol: {what}"),
        }
    }
}

impl std::error::Error for DocumentsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Codec(err) => Some(err),
            Self::Runtime(err) => Some(err),
            Self::InvalidKey(_) | Self::Protocol(_) => None,
        }
    }
}

impl DocumentsError {
    const fn io(op: &'static str) -> impl FnOnce(io::Error) -> Self {
        move |source| Self::Io { op, source }
    }
}

/// The [`MAX_CLIENTS`] handler slots of a server.
#[derive(Debug, Default)]
struct ClientSlots(Arc<std::sync::atomic::AtomicUsize>);

impl ClientSlots {
    /// Whether every slot is busy right now.
    #[cfg(unix)]
    fn is_full(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire) >= MAX_CLIENTS
    }

    /// A slot, or `None` when every one is busy.
    fn take(&self) -> Option<ClientSlot> {
        use std::sync::atomic::Ordering;
        let mut busy = self.0.load(Ordering::Acquire);
        while busy < MAX_CLIENTS {
            match self
                .0
                .compare_exchange_weak(busy, busy + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some(ClientSlot(Arc::clone(&self.0))),
                Err(now) => busy = now,
            }
        }
        None
    }
}

/// One busy handler slot, given back on drop — also when the handler
/// panics, so a crashing request cannot lock forwarding out.
#[derive(Debug)]
struct ClientSlot(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// What a second launch sends to the primary.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForwardRequest {
    /// Random per launch. A launch that gives up waiting re-sends the
    /// same request; a primary that was only slow (stopped, swapping)
    /// then reads every copy, and the nonce lets it open the files once.
    nonce: u64,
    paths: Vec<String>,
    activation_token: Option<String>,
}

/// A per-process random value without a dependency: std seeds every
/// `RandomState` from the OS.
fn random_nonce() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::hash::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.finish()
}

/// Nonces of the requests a primary served lately.
#[derive(Debug, Default)]
struct RecentNonces(std::sync::Mutex<std::collections::VecDeque<u64>>);

impl RecentNonces {
    const KEEP: usize = 64;

    /// `true` the first time `nonce` is seen.
    fn first_time(&self, nonce: u64) -> bool {
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.contains(&nonce) {
            return false;
        }
        if seen.len() == Self::KEEP {
            seen.pop_front();
        }
        seen.push_back(nonce);
        true
    }
}

impl ForwardRequest {
    fn write_to(&self, stream: &mut impl Write) -> Result<(), DocumentsError> {
        let body = codec::encode(self).map_err(DocumentsError::Codec)?;
        let len = u32::try_from(body.len())
            .ok()
            .filter(|&len| len <= MAX_FRAME_LEN)
            .ok_or(DocumentsError::Protocol("request too large"))?;
        let mut frame = Vec::with_capacity(MAGIC.len() + 5 + body.len());
        frame.extend_from_slice(&MAGIC);
        frame.push(PROTOCOL_VERSION);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&body);
        stream
            .write_all(&frame)
            .and_then(|()| stream.flush())
            .map_err(DocumentsError::io("sending the forwarded documents"))
    }

    fn read_from(stream: &mut impl Read) -> Result<Self, DocumentsError> {
        let mut header = [0_u8; 9];
        stream
            .read_exact(&mut header)
            .map_err(DocumentsError::io("reading a forwarded request"))?;
        if header[..4] != MAGIC {
            return Err(DocumentsError::Protocol("bad magic"));
        }
        if header[4] != PROTOCOL_VERSION {
            return Err(DocumentsError::Protocol("unsupported protocol version"));
        }
        let len = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
        if len > MAX_FRAME_LEN {
            return Err(DocumentsError::Protocol("request too large"));
        }
        let mut body = vec![0_u8; len as usize];
        stream
            .read_exact(&mut body)
            .map_err(DocumentsError::io("reading a forwarded request"))?;
        // The frame cap bounds the bytes, not what bincode allocates for a
        // length prefix inside them: a 9-byte body may claim a terabyte
        // Vec. The decode limit caps every claimed allocation as well.
        let config = codec::wire_config().with_limit::<{ MAX_FRAME_LEN as usize }>();
        let (request, read): (Self, usize) = bincode::decode_from_slice(&body, config)
            .map_err(|err| DocumentsError::Codec(CodecError::from(err)))?;
        if read != body.len() {
            return Err(DocumentsError::Protocol("trailing bytes"));
        }
        Ok(request)
    }
}

/// Outcome of [`DocumentsPublisher::start`].
#[derive(Debug)]
pub enum Startup {
    /// This process owns the app. Keep the publisher alive for as long
    /// as the app runs; dropping it stops accepting forwarded launches.
    Primary(DocumentsPublisher),
    /// A running instance took the documents. Exit now.
    Forwarded,
}

/// Owns the single-instance endpoint of the primary process.
#[derive(Debug)]
pub struct DocumentsPublisher {
    server: Option<platform::Server>,
}

impl DocumentsPublisher {
    /// Publish the documents named on the command line and, when `key`
    /// is set, either forward them to a running instance or start
    /// serving later launches.
    ///
    /// `args` excludes the program name (`std::env::args_os().skip(1)`).
    /// Arguments starting with `-` are options and skipped, except
    /// after a literal `--`. Relative paths resolve against the current
    /// directory; paths are not required to exist (the app reports a
    /// missing file itself).
    pub fn start(
        runtime: &Arc<Runtime>,
        args: impl IntoIterator<Item = OsString>,
        key: Option<&InstanceKey>,
    ) -> Result<Startup, DocumentsError> {
        runtime
            .check_declared(crate::documents::DOCUMENTS_PLUGIN_ID)
            .map_err(DocumentsError::Runtime)?;
        let paths = match std::env::current_dir() {
            Ok(cwd) => document_paths(args, Some(&cwd)),
            Err(err) => {
                tracing::warn!(target: "istmo.documents", %err, "no working directory; relative paths are ignored");
                document_paths(args, None)
            }
        };
        let token = activation_token();
        let primary = |server| {
            publish_paths(runtime, &paths, DocumentOrigin::Launch, token.as_deref());
            Ok(Startup::Primary(Self { server }))
        };

        let Some(key) = key else {
            return primary(None);
        };
        let request = ForwardRequest {
            nonce: random_nonce(),
            paths: paths
                .iter()
                .filter_map(|path| {
                    let utf8 = path.to_str().map(str::to_owned);
                    if utf8.is_none() {
                        tracing::warn!(target: "istmo.documents", path = %path.display(), "not forwarding a non UTF-8 path");
                    }
                    utf8
                })
                .collect(),
            activation_token: token.clone(),
        };
        for round in 1..=CLAIM_ROUNDS {
            match platform::claim(key) {
                Ok(platform::Claim::Primary(endpoint)) => {
                    let server = platform::Server::spawn(endpoint, Arc::downgrade(runtime))
                        .map_err(|err| {
                            tracing::warn!(target: "istmo.documents", %err, "not accepting forwarded launches");
                        })
                        .ok();
                    return primary(server);
                }
                Ok(platform::Claim::Secondary) => match platform::forward(key, &request) {
                    Ok(()) => return Ok(Startup::Forwarded),
                    // The primary may be quitting: its lock frees up and
                    // the next round claims it.
                    Err(err) => {
                        tracing::warn!(target: "istmo.documents", %err, round, "the running instance did not answer");
                        std::thread::sleep(CONNECT_BACKOFF * round);
                    }
                },
                Err(err) => {
                    tracing::warn!(target: "istmo.documents", %err, "single instance unavailable");
                    return primary(None);
                }
            }
        }
        tracing::warn!(target: "istmo.documents", "starting another instance: the running one does not answer");
        primary(None)
    }

    /// Whether later launches are forwarded to this process right now.
    /// Turns `false` when the endpoint is lost (removed from disk,
    /// runtime gone).
    #[must_use]
    pub fn is_serving(&self) -> bool {
        self.server.as_ref().is_some_and(platform::Server::is_alive)
    }
}

/// Documents named on a command line, made absolute. Without a working
/// directory relative paths are dropped: resolving them in the primary
/// would open the wrong files.
fn document_paths(args: impl IntoIterator<Item = OsString>, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut options_done = false;
    let mut out = Vec::new();
    for arg in args {
        if !options_done {
            if arg == "--" {
                options_done = true;
                continue;
            }
            if arg.as_encoded_bytes().first() == Some(&b'-') {
                continue;
            }
        }
        if arg.is_empty() {
            continue;
        }
        let path = PathBuf::from(arg);
        if path.is_absolute() {
            out.push(path);
        } else if let Some(cwd) = cwd {
            out.push(cwd.join(path));
        }
    }
    out
}

/// Focus token the launcher handed to this process, if any.
fn activation_token() -> Option<String> {
    ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
}

fn publish_paths<'a>(
    runtime: &Runtime,
    paths: impl IntoIterator<Item = &'a PathBuf>,
    origin: DocumentOrigin,
    token: Option<&str>,
) {
    for path in paths {
        let Some(mut document) = OpenedDocument::from_path(path.clone(), origin) else {
            tracing::warn!(target: "istmo.documents", path = %path.display(), "skipping a non UTF-8 path");
            continue;
        };
        document.activation_token = token.map(str::to_owned);
        if let Err(err) = Documents::publish(runtime, &document) {
            tracing::warn!(target: "istmo.documents", %err, "publishing a document failed");
        }
    }
}

/// Serve one forwarded launch: read, publish, acknowledge.
fn handle_client(
    stream: &mut (impl Read + Write),
    runtime: &Runtime,
    seen: &RecentNonces,
) -> Result<(), DocumentsError> {
    let request = ForwardRequest::read_from(stream)?;
    let token = request.activation_token.as_deref();
    if !seen.first_time(request.nonce) {
        // A copy of a request already served: acknowledge only.
    } else if request.paths.is_empty() {
        Documents::publish_activation(runtime, token).map_err(DocumentsError::Runtime)?;
    } else {
        // A forwarded path is resolved by the sender; a relative one would
        // resolve against this process's directory and name another file.
        let paths: Vec<PathBuf> = request
            .paths
            .into_iter()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .collect();
        publish_paths(runtime, &paths, DocumentOrigin::Running, token);
    }
    stream
        .write_all(&ACK)
        .and_then(|()| stream.flush())
        .map_err(DocumentsError::io("acknowledging a forwarded request"))
}

/// Client side of [`handle_client`].
fn send_request(
    stream: &mut (impl Read + Write),
    request: &ForwardRequest,
) -> Result<(), DocumentsError> {
    request.write_to(stream)?;
    let mut ack = [0_u8; ACK.len()];
    stream
        .read_exact(&mut ack)
        .map_err(DocumentsError::io("waiting for the running instance"))?;
    if ack == ACK {
        Ok(())
    } else {
        Err(DocumentsError::Protocol("unexpected acknowledgement"))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn instance_key_validation() {
        assert!(InstanceKey::try_from("rs.example.app").is_ok());
        assert!(InstanceKey::try_from("my_app-2").is_ok());
        for bad in ["", ".hidden", "a/b", "a b", "a\\b", &"x".repeat(65)] {
            assert!(InstanceKey::try_from(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn build_env_gates_single_instance() {
        assert!(instance_key_from_build(Some("rs.example.app"), Some("1")).is_some());
        assert!(instance_key_from_build(Some("rs.example.app"), Some("0")).is_none());
        assert!(instance_key_from_build(Some("rs.example.app"), None).is_none());
        assert!(instance_key_from_build(None, Some("1")).is_none());
    }

    #[test]
    fn command_line_paths() {
        let cwd = Path::new("/work");
        let args = ["--flag", "a.pdf", "/abs/b.md", "--", "-dash.txt", ""].map(OsString::from);
        assert_eq!(
            document_paths(args.clone(), Some(cwd)),
            [
                PathBuf::from("/work/a.pdf"),
                PathBuf::from("/abs/b.md"),
                PathBuf::from("/work/-dash.txt"),
            ]
        );
        assert_eq!(
            document_paths(args, None),
            [PathBuf::from("/abs/b.md")],
            "relative paths need a working directory"
        );
    }

    #[test]
    fn request_round_trip() {
        let request = ForwardRequest {
            nonce: 7,
            paths: vec!["/a.pdf".to_owned(), "/b c.md".to_owned()],
            activation_token: Some("token".to_owned()),
        };
        let mut buf = Vec::new();
        request.write_to(&mut buf).unwrap();
        assert_eq!(
            ForwardRequest::read_from(&mut Cursor::new(buf)).unwrap(),
            request
        );
    }

    #[test]
    fn rejects_bad_frames() {
        let mut bad_magic = b"NOPE\x01\x00\x00\x00\x00".to_vec();
        assert!(ForwardRequest::read_from(&mut Cursor::new(&mut bad_magic)).is_err());
        let huge = [
            MAGIC.as_slice(),
            &[PROTOCOL_VERSION],
            &u32::MAX.to_le_bytes(),
        ]
        .concat();
        assert!(matches!(
            ForwardRequest::read_from(&mut Cursor::new(huge)),
            Err(DocumentsError::Protocol("request too large"))
        ));
        let old = [MAGIC.as_slice(), &[0], &0_u32.to_le_bytes()].concat();
        assert!(ForwardRequest::read_from(&mut Cursor::new(old)).is_err());
        let truncated = [
            MAGIC.as_slice(),
            &[PROTOCOL_VERSION],
            &8_u32.to_le_bytes(),
            b"abc",
        ]
        .concat();
        assert!(ForwardRequest::read_from(&mut Cursor::new(truncated)).is_err());
    }

    #[test]
    fn huge_length_prefixes_are_rejected_without_allocating() {
        // `paths.len` claims 2^40 entries in a 9-byte body; a plain
        // decode would try to reserve terabytes and abort the process.
        let mut body = vec![0xFD];
        body.extend_from_slice(&(1_u64 << 40).to_le_bytes());
        let len = u32::try_from(body.len()).unwrap();
        let frame = [
            MAGIC.as_slice(),
            &[PROTOCOL_VERSION],
            &len.to_le_bytes(),
            &body,
        ]
        .concat();
        assert!(matches!(
            ForwardRequest::read_from(&mut Cursor::new(frame)),
            Err(DocumentsError::Codec(_))
        ));
    }

    #[test]
    fn repeated_requests_are_served_once() {
        let seen = RecentNonces::default();
        assert!(seen.first_time(1));
        assert!(!seen.first_time(1), "a re-sent copy");
        for nonce in 2..=RecentNonces::KEEP as u64 + 1 {
            assert!(seen.first_time(nonce));
        }
        assert!(seen.first_time(1), "forgotten after KEEP newer ones");
        assert_ne!(random_nonce(), random_nonce());
    }

    #[test]
    fn client_slots_are_bounded_and_given_back() {
        let slots = ClientSlots::default();
        let taken: Vec<_> = (0..MAX_CLIENTS).filter_map(|_| slots.take()).collect();
        assert_eq!(taken.len(), MAX_CLIENTS);
        assert!(slots.take().is_none());
        drop(taken);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _slot = slots.take().unwrap();
            panic!("handler crashed");
        }));
        assert!(panicked.is_err());
        assert_eq!(
            (0..MAX_CLIENTS).filter_map(|_| slots.take()).count(),
            MAX_CLIENTS
        );
    }

    /// Two launches in one process: the second finds the first's endpoint
    /// (its lock is a separate open file), forwards and is told to exit.
    #[test]
    fn second_launch_forwards_to_the_primary() {
        let rt = Runtime::mock().runtime;
        let documents = Documents::from_runtime(&rt).unwrap();
        let opened = documents.stream();
        let activations = documents.activations();
        let key =
            InstanceKey::try_from(format!("istmo-test-forward-{}", std::process::id()).as_str())
                .unwrap();

        let first =
            DocumentsPublisher::start(&rt, [OsString::from("/tmp/first.pdf")], Some(&key)).unwrap();
        let Startup::Primary(primary) = first else {
            panic!("first launch must be primary");
        };
        assert!(primary.is_serving());
        let launched = opened.try_recv().unwrap().unwrap();
        assert_eq!(launched.path(), Some(Path::new("/tmp/first.pdf")));
        assert_eq!(launched.origin, DocumentOrigin::Launch);

        let second = DocumentsPublisher::start(
            &rt,
            [
                OsString::from("--new-window"),
                OsString::from("/tmp/second.md"),
            ],
            Some(&key),
        )
        .unwrap();
        assert!(matches!(second, Startup::Forwarded));
        // The acknowledgement is sent after publishing.
        let forwarded = opened.try_recv().unwrap().unwrap();
        assert_eq!(forwarded.path(), Some(Path::new("/tmp/second.md")));
        assert_eq!(forwarded.origin, DocumentOrigin::Running);
        assert!(opened.try_recv().is_none());

        let empty = DocumentsPublisher::start(&rt, [], Some(&key)).unwrap();
        assert!(matches!(empty, Startup::Forwarded));
        assert!(activations.try_recv().unwrap().is_ok());
        assert!(opened.try_recv().is_none());

        // Once the primary is gone the next launch takes over.
        drop(primary);
        let next = DocumentsPublisher::start(&rt, [], Some(&key)).unwrap();
        let Startup::Primary(next) = next else {
            panic!("launch after the primary quit must be primary");
        };
        assert!(next.is_serving());
    }

    #[test]
    fn without_key_every_launch_is_primary() {
        let rt = Runtime::mock().runtime;
        let opened = Documents::from_runtime(&rt).unwrap().stream();
        let startup = DocumentsPublisher::start(&rt, [OsString::from("a.txt")], None).unwrap();
        let Startup::Primary(publisher) = startup else {
            panic!("no key means no forwarding");
        };
        assert!(!publisher.is_serving());
        let document = opened.try_recv().unwrap().unwrap();
        assert!(document.path().unwrap().is_absolute());
    }
}
