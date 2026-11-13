---
title: Native handles
description: Hand a native-owned object back to Rust without copying, and free it deterministically.
sidebar:
  order: 4
---

Some plugin results are cheap to reference but expensive to serialize —
an authenticated session, a decoded image, a Bluetooth peripheral. You
want Rust to *hold* the object, hand it back on subsequent calls, and
release it when Rust drops the reference.

Istmo models this as a `NativeHandle<T>` — a strongly-typed wrapper
around a `NativeHandleId` that the native side allocates and Rust
owns.

## The wire type: `NativeHandleId`

`istmo::NativeHandleId` is a `u64` newtype and the only thing that
crosses the wire. It carries no meaning on its own: the side that
allocated it maps it to the real object.

- Kotlin and Swift allocate ids counting up from `1`
  (`IstmoRuntime.allocHandleId`, below).
- A Rust host allocates with `Runtime::alloc_native_handle_id()`, which
  counts up from `NativeHandleId::RUST_ALLOCATED_BASE` (`1 << 63`), so
  the two ranges never collide.

In generated Kotlin and Swift, `NativeHandleId` is a type alias of
`Long` / `UInt64` from the runtime package.

## The Rust type: `NativeHandle<T>`

```rust
pub struct NativeHandle<T: ?Sized> { /* id + Weak<Runtime> + PhantomData<T> */ }
```

- `NativeHandle::adopt(&runtime, id)` binds an id received on the wire
  to the runtime that must release it.
- `id()` reads the id; `into_id(self)` consumes the handle **without**
  releasing (to ship the id back to native code).
- `Drop` sends `Frame::ReleaseNativeHandle { handle_id }`.
- Not `Clone`: duplicating an id would let a stale copy release it a
  second time. Share it as `Arc<NativeHandle<T>>`.
- Not a wire type either: a `#[message]` field holds the raw
  `NativeHandleId`.

`T` is a marker: two handles with different markers cannot be mixed
up, at no runtime cost. Declare it as an uninhabited struct:

```rust
/// Marker for a platform sign-in credential.
pub struct Credential(std::convert::Infallible);
```

## Declaring a plugin that returns a handle

```rust
use istmo::{NativeHandleId, plugin};

pub struct Credential(std::convert::Infallible);

#[plugin(name = "acme.auth")]
pub trait Auth {
    #[istmo::owned]
    async fn sign_in(&self, provider: Provider) -> Result<SignInResult, AuthError>;
    async fn refresh(&self, credential: NativeHandleId)
        -> Result<SignInResult, AuthError>;
}

#[istmo::message]
pub struct SignInResult {
    pub email: String,
    #[handle(Credential)]
    pub credential: NativeHandleId,
}
```

`#[handle(Credential)]` goes on a `NativeHandleId` field of a
named-field `#[istmo::message]` struct (it is rejected on tuple structs
and inside enums). The macro then also emits an owned sibling and a
conversion:

```rust
pub struct OwnedSignInResult {
    pub email: String,
    pub credential: NativeHandle<Credential>,
}

impl SignInResult {
    pub fn into_owned(self, rt: &Arc<Runtime>) -> OwnedSignInResult { /* ... */ }
}
```

## `#[istmo::owned]` client methods

Mark a method whose return carries `#[handle]` fields with
`#[istmo::owned]`, and the client gets a companion `<method>_owned`
next to the raw method:

```rust
// Generated on `AuthClient`:
pub async fn sign_in(&self, provider: Provider)
    -> Result<SignInResult, IstmoError>;
pub async fn sign_in_owned(&self, provider: Provider)
    -> Result<OwnedSignInResult, IstmoError>;
```

Supported return shapes are `T`, `Option<T>` and `Vec<T>` (or a
`Result` of one of them), where `T` is a `#[message]` struct with
`#[handle]` fields. `#[istmo::owned]` is not supported on stream
methods.

Callers prefer `sign_in_owned`: the credential is adopted into a
`NativeHandle` on the way out, so it cannot be leaked.

## Releasing on the native side

Because Rust owns the handle's lifetime, the native backend must free
its object when Rust drops the handle. Allocate ids through the
runtime, which records the plugin that owns each one, and implement
`HandleReleaser`:

```kotlin
// Android
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoRuntime

class AuthBackendImpl(private val ctx: Context) : AuthBackend, HandleReleaser {
    private val credentials = ConcurrentHashMap<Long, Credential>()

    override suspend fun sign_in(provider: Provider): SignInResult {
        val credential = /* … platform sign-in … */
        val id = IstmoRuntime.allocHandleId(AuthDispatcher.PLUGIN_ID)
        credentials[id] = credential
        return SignInResult(email = credential.email, credential = id)
    }

    override fun releaseNativeHandle(handleId: Long) {
        credentials.remove(handleId)?.close()
    }
}
```

```swift
// iOS
import IstmoRuntime

final class AuthBackendImpl: AuthBackend, HandleReleaser {
    private var credentials: [UInt64: Credential] = [:]

    func sign_in(provider: Provider) async throws -> SignInResult {
        let credential = /* … platform sign-in … */
        let id = IstmoRuntime.shared.allocHandleId(pluginId: AuthDispatcher.PLUGIN_ID)
        credentials[id] = credential
        return SignInResult(email: credential.email, credential: id)
    }

    func releaseNativeHandle(_ handleId: UInt64) {
        credentials.removeValue(forKey: handleId)?.invalidate()
    }
}
```

On `Frame::ReleaseNativeHandle` the runtime looks up the owner, drops
the record, and calls the plugin's generated dispatcher, which forwards
to the backend when it implements `HandleReleaser`. Unknown ids are
ignored. A backend that discards an object before Rust ever saw its id
calls `IstmoRuntime.forgetHandle(id)` to drop the ownership record.

## Round-tripping a handle back

To call a native method that consumes the handle again (e.g. a refresh
call), pass the raw id:

```rust
let owned = auth.sign_in_owned(provider).await?;
let refreshed = auth.refresh(owned.credential.into_id()).await?;
```

`into_id` consumes the `NativeHandle` **without firing release**, so the
native side now owns that id's lifetime again; release or reuse it in
the `refresh` implementation.

## Rules of thumb

- Prefer `_owned` client methods so `Drop` handles cleanup for you.
- To share a handle, wrap it in `Arc<NativeHandle<T>>` rather than
  trying to make `NativeHandle` clone.
- Do not expose a bare `NativeHandleId` in your crate's public Rust
  API beyond the trait — it is a wire type with no lifetime
  guarantees.

## Next

- The wire variant: [Frame protocol → `ReleaseNativeHandle`](/istmo/concepts/frame-protocol/).
- Real usage: [`google-sign-in` plugin](/istmo/plugins/google-sign-in/).
