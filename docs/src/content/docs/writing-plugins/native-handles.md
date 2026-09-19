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

```rust
pub struct NativeHandleId(u64);
```

`NativeHandleId` is the only thing that crosses the wire — a bare `u64`
carrying no semantic meaning on its own. Kotlin/Swift generate ids from
a per-runtime monotonic counter.

## The Rust type: `NativeHandle<T>`

```rust
pub struct NativeHandle<T: ?Sized> { /* id + Weak<Runtime> */ }
```

- Non-`Clone` (duplicating an id would let a second `drop` fire).
- Implements `Drop` → sends `Frame::ReleaseNativeHandle { handle_id }`.
- `into_id(self) -> NativeHandleId` consumes without releasing (useful
  when you want to re-ship the id back to native code).
- The `T` phantom is a marker (e.g. `Credential`, `DecodedImage`) — no
  runtime cost, but two handles with different `T`s cannot be mixed
  by accident.

## Declaring a plugin that returns a handle

```rust
#[plugin]
pub trait Auth {
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

The `#[handle(Credential)]` attribute tells the `#[istmo::message]`
macro to also emit an "owned" sibling struct where the raw
`NativeHandleId` becomes a `NativeHandle<Credential>`:

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

If a trait method's return contains a `#[handle]` field, mark the
method with `#[istmo::owned]` and the generated client emits a
companion `<method>_owned` returning the owned struct:

```rust
#[plugin]
pub trait Auth {
    #[istmo::owned]
    async fn sign_in(&self, provider: Provider) -> Result<SignInResult, AuthError>;
}

// Generated on the client — plus the raw form for advanced use.
impl AuthClient {
    pub async fn sign_in_owned(&self, provider: Provider)
        -> Result<OwnedSignInResult, AuthError>;
}
```

Callers prefer `sign_in_owned` because the credential is adopted into
a `NativeHandle` on the way out — you cannot forget to release it.

## Registering a release callback on the native side

Because Rust owns the handle's lifetime, the native side must respond
to `Frame::ReleaseNativeHandle` by tearing down its cached object:

```kotlin
// Android
IstmoRuntime.instance.registerReleaser("acme.auth.credential") { id ->
    credentialCache.remove(id)?.close()
}
```

```swift
// iOS
IstmoRuntime.shared.register(
    handleOwner: "acme.auth.credential",
    releaser: { id in credentialCache.removeValue(forKey: id)?.invalidate() },
)
```

The runtime handles ID allocation and dispatch — you only supply the
releaser closure.

## Round-tripping a handle back

To call a native method that consumes the handle again (e.g. a refresh
call), pass the raw id:

```rust
let owned = auth.sign_in_owned(provider).await?;
let refreshed = auth.refresh(owned.credential.into_id()).await?;
```

`into_id` consumes the `NativeHandle` **without firing release**. Once
`refresh` returns, the native side re-allocates a fresh id for the new
credential.

## Rules of thumb

- Prefer `_owned` client methods so `Drop` handles cleanup for you.
- If you clone data across `Arc<T>` boundaries, wrap the handle in
  `Arc<NativeHandle<T>>` rather than trying to make `NativeHandle`
  clone.
- Never expose a bare `NativeHandleId` in a plugin's public API — it
  is a wire type and gives you no lifetime guarantees.

## Next

- The wire variant: [Frame protocol → `ReleaseNativeHandle`](/concepts/frame-protocol/).
- Real usage: [`google-sign-in` plugin](/plugins/google-sign-in/).
