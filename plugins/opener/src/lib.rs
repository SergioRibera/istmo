//! Open URLs with the system, for [`istmo`](https://docs.rs/istmo).
//!
//! [`Opener::open`] hands a URL to whatever the platform uses for it:
//! the browser for `https:`, the mail app for `mailto:`, the dialer for
//! `tel:`, maps for `geo:`.
//!
//! * Android — an `ACTION_VIEW` intent in `CATEGORY_BROWSABLE`, started
//!   from the app context.
//! * iOS — `UIApplication.shared.open`.
//! * Desktop — [`DesktopOpener`]: `xdg-open`, `open` or
//!   `rundll32 url.dll,FileProtocolHandler`, run without a shell.
//!
//! # Scheme allow-list
//!
//! URLs usually come from content the app does not control, and an
//! opener is a launcher for arbitrary apps. Every backend therefore
//! only opens schemes on an allow-list — by default
//! [`DEFAULT_ALLOWED_SCHEMES`] — and refuses the rest with
//! [`OpenError::SchemeNotAllowed`]. Rust callers validate before the
//! call ever leaves the process with [`OpenerClient::open_checked`] and
//! a [`SchemePolicy`]; the reference native backends take the same list
//! as a constructor argument, and [`DesktopOpener::with_policy`] does
//! the same on desktop.
//!
//! The generated `IstmoPluginRegistry` registers both mobile backends;
//! desktop apps register [`DesktopOpener`] themselves.

#![doc(html_root_url = "https://docs.rs/istmo-opener")]

#[cfg(feature = "codegen")]
pub mod codegen;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use istmo_macros::{message, plugin};

/// Wire identifier for the opener plugin.
pub const OPENER_PLUGIN_ID: &str = "istmo.opener";

/// Schemes every backend opens unless configured otherwise.
pub const DEFAULT_ALLOWED_SCHEMES: &[&str] = &["http", "https", "mailto", "tel", "sms", "geo"];

/// Why a URL did not open.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OpenError {
    /// Nothing on the device handles this kind of URL.
    NoHandler,
    /// The URL could not be parsed.
    Invalid,
    /// Any other platform failure, with its message.
    Backend(String),
    /// The URL's scheme (carried, lowercased) is not on the allow-list.
    SchemeNotAllowed(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHandler => f.write_str("no app handles this url"),
            Self::Invalid => f.write_str("invalid url"),
            Self::Backend(msg) => write!(f, "opener backend error: {msg}"),
            Self::SchemeNotAllowed(scheme) => write!(f, "url scheme {scheme:?} is not allowed"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Recover the typed error from a failed [`OpenerClient::open`].
impl From<istmo_core::IstmoError> for OpenError {
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                istmo_core::codec::decode::<Self>(&bytes).map_or_else(
                    |e| Self::Backend(format!("decode plugin error payload: {e}")),
                    |(err, _)| err,
                )
            }
            other => Self::Backend(other.to_string()),
        }
    }
}

/// The plugin trait.
#[plugin(name = "istmo.opener", crate = "::istmo_core")]
pub trait Opener {
    /// Hand `url` to the system.
    async fn open(&self, url: String) -> Result<(), OpenError>;
}

/// Which URL schemes may be opened.
///
/// Defaults to [`DEFAULT_ALLOWED_SCHEMES`]. Schemes compare
/// case-insensitively, as RFC 3986 requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemePolicy {
    schemes: Vec<String>,
}

impl Default for SchemePolicy {
    fn default() -> Self {
        Self::only(DEFAULT_ALLOWED_SCHEMES.iter().copied())
    }
}

impl SchemePolicy {
    /// A policy allowing exactly `schemes`.
    #[must_use]
    pub fn only<S: AsRef<str>>(schemes: impl IntoIterator<Item = S>) -> Self {
        Self {
            schemes: schemes
                .into_iter()
                .map(|s| s.as_ref().to_ascii_lowercase())
                .collect(),
        }
    }

    /// This policy plus `scheme` (e.g. `"file"` or an app's own scheme).
    #[must_use]
    pub fn allow(mut self, scheme: &str) -> Self {
        let scheme = scheme.to_ascii_lowercase();
        if !self.schemes.contains(&scheme) {
            self.schemes.push(scheme);
        }
        self
    }

    /// Check `url` against the policy.
    ///
    /// Rejects as [`OpenError::Invalid`] a URL that is empty, starts
    /// with `-` (it would read as an option to a desktop opener),
    /// contains control characters or invisible format characters
    /// (Unicode `Cf`: zero-width spaces, bidi overrides and isolates,
    /// BOM, ...) or has no RFC 3986 scheme; rejects as
    /// [`OpenError::SchemeNotAllowed`] a well-formed URL whose scheme is
    /// not allowed.
    pub fn check(&self, url: &str) -> Result<(), OpenError> {
        if url.is_empty()
            || url.starts_with('-')
            || url.chars().any(|c| c.is_control() || is_format_char(c))
        {
            return Err(OpenError::Invalid);
        }
        let scheme = parse_scheme(url).ok_or(OpenError::Invalid)?;
        let scheme = scheme.to_ascii_lowercase();
        if self.schemes.contains(&scheme) {
            Ok(())
        } else {
            Err(OpenError::SchemeNotAllowed(scheme))
        }
    }
}

