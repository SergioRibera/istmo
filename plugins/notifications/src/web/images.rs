//! Image bytes as URLs the browser loads: which images a notification
//! uses, their cache keys and `data:` URLs.
//!
//! The browser decodes the bytes itself (nothing is decoded in Rust).
//! Service worker notifications get `data:` URLs: they are
//! self-contained, so they still load after the page that showed them
//! closed (Firefox reloads icons lazily). Page notifications get Blob
//! URLs (no copy into a base64 string), revoked when the last
//! notification using them goes.

use crate::{NotificationImage, Style};

/// Identity of image bytes: FNV-1a 64 of the bytes, their length and
/// type. Equal keys share one URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ImageKey {
    pub(crate) hash: u64,
    pub(crate) len: usize,
    pub(crate) mime: &'static str,
}

/// Where the browser finds an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source<'a> {
    /// Bytes to turn into a URL.
    Bytes(ImageKey, &'a [u8]),
    /// A URL as given ([`NotificationImage::File`]: an absolute path on
    /// the app's origin, or any URL the page may load).
    Url(&'a str),
}

/// FNV-1a 64.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The source of `image`; `None` for platform names (the web has none).
pub(crate) fn source(image: &NotificationImage) -> Option<Source<'_>> {
    let (mime, bytes): (&'static str, &[u8]) = match image {
        NotificationImage::Png(bytes) => ("image/png", bytes),
        NotificationImage::Jpeg(bytes) => ("image/jpeg", bytes),
        NotificationImage::File(url) => return Some(Source::Url(url)),
        NotificationImage::Named(_) => return None,
    };
    Some(Source::Bytes(
        ImageKey {
            hash: fnv1a64(bytes),
            len: bytes.len(),
            mime,
        },
        bytes,
    ))
}

/// The images a notification shows on the web.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Wanted<'a> {
    /// The icon: [`crate::Notification::icon`], else the last sender's
    /// (or the conversation user's) avatar, else the caller's.
    pub(crate) icon: Option<&'a NotificationImage>,
    /// The picture of [`Style::Picture`].
    pub(crate) image: Option<&'a NotificationImage>,
}

/// The images `n` shows.
pub(crate) fn wanted(n: &crate::Notification) -> Wanted<'_> {
    let fallback = match &n.style {
        Style::Messaging(conversation) => conversation
            .messages
            .iter()
            .rev()
            .find_map(|message| message.sender.as_ref())
            .and_then(|sender| sender.avatar.as_ref())
            .or(conversation.user.avatar.as_ref()),
        Style::Call(call) => call.caller.avatar.as_ref(),
        _ => None,
    };
    Wanted {
        icon: n.icon.as_ref().or(fallback),
        image: match &n.style {
            Style::Picture(image) => Some(image),
            _ => None,
        },
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `data:<mime>;base64,<bytes>`.
pub(crate) fn data_url(mime: &str, bytes: &[u8]) -> String {
    let mut out = String::with_capacity(13 + mime.len() + bytes.len().div_ceil(3) * 4);
    out.push_str("data:");
    out.push_str(mime);
    out.push_str(";base64,");
    let sextet = |n: u32, shift: u32| char::from(BASE64[((n >> shift) & 63) as usize]);
    let (chunks, rest) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let n = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
        out.extend([sextet(n, 18), sextet(n, 12), sextet(n, 6), sextet(n, 0)]);
    }
    match *rest {
        [a] => {
            let n = u32::from(a) << 16;
            out.extend([sextet(n, 18), sextet(n, 12), '=', '=']);
        }
        [a, b] => {
            let n = u32::from(a) << 16 | u32::from(b) << 8;
            out.extend([sextet(n, 18), sextet(n, 12), sextet(n, 6), '=']);
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallInfo, CallKind, ChatMessage, Conversation, Notification, Participant};

    #[test]
    fn data_urls_are_base64() {
        assert_eq!(data_url("image/png", b""), "data:image/png;base64,");
        assert_eq!(data_url("image/png", b"f"), "data:image/png;base64,Zg==");
        assert_eq!(data_url("image/png", b"fo"), "data:image/png;base64,Zm8=");
        assert_eq!(data_url("image/png", b"foo"), "data:image/png;base64,Zm9v");
        assert_eq!(
            data_url("image/jpeg", b"foobar"),
            "data:image/jpeg;base64,Zm9vYmFy"
        );
        assert_eq!(data_url("x", &[0xff, 0xfe, 0x00]), "data:x;base64,//4A");
    }

    #[test]
    fn equal_bytes_share_a_key() {
        let a = NotificationImage::png(vec![1, 2, 3]);
        let b = NotificationImage::png(vec![1, 2, 3]);
        let c = NotificationImage::jpeg(vec![1, 2, 3]);
        let key = |image| match source(image) {
            Some(Source::Bytes(key, _)) => key,
            other => panic!("{other:?}"),
        };
        assert_eq!(key(&a), key(&b));
        assert_ne!(key(&a), key(&c));
        assert_eq!(
            source(&NotificationImage::file("/icons/a.png")),
            Some(Source::Url("/icons/a.png"))
        );
        assert_eq!(source(&NotificationImage::named("ic")), None);
    }

    #[test]
    fn avatars_stand_in_for_a_missing_icon() {
        let avatar = NotificationImage::png(vec![9]);
        let ana = Participant::new("ana", "Ana").with_avatar(avatar.clone());
        let me = Participant::new("me", "Me");
        let chat = Notification::new("c", "Chat")
            .unwrap()
            .with_style(Style::Messaging(Conversation {
                user: me,
                title: None,
                is_group: false,
                messages: vec![ChatMessage::received(ana.clone(), "hi")],
            }));
        assert_eq!(wanted(&chat).icon, Some(&avatar));
        let call = Notification::new("k", "Call")
            .unwrap()
            .with_style(Style::Call(CallInfo {
                caller: ana,
                kind: CallKind::Incoming,
                video: false,
            }));
        assert_eq!(wanted(&call).icon, Some(&avatar));
        let own = NotificationImage::png(vec![1]);
        let picture = NotificationImage::jpeg(vec![2]);
        let n = Notification::new("p", "P")
            .unwrap()
            .with_icon(own.clone())
            .with_style(Style::Picture(picture.clone()));
        assert_eq!(
            wanted(&n),
            Wanted {
                icon: Some(&own),
                image: Some(&picture)
            }
        );
    }
}
