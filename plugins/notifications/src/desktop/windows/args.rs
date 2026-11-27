//! The `launch` / `arguments` strings of a toast: which notification,
//! which button, and the fields an activation needs when the process
//! that showed the toast is gone ([`crate::desktop::Carried`]).
//!
//! Format: `istmo1&i=<id>[&k=<key>][&u=<user data>][&l=<url>][&s=<fire ms>]`.
//! Values are percent-encoded for `%`, `&`, `=` and control characters;
//! the XML writer escapes the rest. `s` marks a toast put up by a
//! scheduled toast (the fire time it was armed for).

use crate::NotificationId;
use crate::desktop::{Activation, Carried};

/// Version prefix; strings without it are not ours.
const PREFIX: &str = "istmo1";

/// The decoded arguments of a toast or one of its buttons.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Args {
    /// The notification id (also the toast tag).
    pub(crate) id: String,
    /// The rendered action key; `None` for the toast body or header.
    pub(crate) key: Option<String>,
    /// User data carried for a later process.
    pub(crate) user_data: Option<String>,
    /// URL carried for a later process.
    pub(crate) url: Option<String>,
    /// Fire time of the scheduled toast that showed it.
    pub(crate) fire_ms: Option<i64>,
}

fn push_encoded(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '%' | '&' | '=' => {
                out.push_str(match c {
                    '%' => "%25",
                    '&' => "%26",
                    _ => "%3D",
                });
            }
            c if c.is_control() && u32::from(c) < 0x80 => {
                let byte = u32::from(c);
                out.push('%');
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0xf));
            }
            c => out.push(c),
        }
    }
}

fn hex_digit(nibble: u32) -> char {
    char::from_digit(nibble, 16)
        .unwrap_or('0')
        .to_ascii_uppercase()
}

fn decode_value(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl Args {
    /// The arguments of the body of `id`.
    #[cfg(test)]
    pub(crate) fn body(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            ..Self::default()
        }
    }

    /// The encoded string.
    pub(crate) fn encode(&self) -> String {
        let mut out = String::with_capacity(
            PREFIX.len()
                + 4
                + self.id.len()
                + self.key.as_ref().map_or(0, |k| k.len() + 3)
                + self.user_data.as_ref().map_or(0, |u| u.len() + 3)
                + self.url.as_ref().map_or(0, |u| u.len() + 3)
                + 24,
        );
        out.push_str(PREFIX);
        let mut field = |name: char, value: &str| {
            out.push('&');
            out.push(name);
            out.push('=');
            push_encoded(&mut out, value);
        };
        field('i', &self.id);
        if let Some(key) = &self.key {
            field('k', key);
        }
        if let Some(user_data) = &self.user_data {
            field('u', user_data);
        }
        if let Some(url) = &self.url {
            field('l', url);
        }
        if let Some(fire) = self.fire_ms {
            field('s', &fire.to_string());
        }
        out
    }

    /// Decodes `raw`; `None` when it is not ours or is malformed.
    pub(crate) fn decode(raw: &str) -> Option<Self> {
        let mut parts = raw.split('&');
        if parts.next()? != PREFIX {
            return None;
        }
        let mut args = Self::default();
        for part in parts {
            let (name, value) = part.split_once('=')?;
            let value = decode_value(value)?;
            match name {
                "i" => args.id = value,
                "k" => args.key = Some(value),
                "u" => args.user_data = Some(value),
                "l" => args.url = Some(value),
                "s" => args.fire_ms = Some(value.parse().ok()?),
                // Fields of a later format: ignored.
                _ => {}
            }
        }
        (!args.id.is_empty()).then_some(args)
    }
}

/// The activation of a toast or button whose arguments are `raw`, with
/// the user input COM handed over (`(input id, value)`; the input id of
/// a reply or selection is its action key). `None` when the arguments
/// are not ours.
pub(crate) fn activation(
    raw: &str,
    inputs: &[(String, String)],
    launched_app: bool,
) -> Option<Activation> {
    let args = Args::decode(raw)?;
    let id = NotificationId::try_from(args.id).ok()?;
    let reply = args.key.as_deref().and_then(|key| {
        inputs
            .iter()
            .find(|(input, _)| input == key)
            .map(|(_, value)| value.clone())
    });
    let carried = (args.user_data.is_some() || args.url.is_some()).then_some(Carried {
        user_data: args.user_data,
        url: args.url,
    });
    Some(Activation {
        id,
        key: args.key,
        reply,
        launched_app,
        carried,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activations_take_the_input_of_their_key() {
        let raw = Args {
            id: "chat:1".to_owned(),
            key: Some("reply".to_owned()),
            user_data: Some("u".to_owned()),
            ..Args::default()
        }
        .encode();
        let inputs = vec![
            ("mood".to_owned(), "Good".to_owned()),
            ("reply".to_owned(), "On my way".to_owned()),
        ];
        let activation = activation(&raw, &inputs, true).unwrap();
        assert_eq!(activation.id.as_str(), "chat:1");
        assert_eq!(activation.key.as_deref(), Some("reply"));
        assert_eq!(activation.reply.as_deref(), Some("On my way"));
        assert!(activation.launched_app);
        assert_eq!(
            activation.carried,
            Some(Carried {
                user_data: Some("u".to_owned()),
                url: None
            })
        );
        let body = super::activation(&Args::body("n").encode(), &inputs, false).unwrap();
        assert_eq!((body.key, body.reply, body.carried), (None, None, None));
        assert!(super::activation("istmo1&i=bad id", &[], false).is_none());
        assert!(super::activation("foreign", &[], false).is_none());
    }

    #[test]
    fn round_trips_every_field() {
        let args = Args {
            id: "chat:42".to_owned(),
            key: Some("reply#1".to_owned()),
            user_data: Some("a=b&c=100%\n{\"k\":\"<v>\"}".to_owned()),
            url: Some("app://chat/42?x=1&y=2".to_owned()),
            fire_ms: Some(1_700_000_000_000),
        };
        let encoded = args.encode();
        assert_eq!(
            encoded,
            "istmo1&i=chat:42&k=reply#1&u=a%3Db%26c%3D100%25%0A{\"k\":\"<v>\"}\
             &l=app://chat/42?x%3D1%26y%3D2&s=1700000000000"
        );
        assert_eq!(Args::decode(&encoded), Some(args));
    }

    #[test]
    fn body_has_only_the_id() {
        assert_eq!(Args::body("n").encode(), "istmo1&i=n");
        assert_eq!(Args::decode("istmo1&i=n"), Some(Args::body("n")));
    }

    #[test]
    fn foreign_or_broken_strings_are_refused() {
        assert_eq!(Args::decode("action=open&id=1"), None);
        assert_eq!(Args::decode("istmo1&i="), None);
        assert_eq!(Args::decode("istmo1&i=n&u=%4"), None);
        assert_eq!(Args::decode("istmo1&i=n&s=soon"), None);
        assert_eq!(Args::decode("istmo1&i=n&broken"), None);
        assert_eq!(
            Args::decode("istmo1&i=n&z=later").map(|a| a.id),
            Some("n".to_owned())
        );
    }
}