/// Unicode general category `Cf` (format), as of Unicode 15.1.
///
/// Invisible in rendered text, so a URL carrying one reads differently
/// from where it goes: a bidi override can flip `moc.elpmaxe` into
/// view, a zero-width space can split a familiar host name. No URL
/// needs them raw; percent-encoded they stay harmless.
const fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`, terminated by `:`.
fn parse_scheme(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme)
}

/// `url` with whitespace and `"` percent-encoded, so it stays a single
/// unquoted token on a raw command line.
#[cfg_attr(
    not(any(target_os = "windows", test)),
    expect(dead_code, reason = "Windows-only")
)]
fn encode_for_rundll32(url: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        if c == '"' || c.is_whitespace() {
            let mut buf = [0; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                let _ = write!(out, "%{byte:02X}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl OpenerClient {
    /// [`Self::open`] after checking `url` against `policy`, so a
    /// disallowed URL never leaves the process.
    pub async fn open_checked(&self, url: String, policy: &SchemePolicy) -> Result<(), OpenError> {
        policy.check(&url)?;
        self.open(url).await.map_err(OpenError::from)
    }
}

/// Desktop backend over the system opener command.
#[derive(Debug, Clone, Default)]
pub struct DesktopOpener {
    policy: SchemePolicy,
}

/// How long [`DesktopOpener`] waits for the opener command's exit
/// status. The usual openers hand the URL off and exit at once; one
/// that is still running after this is taken to have launched the
/// handler itself, and is reaped in the background.
const EXIT_WAIT: Duration = Duration::from_secs(5);
const EXIT_POLL: Duration = Duration::from_millis(20);

impl DesktopOpener {
    /// An opener that only opens schemes allowed by `policy`.
    #[must_use]
    pub const fn with_policy(policy: SchemePolicy) -> Self {
        Self { policy }
    }

    // Each opener takes the URL as one argv entry; none goes through a
    // shell, so `&`, `|` or quotes in the URL stay inert.
    fn command(url: &str) -> Command {
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut cmd = Command::new("open");
            cmd.arg(url);
            cmd
        };
        // rundll32 does not split its command line into argv: everything
        // after the entry point reaches `FileProtocolHandler` verbatim,
        // so the quotes and backslashes `Command::arg` would add stay in
        // the URL. Pass it raw instead, with the only characters that
        // could end or quote it (whitespace, `"`) percent-encoded — they
        // are not valid unencoded in a URL anyway.
        #[cfg(target_os = "windows")]
        let mut cmd = {
            use std::os::windows::process::CommandExt as _;
            let mut cmd = Command::new("rundll32");
            cmd.arg("url.dll,FileProtocolHandler")
                .raw_arg(encode_for_rundll32(url));
            cmd
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let mut cmd = {
            let mut cmd = Command::new("xdg-open");
            cmd.arg(url);
            cmd
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    fn spawn(url: &str) -> Result<Child, OpenError> {
        Self::command(url).spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => OpenError::NoHandler,
            _ => OpenError::Backend(e.to_string()),
        })
    }

    /// Wait for `child` on a named thread and report its exit status
    /// through `done`; keep waiting after the deadline so the process
    /// never lingers as a zombie.
    fn watch(
        mut child: Child,
        done: flume::Sender<Result<(), OpenError>>,
    ) -> Result<(), OpenError> {
        std::thread::Builder::new()
            .name("istmo-opener-wait".into())
            .spawn(move || {
                let start = Instant::now();
                let deadline = start.checked_add(EXIT_WAIT).unwrap_or(start);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let result = if status.success() {
                                Ok(())
                            } else {
                                Err(OpenError::NoHandler)
                            };
                            // The caller may have dropped its future; nothing to report then.
                            let _ = done.send(result);
                            return;
                        }
                        Ok(None) if Instant::now() < deadline => std::thread::sleep(EXIT_POLL),
                        Ok(None) => {
                            let _ = done.send(Ok(()));
                            let _ = child.wait();
                            return;
                        }
                        Err(e) => {
                            let _ = done.send(Err(OpenError::Backend(e.to_string())));
                            return;
                        }
                    }
                }
            })
            .map(drop)
            .map_err(|e| OpenError::Backend(format!("spawn opener wait thread: {e}")))
    }
}

impl Opener for DesktopOpener {
    fn open(
        &self,
        url: String,
    ) -> impl std::future::Future<Output = Result<(), OpenError>> + Send + '_ {
        let (tx, rx) = flume::bounded(1);
        let started = self
            .policy
            .check(&url)
            .and_then(|()| Self::spawn(&url))
            .and_then(|child| Self::watch(child, tx));
        async move {
            started?;
            rx.recv_async()
                .await
                .unwrap_or_else(|_| Err(OpenError::Backend("opener wait thread vanished".into())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::encode_for_rundll32;

    #[test]
    fn rundll32_argument_stays_one_unquoted_token() {
        assert_eq!(
            encode_for_rundll32("https://a.test/?q=x y&r=\"z\""),
            "https://a.test/?q=x%20y&r=%22z%22"
        );
        assert_eq!(
            encode_for_rundll32("mailto:a@b?subject=hi\u{3000}"),
            "mailto:a@b?subject=hi%E3%80%80"
        );
        assert_eq!(
            encode_for_rundll32("https://a.test/%20"),
            "https://a.test/%20"
        );
    }
}
