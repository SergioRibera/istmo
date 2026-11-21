//! Desktop delivery for [`crate::documents`] and [`crate::deeplinks`]:
//! command-line documents and URLs, and single-instance forwarding.
//!
//! The first process of an app becomes the **primary**: it publishes
//! the documents of its own command line and serves a per-user
//! [`LocalEndpoint`]. A later launch of the same app (double-click on a
//! file, "Open with") finds that endpoint taken, forwards its documents
//! and URLs and the launcher's activation token, waits for the acknowledgement
//! and returns [`Startup::Forwarded`] so the caller exits. The primary
//! publishes forwarded documents with [`DocumentOrigin::Running`]; the
//! app decides where they show up (a new window, never the current
//! one, is the intended UX).
//!
//! A command-line argument that starts with a URL scheme
//! (`notein://open/…`, `mailto:…`) is a link, not a file: the system
//! starts the registered handler with the URL as its argument. Links
//! go to the [`DeepLinks`] queue, in the primary as well.
//!
//! The endpoint name is derived from the app's [`EndpointName`] in a
//! namespace of its own, so the app can use the same name for a
//! [`LocalEndpoint`] of its own.
//!
//! Failures never stop the app from starting: when the endpoint cannot
//! be created or reached the process runs as a primary without
//! forwarding and logs why.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use istmo_core::{CodecError, IstmoError, Runtime, codec};
use istmo_macros::message;

use crate::deeplinks::{DeepLink, DeepLinks};
use crate::documents::{DocumentOrigin, Documents, OpenedDocument};
use crate::local_endpoint::{
    EndpointClaim, EndpointError, EndpointName, LocalEndpoint, LocalServer,
};

#[cfg(target_os = "macos")]
mod macos;

/// Upper bound of a forwarded request; a command line never gets close.
const MAX_REQUEST_LEN: u32 = 1 << 20;
/// Rounds of "claim, else forward" before giving up on single instance:
/// a forward that fails because the primary was quitting lets the next
/// round claim the freed name instead of starting a second instance.
const CLAIM_ROUNDS: u32 = 3;
const ROUND_BACKOFF: Duration = Duration::from_millis(100);

/// Build-time single-instance setting of the app crate.
///
/// `istmo-build` emits `ISTMO_APP_BUNDLE_ID` and `ISTMO_SINGLE_INSTANCE`
/// for the app crate; read them there with `option_env!` (or the
/// `istmo::single_instance!()` macro) and pass them in.
#[must_use]
pub fn instance_key_from_build(
    bundle_id: Option<&str>,
    single_instance: Option<&str>,
) -> Option<EndpointName> {
    if single_instance != Some("1") {
        return None;
    }
    let bundle_id = bundle_id?;
    match EndpointName::try_from(bundle_id) {
        Ok(name) => Some(name),
        Err(err) => {
            tracing::warn!(target: "istmo.documents", %err, "single instance disabled");
            None
        }
    }
}

/// Failure of the desktop single-instance documents channel.
#[derive(Debug)]
#[non_exhaustive]
pub enum DocumentsError {
    /// The single-instance endpoint failed.
    Endpoint(EndpointError),
    /// A forwarded message did not encode or decode.
    Codec(CodecError),
    /// The runtime refused the publish.
    Runtime(IstmoError),
    /// The running instance answered something else than an
    /// acknowledgement.
    UnexpectedReply,
}

impl fmt::Display for DocumentsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(err) => write!(f, "single-instance endpoint: {err}"),
            Self::Codec(err) => write!(f, "forwarded request codec: {err}"),
            Self::Runtime(err) => write!(f, "documents plugin: {err}"),
            Self::UnexpectedReply => f.write_str("the running instance did not acknowledge"),
        }
    }
}

impl std::error::Error for DocumentsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Endpoint(err) => Some(err),
            Self::Codec(err) => Some(err),
            Self::Runtime(err) => Some(err),
            Self::UnexpectedReply => None,
        }
    }
}

impl From<EndpointError> for DocumentsError {
    fn from(err: EndpointError) -> Self {
        Self::Endpoint(err)
    }
}

