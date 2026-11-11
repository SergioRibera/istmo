//! Marker trait for values that can cross the wire.

/// Anything that can be encoded to and decoded from wire bytes.
///
/// [`Message`] is a blanket alias — any type that derives
/// [`wincode::SchemaWrite`] and [`wincode::SchemaRead`] implements it
/// automatically. Types annotated with
/// [`#[message]`](../../istmo_macros/attr.message.html) satisfy this
/// bound.
pub trait Message: crate::codec::Encode + crate::codec::Decode {}

impl<T> Message for T where T: crate::codec::Encode + crate::codec::Decode {}
