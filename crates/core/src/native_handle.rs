//! Typed, drop-safe owners of platform-side resources.

use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use crate::protocol::NativeHandleId;
use crate::runtime::Runtime;

/// Ownership handle for an opaque, platform-owned object referenced by
/// a [`NativeHandleId`].
///
/// `T` is a phantom marker for the platform type the id refers to (for
/// example `SignInCredential`). Dropping the handle emits a
/// [`Frame::ReleaseNativeHandle`](crate::protocol::Frame::ReleaseNativeHandle)
/// to the peer so the underlying platform object can be freed.
///
/// The handle deliberately does not implement [`Clone`]: duplicating an
/// id would risk a double-release. Wrap it in [`Arc`] for shared
/// ownership.
///
/// The wire type ([`NativeHandleId`]) is safe to include in
/// [`#[message]`](../../istmo_macros/attr.message.html) payloads; the
/// owned wrapper is not encodable and is instead constructed on the
/// Rust side via [`NativeHandle::adopt`] once the raw id is received.
pub struct NativeHandle<T: ?Sized> {
    id: NativeHandleId,
    runtime: Weak<Runtime>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> NativeHandle<T> {
    /// Adopt a raw [`NativeHandleId`] into a typed, drop-releasing
    /// wrapper.
    ///
    /// The `runtime` reference is held weakly so the handle does not
    /// keep the runtime alive on its own.
    #[must_use]
    pub fn adopt(runtime: &Arc<Runtime>, id: NativeHandleId) -> Self {
        Self {
            id,
            runtime: Arc::downgrade(runtime),
            _marker: PhantomData,
        }
    }

    /// The raw [`NativeHandleId`] this wrapper owns.
    #[must_use]
    pub const fn id(&self) -> NativeHandleId {
        self.id
    }

    /// Consume this handle without emitting a release frame.
    ///
    /// Useful when re-shipping the id back through another plugin call
    /// — ownership transfers to whoever receives the id, so the current
    /// wrapper must not fire its [`Drop`] release.
    #[must_use]
    pub const fn into_id(self) -> NativeHandleId {
        let id = self.id;
        std::mem::forget(self);
        id
    }
}

impl<T: ?Sized> std::fmt::Debug for NativeHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHandle")
            .field("id", &self.id)
            .field("type", &std::any::type_name::<T>())
            .finish_non_exhaustive()
    }
}

impl<T: ?Sized> Drop for NativeHandle<T> {
    fn drop(&mut self) {
        let Some(rt) = self.runtime.upgrade() else {
            return;
        };
        drop(rt.release_native_handle(self.id));
    }
}
