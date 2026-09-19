---
title: Handles nativos
description: Devuélvele a Rust un objeto native-owned sin copiar, y libéralo determinísticamente.
sidebar:
  order: 4
---

Algunos resultados de plugin son baratos de referenciar pero caros de
serializar — una sesión autenticada, una imagen decodificada, un
periférico Bluetooth. Quieres que Rust *sostenga* el objeto, lo
devuelva en calls subsiguientes, y lo libere cuando Rust dropea la
referencia.

Istmo modela esto como `NativeHandle<T>` — un wrapper tipado alrededor
de un `NativeHandleId` que el lado nativo asigna y Rust posee.

## El tipo del wire: `NativeHandleId`

```rust
pub struct NativeHandleId(u64);
```

`NativeHandleId` es lo único que cruza el wire — un `u64` desnudo sin
significado semántico por sí solo. Kotlin/Swift generan ids desde un
contador monotónico per-runtime.

## El tipo Rust: `NativeHandle<T>`

```rust
pub struct NativeHandle<T: ?Sized> { /* id + Weak<Runtime> */ }
```

- No es `Clone` (duplicar un id dejaría que un segundo `drop` disparara).
- Implementa `Drop` → envía `Frame::ReleaseNativeHandle { handle_id }`.
- `into_id(self) -> NativeHandleId` consume sin liberar (útil cuando
  quieres re-shippear el id de vuelta al código nativo).
- El fantasma `T` es un marker (ej. `Credential`, `DecodedImage`) —
  costo runtime cero, pero dos handles con `T`s distintos no pueden
  mezclarse por accidente.

## Declarar un plugin que retorna un handle

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

El atributo `#[handle(Credential)]` le dice a la macro
`#[istmo::message]` que también emita un struct hermano "owned" donde
el `NativeHandleId` crudo se convierte en un
`NativeHandle<Credential>`:

```rust
pub struct OwnedSignInResult {
    pub email: String,
    pub credential: NativeHandle<Credential>,
}

impl SignInResult {
    pub fn into_owned(self, rt: &Arc<Runtime>) -> OwnedSignInResult { /* ... */ }
}
```

## Métodos cliente `#[istmo::owned]`

Si un método del trait retorna algo que contiene un campo `#[handle]`,
marca el método con `#[istmo::owned]` y el cliente generado emite un
compañero `<method>_owned` que retorna el struct owned:

```rust
#[plugin]
pub trait Auth {
    #[istmo::owned]
    async fn sign_in(&self, provider: Provider) -> Result<SignInResult, AuthError>;
}

// Generado en el cliente — más la forma raw para uso avanzado.
impl AuthClient {
    pub async fn sign_in_owned(&self, provider: Provider)
        -> Result<OwnedSignInResult, AuthError>;
}
```

Los callers prefieren `sign_in_owned` porque el credential se adopta
dentro de un `NativeHandle` a la salida — no puedes olvidarte de
liberarlo.

## Registrar un callback de release del lado nativo

Como Rust posee el lifetime del handle, el lado nativo debe responder
a `Frame::ReleaseNativeHandle` tirando abajo su objeto cacheado:

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

El runtime maneja la asignación de ID y el dispatch — tú sólo provees
la closure de release.

## Round-trip de un handle

Para llamar un método nativo que consume el handle de nuevo (ej. una
llamada de refresh), pasa el id crudo:

```rust
let owned = auth.sign_in_owned(provider).await?;
let refreshed = auth.refresh(owned.credential.into_id()).await?;
```

`into_id` consume el `NativeHandle` **sin disparar release**. Una vez
que `refresh` retorna, el lado nativo re-asigna un id fresco para el
nuevo credential.

## Reglas de dedo

- Prefiere métodos cliente `_owned` para que `Drop` maneje la limpieza
  por ti.
- Si clonás datos a través de fronteras `Arc<T>`, envuelve el handle
  en `Arc<NativeHandle<T>>` en vez de intentar hacer clonable
  `NativeHandle`.
- Nunca expongas un `NativeHandleId` desnudo en la API pública de un
  plugin — es un tipo del wire y no te da garantías de lifetime.

## Siguiente

- La variante del wire: [Frame protocol → `ReleaseNativeHandle`](/es/concepts/frame-protocol/).
- Uso real: [plugin `google-sign-in`](/es/plugins/google-sign-in/).
