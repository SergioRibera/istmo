---
title: Local endpoint
description: Request/reply messages between desktop processes of the same user, with one owner per name.
sidebar:
  order: 4
---

`istmo::plugins::LocalEndpoint` connects processes of the same user on
the same machine (Linux, macOS, Windows). Only one process at a time
owns a name and serves requests on it; every other process sees the name
as taken and sends requests to the owner. Typical uses:

- a single-instance app that takes commands from later launches;
- a CLI that drives the running GUI;
- a helper process that hands its results back to the app.

Single-instance document forwarding
([`DocumentsPublisher`](/istmo/plugins/built-in/#documents)) runs on
this same endpoint, in a namespace of its own: an app can use its bundle
id as an endpoint name without clashing with it.

## Claim, serve, request

```rust
use istmo::plugins::{EndpointClaim, EndpointName, LocalEndpoint};

let endpoint = LocalEndpoint::from(EndpointName::try_from("rs.example.app.control")?);
match endpoint.claim()? {
    EndpointClaim::Owner(owned) => {
        // Serves until `server` is dropped.
        let server = owned.serve(|request: Vec<u8>| {
            Some(handle(request)) // reply body; `None` closes without replying
        })?;
        run_app(server);
    }
    EndpointClaim::Taken => {
        let reply = endpoint.request(b"open-window")?;
        // …
    }
}
```

- `EndpointName` takes 1–64 ASCII letters, digits, `.`, `-` and `_`.
  All of a user's programs share the namespace, so use a reverse-DNS
  name.
- `claim()` returns `Owner(OwnedEndpoint)` or `Taken`. Holding an
  `OwnedEndpoint` without calling `serve` works as a plain
  single-instance lock: requests to the name time out.
- `serve(handler)` runs the handler on a thread per request and returns
  a `LocalServer`. `LocalServer::is_serving()` turns `false` if the
  endpoint is lost (for example, its lock file was deleted). Dropping the
  server stops it and frees the name.
- `request(body)` connects, retrying for about two seconds while the
  owner is not listening yet, sends the body and waits for the reply.

Messages are opaque bytes. To send types, encode them with
`istmo::codec` (`codec::encode` / `codec::decode_exact_capped`) or any other
format. The handler is given untrusted input: decode it with a size cap.

## Limits

`LocalEndpoint` methods change the defaults. The owner and the
requesters should use the same values:

| Method | Default | Effect |
|---|---|---|
| `with_max_message_len(u32)` | 1 MiB | largest request or reply body; each end rejects bigger frames |
| `with_max_clients(usize)` | 8 | requests served at once; on Unix the rest wait in the kernel queue, on Windows they are dropped |
| `with_request_budget(Duration)` | 1.5 s | (Unix) time the owner gives a client to send its whole request; the clock restarts before the reply |
| `with_reply_wait(Duration)` | 5 s | how long `request` waits for the reply, handler run time included |

## Errors

`EndpointError` (`#[non_exhaustive]`):

| Variant | When |
|---|---|
| `InvalidName` | the name does not pass validation |
| `Io { op, source }` | an OS step failed (runtime directory, pipe, socket) |
| `NotListening` | the name is owned but nobody accepted the request in time |
| `NoReply` | the owner closed the connection or did not reply in time |
| `ForeignOwner` | the endpoint belongs to another user |
| `TooLarge { len, max }` | a body exceeds `with_max_message_len` |
| `Protocol` | the peer sent a malformed frame |

## How it works

- **Linux / macOS** — a Unix domain socket next to an `flock`ed lock
  file, in a directory only the user can enter (`$XDG_RUNTIME_DIR`, the
  per-user temp dir, or a private `istmo-<uid>` directory under the temp
  dir). The lock decides who owns the name, so two simultaneous claims
  never both win, and a crashed owner leaves nothing stale behind. The
  server checks the peer's uid (`SO_PEERCRED` / `getpeereid`) and binds
  the socket again if a temp cleaner removes it.
- **Windows** — a named pipe `\\.\pipe\istmo-<hash>-<SID>-<session>`
  created with `FILE_FLAG_FIRST_PIPE_INSTANCE` (atomic ownership),
  `PIPE_REJECT_REMOTE_CLIENTS` and a DACL for the user and `SYSTEM`.
  Each end checks that the other process runs as the same user.

Each message travels in a frame: the magic `ISTL`, a version byte, a
`u32` little-endian length, then the body.

Other users never reach the endpoint. Processes of the same user can
reach each other by design: if the handler does something sensitive,
authenticate the request inside the message.
