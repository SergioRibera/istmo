---
title: Endpoint local
description: Mensajes de petición y respuesta entre procesos de escritorio del mismo usuario, con un dueño por nombre.
sidebar:
  order: 4
---

`istmo::plugins::LocalEndpoint` conecta procesos del mismo usuario en la
misma máquina (Linux, macOS, Windows). Un nombre tiene un solo dueño a la
vez, que atiende las peticiones; para cualquier otro proceso el nombre
está ocupado y le manda sus peticiones al dueño. Usos típicos:

- una app de instancia única que recibe órdenes de los arranques
  posteriores;
- una CLI que controla la GUI en ejecución;
- un proceso auxiliar que le devuelve resultados a la app.

El reenvío de documentos de instancia única
([`DocumentsPublisher`](/istmo/es/plugins/built-in/#documentos)) usa este
mismo endpoint, en un espacio de nombres propio: puedes usar el bundle id
de tu app como nombre de endpoint sin chocar con él.

## Reclamar, servir, pedir

```rust
use istmo::plugins::{EndpointClaim, EndpointName, LocalEndpoint};

let endpoint = LocalEndpoint::from(EndpointName::try_from("rs.example.app.control")?);
match endpoint.claim()? {
    EndpointClaim::Owner(owned) => {
        // Atiende hasta que sueltas `server`.
        let server = owned.serve(|request: Vec<u8>| {
            Some(handle(request)) // cuerpo de la respuesta; `None` cierra sin responder
        })?;
        run_app(server);
    }
    EndpointClaim::Taken => {
        let reply = endpoint.request(b"open-window")?;
        // …
    }
}
```

- `EndpointName` acepta de 1 a 64 letras ASCII, dígitos, `.`, `-` y `_`.
  Todos los programas del usuario comparten el espacio de nombres: usa
  un nombre en DNS inverso.
- `claim()` devuelve `Owner(OwnedEndpoint)` o `Taken`. Guardar un
  `OwnedEndpoint` sin llamar a `serve` funciona como un lock de
  instancia única: las peticiones al nombre expiran.
- `serve(handler)` corre el handler en un hilo por petición y devuelve
  un `LocalServer`. `LocalServer::is_serving()` pasa a `false` si el
  endpoint se pierde (por ejemplo, alguien borró su archivo de lock).
  Soltar el servidor lo detiene y libera el nombre.
- `request(body)` se conecta (reintenta unos dos segundos mientras el
  dueño todavía no escucha), envía el cuerpo y espera la respuesta.

Los mensajes son bytes opacos. Para mandar tipos, codifícalos con
`istmo::codec` (`codec::encode` / `codec::decode_exact_capped`) o con el
formato que prefieras. Lo que recibe el handler no es de confianza:
decodifícalo con un tope de tamaño.

## Límites

Los métodos de `LocalEndpoint` cambian los valores por defecto. El dueño
y quienes le piden deben usar los mismos:

| Método | Por defecto | Efecto |
|---|---|---|
| `with_max_message_len(u32)` | 1 MiB | cuerpo máximo de petición o respuesta; cada extremo rechaza frames más grandes |
| `with_max_clients(usize)` | 8 | peticiones atendidas a la vez; en Unix el resto espera en la cola del kernel, en Windows se descartan |
| `with_request_budget(Duration)` | 1,5 s | (Unix) tiempo que el dueño le da a un cliente para enviar toda la petición; se reinicia antes de la respuesta |
| `with_reply_wait(Duration)` | 5 s | cuánto espera `request` la respuesta, incluido el tiempo del handler |

## Errores

`EndpointError` (`#[non_exhaustive]`):

| Variante | Cuándo |
|---|---|
| `InvalidName` | el nombre no pasa la validación |
| `Io { op, source }` | falló un paso del SO (directorio de runtime, pipe, socket) |
| `NotListening` | el nombre tiene dueño, pero nadie aceptó la petición a tiempo |
| `NoReply` | el dueño cerró la conexión o no respondió a tiempo |
| `ForeignOwner` | el endpoint es de otro usuario |
| `TooLarge { len, max }` | un cuerpo supera `with_max_message_len` |
| `Protocol` | el otro extremo mandó un frame mal formado |

## Cómo funciona

- **Linux / macOS**: un socket de dominio Unix junto a un archivo de lock
  con `flock`, en un directorio al que solo entra el usuario
  (`$XDG_RUNTIME_DIR`, el directorio temporal del usuario o un directorio
  privado `istmo-<uid>` dentro del temporal). El lock decide quién es el
  dueño del nombre: dos reclamos simultáneos nunca ganan los dos, y un
  dueño que se cae no deja nada colgado. El servidor verifica el uid del
  otro extremo (`SO_PEERCRED` / `getpeereid`) y vuelve a crear el socket
  si un limpiador de temporales lo borra.
- **Windows**: un named pipe `\\.\pipe\istmo-<hash>-<SID>-<sesión>`
  creado con `FILE_FLAG_FIRST_PIPE_INSTANCE` (propiedad atómica),
  `PIPE_REJECT_REMOTE_CLIENTS` y una DACL para el usuario y `SYSTEM`.
  Cada extremo verifica que el otro proceso corre como el mismo usuario.

Cada mensaje viaja en un frame: el magic `ISTL`, un byte de versión, el
largo en `u32` little-endian y después el cuerpo.

Otros usuarios nunca llegan al endpoint. Los procesos del mismo usuario
sí pueden alcanzarse entre sí: si el handler hace algo sensible,
autentica la petición dentro del mensaje.
