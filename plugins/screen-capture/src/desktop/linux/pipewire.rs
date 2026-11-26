//! PipeWire plumbing shared by portal video and audio: one main loop on
//! its own thread per session, the connection to the daemon, and SPA pod
//! serialisation.
//!
//! Every PipeWire object lives on its loop thread; other threads only
//! talk to it through `pipewire::channel` (terminate, wake-ups), which is
//! what makes the `Rc`-based PipeWire types usable from the plugin's
//! `Send` sessions.

use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::sync::Once;
use std::thread::{JoinHandle, ThreadId};
use std::time::Duration;

use pipewire as pw;
use pw::spa::pod::{Object, Pod, Value, serialize::PodSerializer};

use crate::types::{BackendDetail, BackendId, CaptureError};

/// A `Protocol` error of `backend` (`None` for audio, which is not a
/// capture backend of its own).
pub(crate) fn error(
    backend: Option<BackendId>,
    step: &str,
    message: impl std::fmt::Display,
) -> CaptureError {
    CaptureError::Protocol(BackendDetail {
        backend,
        step: step.to_owned(),
        message: message.to_string(),
    })
}

/// Which PipeWire daemon a session's streams live on.
#[derive(Debug)]
pub(crate) enum Remote {
    /// The user's session daemon (`Context::connect(None)`).
    SessionDaemon,
    /// A private remote, e.g. the portal's `OpenPipeWireRemote` fd.
    #[cfg_attr(
        not(feature = "linux-portal"),
        allow(dead_code, reason = "only the portal opens private remotes")
    )]
    Fd(OwnedFd),
}

/// `pw_init` once per process.
fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(pw::init);
}

/// Serialise a SPA object into pod bytes.
///
/// # Errors
/// `Protocol` naming `step` when the serializer refuses the object.
pub(crate) fn serialize(
    backend: Option<BackendId>,
    step: &str,
    object: Object,
) -> Result<Vec<u8>, CaptureError> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|err| error(backend, step, format!("SPA pod: {err:?}")))
}

/// The `&Pod` view of serialised pod bytes.
///
/// # Errors
/// `Protocol` when the bytes are not a pod.
pub(crate) fn pod(backend: Option<BackendId>, bytes: &[u8]) -> Result<&Pod, CaptureError> {
    Pod::from_bytes(bytes).ok_or_else(|| error(backend, "SPA pod", "not a valid pod"))
}

/// Sent to the loop thread to stop it.
struct Terminate;

/// What a loop body gets: its main loop and core, ready to build
/// streams on. [`LoopCtx::run`] runs the loop until the session ends.
pub(crate) struct LoopCtx {
    pub(crate) mainloop: pw::main_loop::MainLoopRc,
    pub(crate) core: pw::core::CoreRc,
    terminate: pw::channel::Receiver<Terminate>,
}

impl LoopCtx {
    /// Run the loop until [`LoopThread`] is dropped or the body quits it.
    pub(crate) fn run(self) {
        let Self {
            mainloop,
            core,
            terminate,
        } = self;
        let _attached = terminate.attach(mainloop.loop_(), {
            let mainloop = mainloop.clone();
            move |_| mainloop.quit()
        });
        mainloop.run();
        drop(core);
    }
}

/// How long [`LoopThread::spawn`] waits for a body that only builds
/// streams (no dialog in between).
pub(crate) const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// A PipeWire main loop on its own thread. Dropping it stops the loop
/// and joins the thread (unless dropped on that thread).
/// How long dropping a [`LoopThread`] waits for the loop to end before it
/// detaches the thread.
const JOIN_WAIT: Duration = Duration::from_secs(2);

pub(crate) struct LoopThread {
    terminate: pw::channel::Sender<Terminate>,
    join: Option<JoinHandle<()>>,
    thread: ThreadId,
    /// Disconnected once the loop thread's body returned.
    done: flume::Receiver<()>,
}

impl std::fmt::Debug for LoopThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopThread")
            .field("thread", &self.thread)
            .finish_non_exhaustive()
    }
}

