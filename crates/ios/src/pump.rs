//! Outbound frame pump.
//!
//! Drains the runtime's outbound channel and decomposes each [`Frame`] into
//! a typed callback invocation against the [`super::IstmoIosCallbacks`]
//! table registered at [`super::istmo_ios_start`].
//!
//! Every callback is invoked on this dedicated OS thread — never on the
//! caller's context. The Swift side is expected to hop onto the main queue
//! (via `DispatchQueue.main.async`) inside the callback body when the work
//! must run on the main thread; the transport itself makes no such promise.
//!
//! Payload buffers passed to callbacks are borrowed for the duration of the
//! call. Swift copies out into `Data` or `Array<UInt8>` synchronously —
//! nothing crosses `await` boundaries with a raw pointer.

use std::thread::{self, JoinHandle};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, unbounded};
use istmo_core::{Envelope, Frame};

use crate::cdecl_exports::IstmoIosCallbacks;

pub(crate) struct PumpHandles {
    pub(crate) shutdown: FlumeSender<()>,
    pub(crate) join: JoinHandle<()>,
}

pub(crate) fn spawn(
    callbacks: IstmoIosCallbacks,
    outbound: FlumeReceiver<Envelope>,
) -> PumpHandles {
    let (shutdown_tx, shutdown_rx) = unbounded::<()>();
    let join = thread::Builder::new()
        .name("istmo-ios-pump".to_owned())
        .spawn(move || pump_loop(callbacks, &outbound, &shutdown_rx))
        .expect("spawn istmo ios pump thread");
    PumpHandles {
        shutdown: shutdown_tx,
        join,
    }
}

fn pump_loop(
    callbacks: IstmoIosCallbacks,
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
        deliver(&callbacks, envelope.frame);
    }
    tracing::info!("istmo ios pump thread exiting");
}

enum PumpStep {
    Deliver(Envelope),
    Shutdown,
}

#[allow(clippy::cast_possible_wrap)]
fn deliver(callbacks: &IstmoIosCallbacks, frame: Frame) {
    match frame {
        Frame::Call {
            call_id,
            plugin_id,
            instance_id,
            method,
            payload,
        } => {
            // SAFETY: pointers are non-null when they come from Swift-registered
            // callbacks; lengths match the borrowed slices for the duration of
            // the call. Swift copies out synchronously.
            unsafe {
                (callbacks.on_call)(
                    callbacks.ctx,
                    call_id.get(),
                    plugin_id.as_ptr(),
                    plugin_id.len(),
                    instance_id.map_or(0, istmo_core::InstanceId::get),
                    method.as_ptr(),
                    method.len(),
                    payload.as_ptr(),
                    payload.len(),
                );
            }
        }
        Frame::Cancel { call_id } => unsafe {
            (callbacks.on_cancel)(callbacks.ctx, call_id.get());
        },
        Frame::CreateInstance {
            call_id,
            plugin_id,
            payload,
        } => unsafe {
            (callbacks.on_create_instance)(
                callbacks.ctx,
                call_id.get(),
                plugin_id.as_ptr(),
                plugin_id.len(),
                payload.as_ptr(),
                payload.len(),
            );
        },
        Frame::DestroyInstance { instance_id } => unsafe {
            (callbacks.on_destroy_instance)(callbacks.ctx, instance_id.get());
        },
        Frame::Respond { call_id, result } => {
            let (ok, payload) = match result {
                Ok(bytes) => (true, bytes),
                Err(bytes) => (false, bytes),
            };
            unsafe {
                (callbacks.on_respond)(
                    callbacks.ctx,
                    call_id.get(),
                    ok,
                    payload.as_ptr(),
                    payload.len(),
                );
            }
        }
        Frame::Event { stream_id, payload } => unsafe {
            (callbacks.on_event)(
                callbacks.ctx,
                stream_id.get(),
                payload.as_ptr(),
                payload.len(),
            );
        },
        Frame::StreamEnd { stream_id, reason } => {
            let (tag, payload) = match reason {
                istmo_core::StreamEndReason::Complete => (0u32, Vec::new()),
                istmo_core::StreamEndReason::Cancelled => (1, Vec::new()),
                istmo_core::StreamEndReason::Error(bytes) => (2, bytes),
            };
            unsafe {
                (callbacks.on_stream_end)(
                    callbacks.ctx,
                    stream_id.get(),
                    tag,
                    payload.as_ptr(),
                    payload.len(),
                );
            }
        }
    }
}
