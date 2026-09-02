//! Opaque handle to an object owned by the native side.
//!
//! Many platform APIs surface objects that cannot be sensibly serialised over
//! the wire — `androidx.credentials.Credential`, `SecKeyRef`,
//! `android.graphics.Bitmap`, a billing session, an active MIDI endpoint.
//! Rust needs a way to hold a reference to those objects, pass them into
//! subsequent plugin calls and free them deterministically, without ever
//! materialising their contents.
//!
//! The mechanism is a numeric id registered on the native side and wrapped in
//! a [`NativeHandle<T>`] on the Rust side. The generic `T` is a phantom
//! marker — no `T` is ever stored — so plugin authors can distinguish
//! `NativeHandle<Credential>` from `NativeHandle<Bitmap>` at compile time
//! while the wire representation is a single [`NativeHandleId`] (a `u64`).
//!
//! Dropping the handle sends a [`crate::protocol::Frame::ReleaseNativeHandle`]
//! frame so the native registry can free the underlying object. The runtime
//! reference is a [`Weak`] to avoid keeping the runtime alive past its
//! natural lifetime.

use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use crate::protocol::NativeHandleId;
use crate::runtime::Runtime;

/// RAII handle to a native-side object registered under
/// [`NativeHandleId`].
///
/// Held on the Rust side of plugin APIs whose payload cannot be serialised
/// (raw platform objects). The type parameter `T` is a phantom marker so
/// plugin authors can spell `NativeHandle<GoogleCredential>` vs
/// `NativeHandle<Bitmap>` without conflating the two — the wire only carries
/// the [`NativeHandleId`].
///
/// Dropping the handle sends [`crate::protocol::Frame::ReleaseNativeHandle`]
/// so the native side can free the backing object. If the runtime has
/// already gone away the drop is a no-op.
pub struct NativeHandle<T: ?Sized> {
    id: NativeHandleId,
    runtime: Weak<Runtime>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> NativeHandle<T> {
    /// Adopt a `NativeHandleId` received across the wire into a typed handle
    /// tied to `runtime`. The handle takes ownership of the referenced
    /// object — dropping it will fire a `ReleaseNativeHandle` frame.
    #[must_use]
    pub fn adopt(runtime: &Arc<Runtime>, id: NativeHandleId) -> Self {
        Self {
            id,
            runtime: Arc::downgrade(runtime),
            _marker: PhantomData,
        }
    }

    /// Numeric id used on the wire. Handy for logging and for passing the id
    /// back into subsequent plugin calls without moving the handle.
    #[must_use]
    pub const fn id(&self) -> NativeHandleId {
        self.id
    }

    /// Consume the handle and return its `NativeHandleId` without firing a
    /// release frame. Intended for passing ownership through a subsequent
    /// plugin call that will register the same id on its own side.
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
        // Best-effort: a closed outbound channel means the runtime is
        // tearing down and any release would race the shutdown. Swallow the
        // error rather than panic on drop.
        drop(rt.release_native_handle(self.id));
    }
}

// NativeHandle is intentionally *not* `Clone`. Duplicating the id would let
// a stale copy fire an extra release after the primary owner already did,
// which the native registry would treat as a double-free. If callers need
// shared ownership they wrap the handle in an `Arc<NativeHandle<T>>`.
//
// `Send + Sync` fall out of the field types (`NativeHandleId: Copy`,
// `Weak<Runtime>: Send + Sync`, `PhantomData<fn() -> T>: Send + Sync`), so
// no manual `unsafe impl` is required — the workspace forbids `unsafe_code`
// and relying on auto traits here keeps the crate under that rule.

// NativeHandle deliberately implements neither `Encode` nor `Decode`. Wire
// types carry [`NativeHandleId`] (a plain `u64` newtype that IS `Encode +
// Decode`), and the plugin client side calls [`NativeHandle::adopt`] to bind
// the id to the receiving runtime. Enforcing the split at the type level
// keeps ill-formed contracts (a `#[message]` struct with a bare
// `NativeHandle<T>` field) from compiling.