/// What a second launch sends to the primary. The primary answers with
/// an empty reply once the documents are published.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForwardRequest {
    /// Random per launch. A launch that gives up waiting re-sends the
    /// same request; a primary that was only slow (stopped, swapping)
    /// then reads every copy, and the nonce lets it open the files once.
    nonce: u64,
    paths: Vec<String>,
    /// URLs of the launch, as received.
    urls: Vec<String>,
    activation_token: Option<String>,
}

impl ForwardRequest {
    fn encode(&self) -> Result<Vec<u8>, DocumentsError> {
        codec::encode(self).map_err(DocumentsError::Codec)
    }

    fn decode(body: &[u8]) -> Result<Self, DocumentsError> {
        // The frame cap bounds the bytes, not what a length prefix inside
        // them may claim: the decode limit caps every claimed allocation too.
        codec::decode_exact_capped(body, MAX_REQUEST_LEN as usize).map_err(DocumentsError::Codec)
    }
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
        let mut seen = istmo_core::sync::lock(&self.0);
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
    server: Option<LocalServer>,
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
    /// missing file itself). An argument with a URL scheme is a link
    /// and goes to the [`DeepLinks`] queue.
    ///
    /// # Errors
    ///
    /// [`DocumentsError::Runtime`] when the runtime does not declare the
    /// documents plugin. Endpoint failures are logged and the process
    /// becomes the primary.
    pub fn start(
        runtime: &Arc<Runtime>,
        args: impl IntoIterator<Item = OsString>,
        key: Option<&EndpointName>,
    ) -> Result<Startup, DocumentsError> {
        runtime
            .check_declared(crate::documents::DOCUMENTS_PLUGIN_ID)
            .map_err(DocumentsError::Runtime)?;
        // Finder and `open` hand documents over as Apple Events, not argv.
        #[cfg(target_os = "macos")]
        macos::install(runtime);
        let LaunchArgs { paths, urls } = match std::env::current_dir() {
            Ok(cwd) => LaunchArgs::parse(args, Some(&cwd)),
            Err(err) => {
                tracing::warn!(target: "istmo.documents", %err, "no working directory; relative paths are ignored");
                LaunchArgs::parse(args, None)
            }
        };
        let token = activation_token();
        let primary = |server| {
            publish_paths(runtime, &paths, DocumentOrigin::Launch, token.as_deref());
            publish_urls(runtime, &urls, "launch");
            Ok(Startup::Primary(Self { server }))
        };

        let Some(key) = key else {
            return primary(None);
        };
        let endpoint = LocalEndpoint::from(EndpointName::reserved("documents", key))
            .with_max_message_len(MAX_REQUEST_LEN);
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
            urls: urls.clone(),
            activation_token: token.clone(),
        };
        let request = request.encode()?;
        for round in 1..=CLAIM_ROUNDS {
            match endpoint.claim() {
                Ok(EndpointClaim::Owner(owned)) => {
                    let server = owned
                        .serve(forwarded_launch_handler(Arc::downgrade(runtime)))
                        .map_err(|err| {
                            tracing::warn!(target: "istmo.documents", %err, "not accepting forwarded launches");
                        })
                        .ok();
                    return primary(server);
                }
                Ok(EndpointClaim::Taken) => match forward(&endpoint, &request) {
                    Ok(()) => return Ok(Startup::Forwarded),
                    // The primary may be quitting: its name frees up and
                    // the next round claims it.
                    Err(err) => {
                        tracing::warn!(target: "istmo.documents", %err, round, "the running instance did not answer");
                        std::thread::sleep(ROUND_BACKOFF * round);
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
    /// Turns `false` when the endpoint is lost (removed from disk).
    #[must_use]
    pub fn is_serving(&self) -> bool {
        self.server.as_ref().is_some_and(LocalServer::is_serving)
    }
}

/// Send a launch to the primary and wait for its acknowledgement.
fn forward(endpoint: &LocalEndpoint, request: &[u8]) -> Result<(), DocumentsError> {
    let reply = endpoint.request(request)?;
    if reply.is_empty() {
        Ok(())
    } else {
        Err(DocumentsError::UnexpectedReply)
    }
}

/// The primary's side of [`forward`]: publish, then acknowledge with an
/// empty reply. No reply when the runtime is gone, so the launch retries
/// and takes over once this process quits.
fn forwarded_launch_handler(
    runtime: Weak<Runtime>,
) -> impl Fn(Vec<u8>) -> Option<Vec<u8>> + Send + Sync + 'static {
    let seen = RecentNonces::default();
    move |body| {
        let runtime = runtime.upgrade()?;
        match serve_forwarded(&body, &runtime, &seen) {
            Ok(()) => Some(Vec::new()),
            Err(err) => {
                tracing::warn!(target: "istmo.documents", %err, "rejected a forwarded launch");
                None
            }
        }
    }
}

fn serve_forwarded(
    body: &[u8],
    runtime: &Runtime,
    seen: &RecentNonces,
) -> Result<(), DocumentsError> {
    let request = ForwardRequest::decode(body)?;
    let token = request.activation_token.as_deref();
    if !seen.first_time(request.nonce) {
        // A copy of a request already served: acknowledge only.
    } else if request.paths.is_empty() {
        // Links open in a window that already exists: raise it too.
        Documents::publish_activation(runtime, token).map_err(DocumentsError::Runtime)?;
        publish_urls(runtime, &request.urls, "running");
    } else {
        publish_urls(runtime, &request.urls, "running");
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
    Ok(())
}

/// What a command line asks the app to open.
#[derive(Debug, Default, PartialEq, Eq)]
struct LaunchArgs {
    /// Documents, made absolute.
    paths: Vec<PathBuf>,
    /// Links, as given.
    urls: Vec<String>,
}

impl LaunchArgs {
    /// Without a working directory relative paths are dropped:
    /// resolving them in the primary would open the wrong files.
    fn parse(args: impl IntoIterator<Item = OsString>, cwd: Option<&Path>) -> Self {
        let mut options_done = false;
        let mut out = Self::default();
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
            let path = match arg.to_str().filter(|arg| is_url(arg)) {
                // Launchers handing `%U` pass local files as `file:` URLs.
                Some(url) => {
                    let Some(path) = file_url_path(url) else {
                        out.urls.push(url.to_owned());
                        continue;
                    };
                    path
                }
                None => PathBuf::from(arg),
            };
            if path.is_absolute() {
                out.paths.push(path);
            } else if let Some(cwd) = cwd {
                out.paths.push(cwd.join(path));
            }
        }
        out
    }
}

/// Whether `arg` starts with a URL scheme (RFC 3986: a letter, then
/// letters, digits, `+`, `-` or `.`, then `:`). One-letter schemes are
/// Windows drive letters (`C:\notes.md`).
fn is_url(arg: &str) -> bool {
    let Some((scheme, _)) = arg.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    scheme.len() > 1
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The local path of a `file:` URL (`file:///a%20b.md`,
/// `file://localhost/a.md`, `file:///C:/a.md` on Windows). `None` for
/// other schemes, remote hosts and paths that do not decode to UTF-8.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = rest.strip_prefix("//")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len());
    let mut iter = path.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let hex = [iter.next()?, iter.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    // `/C:/a.md` → `C:/a.md`.
    #[cfg(windows)]
    let path = match path.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => path[1..].to_owned(),
        _ => path,
    };
    Some(PathBuf::from(path))
}

/// Publish `urls` as deep links; `source` says whether they started the
/// app (`launch`) or reached the running one (`running`).
fn publish_urls(runtime: &Runtime, urls: &[String], source: &str) {
    let received_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok());
    for uri in urls {
        let link = DeepLink {
            uri: uri.clone(),
            source: Some(source.to_owned()),
            received_at_ms,
        };
        if let Err(err) = DeepLinks::publish(runtime, &link) {
            tracing::warn!(target: "istmo.documents", %err, "publishing a link failed");
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_env_gates_single_instance() {
        assert!(instance_key_from_build(Some("rs.example.app"), Some("1")).is_some());
        assert!(instance_key_from_build(Some("rs.example.app"), Some("0")).is_none());
        assert!(instance_key_from_build(Some("rs.example.app"), None).is_none());
        assert!(instance_key_from_build(None, Some("1")).is_none());
        assert!(instance_key_from_build(Some("not a name"), Some("1")).is_none());
    }

    #[test]
    fn command_line_paths() {
        let cwd = Path::new("/work");
        let args = ["--flag", "a.pdf", "/abs/b.md", "--", "-dash.txt", ""].map(OsString::from);
        assert_eq!(
            LaunchArgs::parse(args.clone(), Some(cwd)).paths,
            [
                PathBuf::from("/work/a.pdf"),
                PathBuf::from("/abs/b.md"),
                PathBuf::from("/work/-dash.txt"),
            ]
        );
        assert_eq!(
            LaunchArgs::parse(args, None).paths,
            [PathBuf::from("/abs/b.md")],
            "relative paths need a working directory"
        );
    }

    #[test]
    fn command_line_urls() {
        let args = [
            "notein://open/1b2c",
            "C:\\notes\\a.md",
            "mailto:a@b.c",
            "dir/b:c.md",
            "x-app+v1.2:go",
        ]
        .map(OsString::from);
        let launch = LaunchArgs::parse(args, Some(Path::new("/work")));
        assert_eq!(
            launch.urls,
            ["notein://open/1b2c", "mailto:a@b.c", "x-app+v1.2:go"]
        );
        assert_eq!(
            launch.paths.len(),
            2,
            "drive letters and colons in paths stay paths"
        );
    }

    #[test]
    fn file_urls_are_paths() {
        let args = [
            "file:///notes/a%20b.md",
            "file://localhost/c.pdf",
            "file://server/share/d.md",
        ]
        .map(OsString::from);
        let launch = LaunchArgs::parse(args, None);
        assert_eq!(
            launch.paths,
            [PathBuf::from("/notes/a b.md"), PathBuf::from("/c.pdf")]
        );
        assert_eq!(launch.urls, ["file://server/share/d.md"], "remote host");
        assert_eq!(file_url_path("file:///bad%2"), None);
    }

    #[test]
    fn urls_reach_the_deep_link_queue() {
        let rt = Runtime::mock().runtime;
        publish_urls(&rt, &["notein://open/x".to_owned()], "launch");
        let link = DeepLinks::from_runtime(&rt)
            .unwrap()
            .stream()
            .recv()
            .unwrap();
        assert_eq!(link.uri, "notein://open/x");
        assert_eq!(link.source.as_deref(), Some("launch"));
    }

    #[test]
    fn request_round_trip() {
        let request = ForwardRequest {
            nonce: 7,
            paths: vec!["/a.pdf".to_owned(), "/b c.md".to_owned()],
            urls: vec!["notein://open/x".to_owned()],
            activation_token: Some("token".to_owned()),
        };
        let body = request.encode().unwrap();
        assert_eq!(ForwardRequest::decode(&body).unwrap(), request);
        let trailing = [body.as_slice(), &[0]].concat();
        assert!(matches!(
            ForwardRequest::decode(&trailing),
            Err(DocumentsError::Codec(CodecError::TrailingBytes { .. }))
        ));
    }

    #[test]
    fn huge_length_prefixes_are_rejected_without_allocating() {
        // `paths.len` claims 2^40 entries in a 9-byte body; a plain
        // decode would try to reserve terabytes and abort the process.
        let mut body = vec![0xFD];
        body.extend_from_slice(&(1_u64 << 40).to_le_bytes());
        assert!(matches!(
            ForwardRequest::decode(&body),
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

    /// Two launches in one process: the second finds the first's endpoint
    /// (its lock is a separate open file), forwards and is told to exit.
    #[test]
    fn second_launch_forwards_to_the_primary() {
        let rt = Runtime::mock().runtime;
        let documents = Documents::from_runtime(&rt).unwrap();
        let opened = documents.stream();
        let activations = documents.activations();
        let key =
            EndpointName::try_from(format!("istmo-test-forward-{}", std::process::id()).as_str())
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

        // The app's own endpoint under the same name is a different one.
        let own = LocalEndpoint::from(key.clone());
        assert!(matches!(own.claim().unwrap(), EndpointClaim::Owner(_)));

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
