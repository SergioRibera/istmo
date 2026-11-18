//! Cheap-clone byte buffer for plugin payloads.
//!
//! [`Bytes`] wraps `Arc<[u8]>`. Cloning is an atomic increment; it costs
//! two nanoseconds on modern CPUs regardless of the buffer's length,
//! which is useful for plugins that keep large binary payloads around
//! while they fan them out to subscribers or logs.
//!
//! Over the wire `Bytes` encodes exactly like `Vec<u8>` (varint length
//! then raw bytes), so the two are interchangeable in `#[message]`
//! shapes and across runtime versions.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use wincode::adapter::FromInto;

/// Reference-counted, immutable byte buffer.
///
/// # Wire format
///
/// `Bytes` serializes with the same layout as `Vec<u8>` — a varint
/// length followed by the raw bytes — so a field declared `Bytes` on
/// one end of the wire can be received into a `Vec<u8>` on the other,
/// and vice versa. Use the adapter attribute on a `#[message]` field:
///
/// ```ignore
/// use istmo::{Bytes, message};
///
/// #[message]
/// pub struct Payload {
///     #[wincode(with = "wincode::adapter::FromInto<Vec<u8>, _>")]
///     pub body: Bytes,
/// }
/// ```
///
/// # Cheap clone
///
/// Cloning `Bytes` is an atomic increment — no allocation, no copy of
/// the underlying bytes — so a plugin can hand the same payload to a
/// log sink, a cache and a downstream call without duplicating memory.
/// The buffer itself is immutable; obtain owned bytes by calling
/// [`into_vec`](Self::into_vec) (which copies when the buffer is
/// shared and takes the backing allocation when it is not).
#[derive(Clone, Default)]
pub struct Bytes(Arc<[u8]>);

impl Bytes {
    /// Empty buffer, no allocation.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::from(Vec::<u8>::new()))
    }

    /// Copy `slice` into a fresh buffer.
    #[must_use]
    pub fn copy_from_slice(slice: &[u8]) -> Self {
        Self(Arc::from(slice))
    }

    /// Take ownership of `vec`'s allocation.
    #[must_use]
    pub fn from_vec(vec: Vec<u8>) -> Self {
        Self(Arc::from(vec.into_boxed_slice()))
    }

    /// View the underlying bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Length of the buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when the buffer carries no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Clone cheap-ref to the underlying `Arc<[u8]>`.
    #[must_use]
    pub fn as_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.0)
    }

    /// Return the bytes as an owned `Vec<u8>`.
    ///
    /// Reuses the backing allocation when this is the only live clone;
    /// otherwise copies. No way to tell from the outside which path
    /// ran — treat it as "owned, cost proportional to length".
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0.as_ref().to_vec()
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bytes")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for Bytes {}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        &**self == other
    }
}

impl PartialEq<&[u8]> for Bytes {
    fn eq(&self, other: &&[u8]) -> bool {
        &**self == *other
    }
}

impl PartialEq<Vec<u8>> for Bytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        &**self == other.as_slice()
    }
}

impl<const N: usize> PartialEq<[u8; N]> for Bytes {
    fn eq(&self, other: &[u8; N]) -> bool {
        &**self == other.as_slice()
    }
}

impl std::hash::Hash for Bytes {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(vec: Vec<u8>) -> Self {
        Self::from_vec(vec)
    }
}

impl From<&Vec<u8>> for Bytes {
    fn from(vec: &Vec<u8>) -> Self {
        Self::copy_from_slice(vec)
    }
}

impl From<&[u8]> for Bytes {
    fn from(slice: &[u8]) -> Self {
        Self::copy_from_slice(slice)
    }
}

impl<const N: usize> From<[u8; N]> for Bytes {
    fn from(array: [u8; N]) -> Self {
        Self::copy_from_slice(&array)
    }
}

impl From<Arc<[u8]>> for Bytes {
    fn from(arc: Arc<[u8]>) -> Self {
        Self(arc)
    }
}

impl From<Bytes> for Vec<u8> {
    fn from(bytes: Bytes) -> Vec<u8> {
        bytes.into_vec()
    }
}

impl From<&Bytes> for Vec<u8> {
    fn from(bytes: &Bytes) -> Vec<u8> {
        bytes.as_slice().to_vec()
    }
}

/// Wire adapter that encodes `Bytes` byte-for-byte like `Vec<u8>`.
///
/// Field macro: `#[wincode(with = "istmo::bytes::WireVec")]`.
pub type WireVec = FromInto<Vec<u8>, Bytes>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_shares_backing() {
        let a = Bytes::from(vec![1, 2, 3, 4]);
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(Arc::strong_count(&a.as_arc()), 3); // a, b, and the fresh clone from as_arc
    }

    #[test]
    fn empty_bytes() {
        let a = Bytes::new();
        assert!(a.is_empty());
        assert_eq!(a.len(), 0);
        assert_eq!(a.as_slice(), &[] as &[u8]);
    }

    #[test]
    fn from_slice_copies() {
        let src = [9u8, 8, 7];
        let b = Bytes::from(&src[..]);
        assert_eq!(b, src);
    }

    #[test]
    fn into_vec_matches_slice() {
        let b = Bytes::from(vec![1, 2, 3]);
        assert_eq!(b.clone().into_vec(), vec![1, 2, 3]);
    }
}
