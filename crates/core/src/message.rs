//! Convenience trait alias for values crossing the istmo wire boundary.
//!
//! Every argument, return value, error payload and event carried across the
//! FFI boundary is (de)serialised via `bincode` 2. Types that satisfy the
//! codec bounds automatically satisfy [`Message`] through the blanket impl,
//! so plugin authors rarely name the trait directly — they usually just
//! ensure their types derive `bincode::Encode` and `bincode::Decode`.

/// Marker trait implemented by every type that can round-trip through the
/// istmo wire codec.
///
/// Implemented automatically for any `T: bincode::Encode + bincode::Decode<()>`.
pub trait Message: bincode::Encode + bincode::Decode<()> {}

impl<T> Message for T where T: bincode::Encode + bincode::Decode<()> {}
