//! Outbound pump: one OS thread turning runtime envelopes into calls on
//! the Swift callback table.
//!
//! Every callback below is invoked under the same contract, accepted by
//! the Swift side in `istmo_ios_start`: the function pointer is callable
//! from this thread, `ctx` is still valid (Swift keeps it alive until
//! `istmo_ios_shutdown` joins this thread), and each `(ptr, len)` pair
//! points into a buffer owned by the frame being delivered, which
//! outlives the call.

use std::thread::{self, JoinHandle};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, unbounded};
use istmo_core::{Envelope, Frame, InstanceId, StreamEndReason};

use crate::cdecl_exports::Callbacks;
use crate::error::IosRuntimeError;

pub(crate) struct PumpHandles {
    pub(crate) shutdown: FlumeSender<()>,
    pub(crate) join: JoinHandle<()>,
}

pub(crate) fn spawn(
    callbacks: Callbacks,
    outbound: FlumeReceiver<Envelope>,
) -> Result<PumpHandles, IosRuntimeError> {
    let (shutdown_tx, shutdown_rx) = unbounded::<()>();
    let join = thread::Builder::new()
        .name("istmo-ios-pump".to_owned())
        .spawn(move || pump_loop(&callbacks, &outbound, &shutdown_rx))
        .map_err(IosRuntimeError::SpawnPump)?;
    Ok(PumpHandles {
        shutdown: shutdown_tx,
        join,
    })
}

fn pump_loop(
    callbacks: &Callbacks,
    outbound: &FlumeReceiver<Envelope>,
    shutdown: &FlumeReceiver<()>,
) {
    loop {
        let step = flume::Selector::new()
            .recv(shutdown, |_| PumpStep::Shutdown)
            .recv(outbound, |envelope| {
                envelope.map_or(PumpStep::Shutdown, PumpStep::Deliver)
            })
            .wait();

        let envelope = match step {
            PumpStep::Shutdown => break,
            PumpStep::Deliver(env) => env,
        };
        callbacks.deliver(envelope.frame);
    }
    tracing::info!("istmo ios pump thread exiting");
}

enum PumpStep {
    Deliver(Envelope),
    Shutdown,
}

impl Callbacks {
    #[expect(
        clippy::too_many_lines,
        reason = "one flat arm per frame variant reads better than splitting"
    )]
    fn deliver(&self, frame: Frame) {
        let ctx = self.ctx;
        match frame {
            Frame::Call {
                call_id,
                plugin_id,
                instance_id,
                method,
                payload,
            } => {
                // SAFETY: module-level callback contract; the three
                // buffers are owned by this arm and live past the call.
                unsafe {
                    (self.on_call)(
                        ctx,
                        call_id.get(),
                        plugin_id.as_ptr(),
                        plugin_id.len(),
                        instance_id.map_or(0, InstanceId::get),
                        method.as_ptr(),
                        method.len(),
                        payload.as_ptr(),
                        payload.len(),
                    );
                }
            }
            Frame::Cancel { call_id } => {
                // SAFETY: module-level callback contract; no buffers.
                unsafe { (self.on_cancel)(ctx, call_id.get()) };
            }
            Frame::CreateInstance {
                call_id,
                plugin_id,
                payload,
            } => {
                // SAFETY: module-level callback contract; both buffers
                // are owned by this arm and live past the call.
                unsafe {
                    (self.on_create_instance)(
                        ctx,
                        call_id.get(),
                        plugin_id.as_ptr(),
                        plugin_id.len(),
                        payload.as_ptr(),
                        payload.len(),
                    );
                }
            }
            Frame::DestroyInstance { instance_id } => {
                // SAFETY: module-level callback contract; no buffers.
                unsafe { (self.on_destroy_instance)(ctx, instance_id.get()) };
            }
            Frame::Respond { call_id, result } => {
                let (ok, payload) = match result {
                    Ok(bytes) => (true, bytes),
                    Err(bytes) => (false, bytes),
                };
                // SAFETY: module-level callback contract; `payload` is
                // owned by this arm and lives past the call.
                unsafe {
                    (self.on_respond)(ctx, call_id.get(), ok, payload.as_ptr(), payload.len());
                }
            }
            Frame::Event { stream_id, payload } => {
                // SAFETY: module-level callback contract; `payload` is
                // owned by this arm and lives past the call.
                unsafe {
                    (self.on_event)(ctx, stream_id.get(), payload.as_ptr(), payload.len());
                }
            }
            Frame::StreamEnd { stream_id, reason } => {
                let (tag, payload) = match reason {
                    StreamEndReason::Complete => (0u32, Vec::new()),
                    StreamEndReason::Cancelled => (1, Vec::new()),
                    StreamEndReason::Error(bytes) => (2, bytes),
                    // Swift only knows the three tags above: end anything newer
                    // as an error so the consumer still sees the stream close.
                    other => {
                        tracing::warn!(?other, "unmapped stream end reason; reported as error");
                        (2, Vec::new())
                    }
                };
                // SAFETY: module-level callback contract; `payload` is
                // owned by this arm and lives past the call.
                unsafe {
                    (self.on_stream_end)(
                        ctx,
                        stream_id.get(),
                        tag,
                        payload.as_ptr(),
                        payload.len(),
                    );
                }
            }
            Frame::EarlyEvent { channel, .. } => {
                tracing::warn!(
                    channel = %channel,
                    "dropped outbound EarlyEvent frame; variant is inbound-only",
                );
            }
            Frame::ReleaseNativeHandle { handle_id } => {
                // SAFETY: module-level callback contract; no buffers.
                unsafe { (self.on_release_native_handle)(ctx, handle_id.get()) };
            }
            Frame::Notify {
                plugin_id,
                instance_id,
                method,
                payload,
            } => {
                // SAFETY: module-level callback contract; the three
                // buffers are owned by this arm and live past the call.
                unsafe {
                    (self.on_notify)(
                        ctx,
                        plugin_id.as_ptr(),
                        plugin_id.len(),
                        instance_id.map_or(0, InstanceId::get),
                        method.as_ptr(),
                        method.len(),
                        payload.as_ptr(),
                        payload.len(),
                    );
                }
            }
        }
    }
}