impl LoopThread {
    /// Start a loop thread connected to `remote` and run `body` on it.
    /// The body builds its streams, reports once through `ready` (the
    /// negotiated stream, the screenshot frames, ...), then usually calls
    /// [`LoopCtx::run`]. Returns once it reported.
    ///
    /// # Errors
    /// The body's error; `Protocol` when PipeWire cannot be reached, the
    /// thread cannot start, or the body neither reports nor exits within
    /// `timeout`.
    pub(crate) fn spawn<T, F>(
        backend: Option<BackendId>,
        name: &str,
        remote: Remote,
        timeout: Duration,
        body: F,
    ) -> Result<(Self, T), CaptureError>
    where
        T: Send + 'static,
        F: FnOnce(LoopCtx, &flume::Sender<Result<T, CaptureError>>) + Send + 'static,
    {
        init();
        let (terminate, terminate_rx) = pw::channel::channel::<Terminate>();
        let (ready_tx, ready_rx) = flume::bounded(1);
        let (done_tx, done) = flume::bounded::<()>(0);
        let join = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _done = done_tx;
                match connect(backend, remote) {
                    Ok((mainloop, core)) => body(
                        LoopCtx {
                            mainloop,
                            core,
                            terminate: terminate_rx,
                        },
                        &ready_tx,
                    ),
                    Err(err) => {
                        // The caller gave up waiting when this fails.
                        let _unwanted = ready_tx.send(Err(err));
                    }
                }
            })
            .map_err(|err| error(backend, "spawn the PipeWire thread", err))?;
        let thread = join.thread().id();
        let this = Self {
            terminate,
            join: Some(join),
            thread,
            done,
        };
        match ready_rx.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok((this, value)),
            Ok(Err(err)) => Err(err),
            Err(flume::RecvTimeoutError::Timeout) => Err(error(
                backend,
                "PipeWire negotiation",
                format!("no answer within {timeout:?}"),
            )),
            Err(flume::RecvTimeoutError::Disconnected) => Err(error(
                backend,
                "PipeWire negotiation",
                "the PipeWire thread ended without an answer",
            )),
        }
    }
}

impl Drop for LoopThread {
    fn drop(&mut self) {
        // Fails only when the loop already ended.
        let _ended = self.terminate.send(Terminate);
        if std::thread::current().id() == self.thread {
            return;
        }
        // Bounded: a wedged PipeWire loop is detached, never waited on.
        if !matches!(
            self.done.recv_timeout(JOIN_WAIT),
            Err(flume::RecvTimeoutError::Disconnected)
        ) {
            tracing::warn!("screen-capture: a PipeWire thread did not stop in time; detached");
            return;
        }
        if let Some(join) = self.join.take()
            && join.join().is_err()
        {
            tracing::warn!("screen-capture: a PipeWire thread panicked");
        }
    }
}

fn connect(
    backend: Option<BackendId>,
    remote: Remote,
) -> Result<(pw::main_loop::MainLoopRc, pw::core::CoreRc), CaptureError> {
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| error(backend, "PipeWire main loop", err))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| error(backend, "PipeWire context", err))?;
    let core = match remote {
        Remote::SessionDaemon => context.connect_rc(None),
        Remote::Fd(fd) => context.connect_fd_rc(fd, None),
    }
    .map_err(|err| error(backend, "connect to PipeWire", err))?;
    Ok((mainloop, core))
}

/// Why a stream state change ends the session: an explicit error, or
/// falling back to `Unconnected` after connecting (the source node went
/// away; `DONT_RECONNECT` keeps PipeWire from linking another one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    Error(String),
    Disconnected,
}

impl StreamEnd {
    /// `Some` for a transition that ends the stream.
    pub(crate) fn of(old: &pw::stream::StreamState, new: &pw::stream::StreamState) -> Option<Self> {
        use pw::stream::StreamState;
        match (old, new) {
            (_, StreamState::Error(message)) => Some(Self::Error(message.clone())),
            (
                StreamState::Connecting | StreamState::Paused | StreamState::Streaming,
                StreamState::Unconnected,
            ) => Some(Self::Disconnected),
            _ => None,
        }
    }

    /// The session error it stands for.
    pub(crate) fn into_error(self, backend: Option<BackendId>) -> CaptureError {
        match self {
            Self::Error(message) => error(backend, "PipeWire stream", message),
            Self::Disconnected => CaptureError::SourceLost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pw::stream::StreamState::{Connecting, Error, Paused, Streaming, Unconnected};

    #[test]
    fn unconnected_after_connect_and_errors_are_fatal() {
        assert_eq!(
            StreamEnd::of(&Streaming, &Unconnected),
            Some(StreamEnd::Disconnected)
        );
        assert_eq!(
            StreamEnd::of(&Paused, &Unconnected),
            Some(StreamEnd::Disconnected)
        );
        assert_eq!(
            StreamEnd::of(&Connecting, &Unconnected),
            Some(StreamEnd::Disconnected)
        );
        assert_eq!(
            StreamEnd::of(&Streaming, &Error("boom".into())),
            Some(StreamEnd::Error("boom".into()))
        );
        assert_eq!(StreamEnd::of(&Unconnected, &Connecting), None);
        assert_eq!(StreamEnd::of(&Streaming, &Paused), None);
        assert_eq!(StreamEnd::of(&Paused, &Streaming), None);
        assert_eq!(
            StreamEnd::Disconnected.into_error(Some(BackendId::LinuxPortal)),
            CaptureError::SourceLost
        );
    }
}
