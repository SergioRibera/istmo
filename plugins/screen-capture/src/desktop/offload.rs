//! Runs blocking backend work on the plugin's own threads, so every
//! host method stays a future that completes on any executor (hosts
//! run inline on the caller unless `#[istmo::blocking]`).

use crate::types::{BackendDetail, CaptureError};

/// Run `work` on a new named thread and await its result.
///
/// # Errors
/// The work's own error, `Protocol` when the thread could not start or
/// panicked.
pub(crate) async fn offload<T, F>(what: &'static str, work: F) -> Result<T, CaptureError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CaptureError> + Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name(format!("istmo-screen-capture-{what}"))
        .spawn(move || {
            // The caller may have stopped waiting; the result is then
            // dropped here.
            let _unwanted = tx.send(work());
        })
        .map_err(|err| internal(what, err.to_string()))?;
    rx.await
        .map_err(|_| internal(what, "the worker thread panicked".to_owned()))?
}

fn internal(step: &str, message: String) -> CaptureError {
    CaptureError::Protocol(BackendDetail {
        backend: None,
        step: step.to_owned(),
        message,
    })
}
