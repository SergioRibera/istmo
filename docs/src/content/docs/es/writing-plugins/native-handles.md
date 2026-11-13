---
title: Native handles
description: Pasa un objeto propiedad del lado nativo a Rust sin copiarlo, y libéralo de forma determinista.
sidebar:
  order: 4
---

Algunos resultados de plugins son baratos de referenciar pero caros de
serializar — una sesión autenticada, una imagen decodificada, un
periférico Bluetooth. Quieres que Rust *sostenga* el objeto, lo
devuelva en llamadas posteriores y lo libere cuando suelte la
referencia.

Istmo lo modela como un `NativeHandle<T>` — un wrapper fuertemente
tipado alrededor de un `NativeHandleId` que el lado nativo asigna y
Rust posee.

## El tipo del wire: `NativeHandleId`

`istmo::NativeHandleId` es un newtype de `u64` y lo único que cruza el
wire. No significa nada por sí solo: el lado que lo asignó lo mapea al
objeto real.

- Kotlin y Swift asignan ids contando desde `1`
  (`IstmoRuntime.allocHandleId`, más abajo).
- Un host Rust asigna con `Runtime::alloc_native_handle_id()`, que
  cuenta desde `NativeHandleId::RUST_ALLOCATED_BASE` (`1 << 63`), así
  que los dos rangos nunca chocan.

En Kotlin y Swift generados, `NativeHandleId` es un alias de tipo de
`Long` / `UInt64` del paquete del runtime.

## El tipo Rust: `NativeHandle<T>`

```rust
pub struct NativeHandle<T: ?Sized> { /* id + Weak<Runtime> + PhantomData<T> */ }
```

- `NativeHandle::adopt(&runtime, id)` liga un id recibido por el wire
  al runtime que debe liberarlo.
- `id()` lee el id; `into_id(self)` consume el handle **sin** liberarlo
  (para devolver el id a código nativo).
- `Drop` envía `Frame::ReleaseNativeHandle { handle_id }`.
- No es `Clone`: duplicar un id permitiría que una copia vieja lo
  libere por segunda vez. Compártelo como `Arc<NativeHandle<T>>`.
- Tampoco es un tipo del wire: un campo de `#[message]` guarda el
  `NativeHandleId` crudo.

`T` es un marcador: dos handles con marcadores distintos no se pueden
mezclar, sin costo en runtime. Decláralo como un struct sin valores
posibles:

```rust
/// Marcador de una credencial de inicio de sesión de la plataforma.
pub struct Credential(std::convert::Infallible);
```

## Declarar un plugin que retorna un handle

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

`#[handle(Credential)]` va en un campo `NativeHandleId` de un struct
`#[istmo::message]` con campos con nombre (se rechaza en tuple structs
y dentro de enums). La macro emite además un sibling owned y una
conversión:

```rust
pub struct OwnedSignInResult {
    pub email: String,
    pub credential: NativeHandle<Credential>,
}

impl SignInResult {
    pub fn into_owned(self, rt: &Arc<Runtime>) -> OwnedSignInResult { /* ... */ }
}
```

## Métodos de cliente `#[istmo::owned]`

Marca con `#[istmo::owned]` un método cuyo retorno lleva campos
`#[handle]`, y el cliente obtiene un `<método>_owned` junto al método
crudo:

```rust
// Generado en `AuthClient`:
pub async fn sign_in(&self, provider: Provider)
    -> Result<SignInResult, IstmoError>;
pub async fn sign_in_owned(&self, provider: Provider)
    -> Result<OwnedSignInResult, IstmoError>;
```

Las formas de retorno admitidas son `T`, `Option<T>` y `Vec<T>` (o un
`Result` de alguna de ellas), donde `T` es un struct `#[message]` con
campos `#[handle]`. `#[istmo::owned]` no se admite en métodos stream.

Los callers prefieren `sign_in_owned`: la credencial se adopta en un
`NativeHandle` al salir, así que no se puede filtrar.

## Liberar del lado nativo

Como Rust es dueño de la vida del handle, el backend nativo debe
liberar su objeto cuando Rust suelta el handle. Asigna los ids a
través del runtime, que registra qué plugin es dueño de cada uno, e
implementa `HandleReleaser`:

```kotlin
// Android
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoRuntime

class AuthBackendImpl(private val ctx: Context) : AuthBackend, HandleReleaser {
    private val credentials = ConcurrentHashMap<Long, Credential>()

    override suspend fun sign_in(provider: Provider): SignInResult {
        val credential = /* … inicio de sesión de la plataforma … */
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
        let credential = /* … inicio de sesión de la plataforma … */
        let id = IstmoRuntime.shared.allocHandleId(pluginId: AuthDispatcher.PLUGIN_ID)
        credentials[id] = credential
        return SignInResult(email: credential.email, credential: id)
    }

    func releaseNativeHandle(_ handleId: UInt64) {
        credentials.removeValue(forKey: handleId)?.invalidate()
    }
}
```

Ante un `Frame::ReleaseNativeHandle` el runtime busca al dueño, borra
el registro y llama al dispatcher generado del plugin, que lo reenvía
al backend cuando implementa `HandleReleaser`. Los ids desconocidos se
ignoran. Un backend que descarta un objeto antes de que Rust viera su
id llama a `IstmoRuntime.forgetHandle(id)` para borrar el registro de
propiedad.

## Devolver un handle

Para llamar a un método nativo que vuelve a consumir el handle (p. ej.
un refresh), pasa el id crudo:

```rust
let owned = auth.sign_in_owned(provider).await?;
let refreshed = auth.refresh(owned.credential.into_id()).await?;
```

`into_id` consume el `NativeHandle` **sin disparar el release**, así
que el lado nativo vuelve a ser dueño de la vida de ese id; libéralo o
reúsalo en la implementación de `refresh`.

## Reglas prácticas

- Prefiere los métodos `_owned` del cliente para que `Drop` se encargue
  de la limpieza.
- Para compartir un handle, envuélvelo en `Arc<NativeHandle<T>>` en vez
  de intentar hacer `NativeHandle` clonable.
- No expongas un `NativeHandleId` suelto en la API Rust pública de tu
  crate más allá del trait — es un tipo del wire sin garantías de vida.

## Siguiente

- La variante del wire: [Protocolo de frames → `ReleaseNativeHandle`](/istmo/es/concepts/frame-protocol/).
- Uso real: [plugin `google-sign-in`](/istmo/es/plugins/google-sign-in/).
