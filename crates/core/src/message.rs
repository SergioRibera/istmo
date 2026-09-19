//! Marker trait for values that can cross the wire.

/// Anything that can be encoded to and decoded from bincode bytes.
///
/// [`Message`] is a blanket alias — any type that derives
/// [`bincode::Encode`] and [`bincode::Decode`] implements it
/// automatically. Types annotated with
/// [`#[message]`](../../istmo_macros/attr.message.html) satisfy this
/// bound.
pub trait Message: bincode::Encode + bincode::Decode<()> {}

impl<T> Message for T where T: bincode::Encode + bincode::Decode<()> {}
