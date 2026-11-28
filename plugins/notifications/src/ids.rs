//! Identifier newtypes: `1..=64` bytes of `[A-Za-z0-9._:-]`.
//!
//! Every id is checked by its `TryFrom<&str>` impl; the wire never
//! carries anything else from a Rust client. Hosts check again
//! ([`crate::Notification::validate`]) because Kotlin and Swift callers
//! build ids without this code.

use istmo_macros::message;

use crate::{InvalidField, Problem};

/// Longest id, in bytes.
pub const MAX_ID_LEN: usize = 64;

/// Prefix of the action ids the plugin itself uses (`istmo.more`,
/// `istmo.call.answer`, ...). Apps cannot create an [`ActionId`] with it.
pub const RESERVED_ACTION_PREFIX: &str = "istmo.";

/// Checks the shared id rule; `field` names the id in the error.
pub(crate) fn check_id(field: &str, value: &str) -> Result<(), InvalidField> {
    if value.is_empty() {
        return Err(InvalidField::new(field, Problem::Empty));
    }
    if value.len() > MAX_ID_LEN {
        return Err(InvalidField::new(field, Problem::TooLong));
    }
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-');
    if !value.bytes().all(allowed) {
        return Err(InvalidField::new(field, Problem::BadCharset));
    }
    Ok(())
}

macro_rules! id_impls {
    ($ty:ident, $field:literal) => {
        impl $ty {
            /// The id as a string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.value
            }

            /// Checks the id rule (for values built without `TryFrom`,
            /// such as ones decoded from the wire).
            pub fn validate(&self) -> Result<(), InvalidField> {
                check_id($field, &self.value)
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = InvalidField;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_owned())
            }
        }

        impl TryFrom<String> for $ty {
            type Error = InvalidField;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                let id = Self { value };
                id.validate()?;
                Ok(id)
            }
        }

        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.value)
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.value
            }
        }
    };
}

/// Identifies one notification: showing another with the same id
/// replaces it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NotificationId {
    /// `1..=64` bytes of `[A-Za-z0-9._:-]`.
    pub value: String,
}

/// Identifies a channel (Android notification channel; elsewhere a set
/// of defaults merged into each notification).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId {
    /// `1..=64` bytes of `[A-Za-z0-9._:-]`.
    pub value: String,
}

/// Identifies an action of a notification. Ids starting with `istmo.`
/// are the plugin's own.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionId {
    /// `1..=64` bytes of `[A-Za-z0-9._:-]`, not starting with `istmo.`.
    pub value: String,
}

/// Identifies a group (Android group, Apple thread, Windows group).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId {
    /// `1..=64` bytes of `[A-Za-z0-9._:-]`.
    pub value: String,
}

id_impls!(NotificationId, "id");
id_impls!(ChannelId, "channel");
id_impls!(GroupId, "group");

impl ActionId {
    /// The "More" action that opens the app with the actions the
    /// platform could not show.
    pub const MORE: &'static str = "istmo.more";
    /// Answer an incoming call ([`crate::Style::Call`]).
    pub const CALL_ANSWER: &'static str = "istmo.call.answer";
    /// Decline an incoming call.
    pub const CALL_DECLINE: &'static str = "istmo.call.decline";
    /// Hang up an ongoing call.
    pub const CALL_HANG_UP: &'static str = "istmo.call.hang_up";

    /// The id as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Whether this is one of the plugin's own ids (`istmo.` prefix).
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        self.value.starts_with(RESERVED_ACTION_PREFIX)
    }

    /// Checks the id rule. Reserved ids pass: hosts and natives build
    /// them, and they come back in responses.
    pub fn validate(&self) -> Result<(), InvalidField> {
        check_id("action", &self.value)
    }

    /// A plugin-owned id (`istmo.` prefix), built without the reserved
    /// check.
    pub(crate) fn reserved(value: &'static str) -> Self {
        Self {
            value: value.to_owned(),
        }
    }
}

impl TryFrom<&str> for ActionId {
    type Error = InvalidField;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl TryFrom<String> for ActionId {
    type Error = InvalidField;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let id = Self { value };
        id.validate()?;
        if id.is_reserved() {
            return Err(InvalidField::new("action", Problem::Reserved));
        }
        Ok(id)
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value)
    }
}

impl AsRef<str> for ActionId {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_the_rule() {
        assert!(NotificationId::try_from("dl:42").is_ok());
        assert!(NotificationId::try_from("a.b_c-d:e").is_ok());
        assert_eq!(
            NotificationId::try_from("").unwrap_err().problem,
            Problem::Empty
        );
        assert_eq!(
            NotificationId::try_from("a b").unwrap_err().problem,
            Problem::BadCharset
        );
        assert_eq!(
            NotificationId::try_from("é").unwrap_err().problem,
            Problem::BadCharset
        );
        let long = "x".repeat(MAX_ID_LEN + 1);
        assert_eq!(
            ChannelId::try_from(long.as_str()).unwrap_err().problem,
            Problem::TooLong
        );
        assert!(GroupId::try_from("x".repeat(MAX_ID_LEN)).is_ok());
    }

    #[test]
    fn action_ids_reserve_the_istmo_prefix() {
        let err = ActionId::try_from("istmo.more").unwrap_err();
        assert_eq!(err.problem, Problem::Reserved);
        assert_eq!(err.field, "action");
        assert!(ActionId::reserved(ActionId::MORE).validate().is_ok());
        assert!(ActionId::try_from("istmo").is_ok());
    }
}
