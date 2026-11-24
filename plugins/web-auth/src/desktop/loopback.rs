//! Loopback callback (RFC 8252 §7.3): a one-shot HTTP listener on
//! `127.0.0.1` and an ephemeral port.
//!
//! Only the exact callback path carrying the expected `state` ends the
//! wait; anything else (a probe, a stale tab, another local process
//! guessing the port) gets an error page and the listener keeps
//! waiting. The success page never echoes the parameters.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::time::Duration;

use istmo_core::CancelToken;

use super::{Deadline, check};
use crate::{VerifiedCallback, WebAuthError};

/// How often the accept loop looks at the cancel token.
const POLL: Duration = Duration::from_millis(50);
/// Per-connection read budget.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest request head accepted.
const MAX_HEAD: usize = 16 * 1024;

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Signed in</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Signed in</h1>\
<p>You can close this tab and return to the app.</p>";
const REJECTED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Not this sign-in</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Not this sign-in</h1>\
<p>The app is not waiting for this request.</p>";

/// A bound, waiting listener.
#[derive(Debug)]
pub(super) struct Listener {
    inner: TcpListener,
    port: u16,
    path: String,
}

impl Listener {
    pub(super) fn bind(path: &str) -> Result<Self, WebAuthError> {
        let path = if path.is_empty() { "/" } else { path };
        if !path.starts_with('/') || path.contains(['?', '#', ' ', '\\']) {
            return Err(WebAuthError::InvalidArgument(format!(
                "loopback path `{path}`"
            )));
        }
        // IPv4 literal: `localhost` may resolve elsewhere and some
        // providers (Microsoft) reject `[::1]`.
        let inner = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|err| WebAuthError::Backend(format!("loopback listener: {err}")))?;
        inner
            .set_nonblocking(true)
            .map_err(|err| WebAuthError::Backend(err.to_string()))?;
        let port = inner
            .local_addr()
            .map_err(|err| WebAuthError::Backend(err.to_string()))?
            .port();
        Ok(Self {
            inner,
            port,
            path: path.to_owned(),
        })
    }

    pub(super) fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.path)
    }

    /// Accept until the right callback arrives.
    pub(super) fn wait(
        &self,
        state: &str,
        cancel: &CancelToken,
        deadline: Deadline,
    ) -> Result<String, WebAuthError> {
        loop {
            check(cancel, deadline)?;
            match self.inner.accept() {
                Ok((stream, _)) => {
                    if let Some(url) = self.serve(stream, state) {
                        return Ok(url);
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(deadline.remaining(POLL));
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => return Err(WebAuthError::Backend(format!("loopback accept: {err}"))),
            }
        }
    }

    /// Answer one connection; `Some(url)` when it was the callback.
    fn serve(&self, mut stream: TcpStream, state: &str) -> Option<String> {
        let target = match read_request_target(&mut stream) {
            Ok(target) => target,
            Err(status) => {
                respond(&mut stream, status, REJECTED_PAGE);
                return None;
            }
        };
        let path = target.split(['?', '#']).next().unwrap_or_default();
        if path != self.path {
            respond(&mut stream, "404 Not Found", REJECTED_PAGE);
            return None;
        }
        let url = format!("http://127.0.0.1:{}{target}", self.port);
        if VerifiedCallback::verify(&url, state, None).is_err() {
            respond(&mut stream, "400 Bad Request", REJECTED_PAGE);
            return None;
        }
        respond(&mut stream, "200 OK", DONE_PAGE);
        Some(url)
    }
}

/// Request target of a `GET`, or the status to answer with.
fn read_request_target(stream: &mut TcpStream) -> Result<String, &'static str> {
    stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(READ_TIMEOUT)))
        .map_err(|_| "500 Internal Server Error")?;
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0_u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).map_err(|_| "408 Request Timeout")?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > MAX_HEAD {
            return Err("431 Request Header Fields Too Large");
        }
    }
    let line = head
        .split(|&b| b == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .ok_or("400 Bad Request")?;
    let mut parts = line.trim_end().split(' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("GET"), Some(target), Some(version))
            if target.starts_with('/') && version.starts_with("HTTP/1.") =>
        {
            Ok(target.to_owned())
        }
        (Some("GET"), _, _) => Err("400 Bad Request"),
        _ => Err("405 Method Not Allowed"),
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser may already have gone; nothing to do about it.
    drop(stream.write_all(response.as_bytes()));
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::*;

    fn get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn only_the_right_callback_ends_the_wait() {
        let listener = Listener::bind("/cb").unwrap();
        let uri = listener.redirect_uri();
        assert!(uri.starts_with("http://127.0.0.1:") && uri.ends_with("/cb"));
        let port = listener.port;
        let client = std::thread::spawn(move || {
            let probe = get(port, "/other?state=s1");
            let wrong = get(port, "/cb?code=x&state=nope");
            let right = get(port, "/cb?code=x&state=s1");
            (probe, wrong, right)
        });
        let url = listener
            .wait("s1", &CancelToken::new(), Deadline::new(10_000))
            .unwrap();
        let (probe, wrong, right) = client.join().unwrap();
        assert!(probe.starts_with("HTTP/1.1 404"));
        assert!(wrong.starts_with("HTTP/1.1 400"));
        assert!(right.starts_with("HTTP/1.1 200"));
        assert!(!right.contains("code=x"), "parameters are not echoed");
        assert!(url.ends_with("/cb?code=x&state=s1"));
    }

    #[test]
    fn cancel_and_timeout_end_the_wait() {
        let listener = Listener::bind("/cb").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            listener.wait("s", &cancel, Deadline::new(0)).unwrap_err(),
            WebAuthError::Cancelled
        );
        assert_eq!(
            listener
                .wait("s", &CancelToken::new(), Deadline::new(1))
                .unwrap_err(),
            WebAuthError::Timeout
        );
    }

    #[test]
    fn rejects_bad_paths() {
        for bad in ["cb", "/cb?x", "/c b", "/c\\b"] {
            assert!(Listener::bind(bad).is_err(), "{bad}");
        }
    }
}
