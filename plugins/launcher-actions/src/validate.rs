//! Client-side checks of [`LauncherAction`]s.
//!
//! `istmo-build` applies the same rules to `[[app.launcher_action]]`
//! (`crates/build/src/launcher_actions.rs`); keep the two in step.

use crate::{
    ActionIcon, ActionProblem, LauncherAction, LauncherError, MAX_ICON_BYTES, MAX_ID_LEN,
    MAX_SUBTITLE_CHARS, MAX_TITLE_CHARS, MAX_URL_LEN, Progress,
};

impl Progress {
    /// The fraction of a `Normal` / `Paused` / `Error` state, clamped to
    /// `0.0..=1.0` (`NaN` reads as `0.0`).
    #[must_use]
    pub const fn fraction(self) -> Option<f32> {
        match self {
            Self::Clear | Self::Indeterminate => None,
            Self::Normal(f) | Self::Paused(f) | Self::Error(f) => {
                Some(if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) })
            }
        }
    }
}

/// First bytes of every PNG file.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

impl LauncherAction {
    /// Check one action.
    ///
    /// The id is `1..=64` bytes of `[A-Za-z0-9._-]`; title and subtitle
    /// are non-blank, short and free of control characters; the URL has
    /// an RFC 3986 scheme other than `http(s)`, does not start with `-`
    /// and holds no whitespace, `"`, control or invisible format
    /// characters (it travels as one unquoted command-line argument on
    /// Windows and Linux); a PNG icon is a PNG of at most 1 MiB.
    pub fn validate(&self) -> Result<(), LauncherError> {
        self.problem()
            .map_or(Ok(()), |reason| Err(self.invalid(reason)))
    }

    /// Check a whole set: every action, and ids unique within it. The
    /// platform's own maximum is the backend's to enforce
    /// ([`LauncherError::TooMany`]).
    pub fn validate_set(actions: &[Self]) -> Result<(), LauncherError> {
        for (index, action) in actions.iter().enumerate() {
            action.validate()?;
            if actions[..index].iter().any(|other| other.id == action.id) {
                return Err(action.invalid("id used by an earlier action"));
            }
        }
        Ok(())
    }

    fn invalid(&self, reason: &str) -> LauncherError {
        LauncherError::InvalidAction(ActionProblem {
            id: self.id.clone(),
            reason: reason.to_owned(),
        })
    }

    fn problem(&self) -> Option<&'static str> {
        id_problem(&self.id)
            .or_else(|| Label::Title.problem(&self.title))
            .or_else(|| {
                self.subtitle
                    .as_deref()
                    .and_then(|subtitle| Label::Subtitle.problem(subtitle))
            })
            .or_else(|| url_problem(&self.url))
            .or_else(|| self.icon.as_ref().and_then(ActionIcon::problem))
    }
}

impl ActionIcon {
    fn problem(&self) -> Option<&'static str> {
        match self {
            Self::Symbol(name) => (name.is_empty()
                || name.len() > 128
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .then_some("symbol name must be 1..=128 of [A-Za-z0-9._-]"),
            Self::Png(bytes) if !bytes.starts_with(PNG_SIGNATURE) => Some("icon is not a PNG"),
            Self::Png(bytes) if bytes.len() > MAX_ICON_BYTES => Some("icon is larger than 1 MiB"),
            Self::Png(_) => None,
        }
    }
}

fn id_problem(id: &str) -> Option<&'static str> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Some("id must be 1..=64 bytes");
    }
    (!id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    .then_some("id may only hold [A-Za-z0-9._-]")
}

/// The two text fields of an action.
#[derive(Debug, Clone, Copy)]
enum Label {
    Title,
    Subtitle,
}

impl Label {
    const fn max_chars(self) -> usize {
        match self {
            Self::Title => MAX_TITLE_CHARS,
            Self::Subtitle => MAX_SUBTITLE_CHARS,
        }
    }

    fn problem(self, text: &str) -> Option<&'static str> {
        if text.trim().is_empty() {
            return Some(match self {
                Self::Title => "title is blank",
                Self::Subtitle => "subtitle is blank",
            });
        }
        if text.chars().count() > self.max_chars() {
            return Some(match self {
                Self::Title => "title is longer than 128 characters",
                Self::Subtitle => "subtitle is longer than 256 characters",
            });
        }
        text.chars()
            .any(char::is_control)
            .then_some("label holds control characters")
    }
}

fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return Some("url must be 1..=2048 bytes");
    }
    if url.starts_with('-') {
        return Some("url starts with `-`");
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c.is_control() || is_format_char(c))
    {
        return Some("url holds whitespace, `\"`, control or format characters");
    }
    let Some(scheme) = parse_scheme(url) else {
        return Some("url has no RFC 3986 scheme");
    };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        .then_some("http(s) urls belong to the opener, not to a launcher action")
}

/// `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`, terminated by `:`.
fn parse_scheme(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Invisible format characters (Unicode `Cf`) that make a URL read
/// differently from where it goes: zero-width spaces, bidi controls,
/// BOM, tags. Same list as `istmo-opener`.
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
